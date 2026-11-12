use crate::audio::{MicConfig, PCM_CHANNEL_CAPACITY, spawn_mic_thread};
use crate::error::AppError;
use crate::stage::StageActor;
use crate::stage::command::{ActorSender, StageCommand};
use crate::stage::control::{self, ControlPlane, ControlSigner, VerifiedControl};
use crate::stage::fanout::Fanout;
use crate::stage::mixer::spawn_mixer;
use crate::stage::pipeline::{self, ListenerConfig, SpeakerConfig};
use crate::stage::speaker_mixer::spawn_speaker_mixer;
use crate::stage::state::{
    ActiveStage, HostState, KickList, LISTENER_CAPACITY, LISTENER_CAPACITY_RESERVE,
    MixedStreamServer, Participant, RoomRoles, SenderTable, SpeakerSession, StageRuntime,
};
use crate::stage::topology::TopologyManager;
use iroh::EndpointId;
use proscenium_types::{
    MAX_STAGE_TITLE_CHARS, StageControl, StageEvent, StageRole, StageTicket, now_millis, short_id,
};
use std::collections::{HashMap, HashSet};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64},
};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Interval of the host's `HostTick` command.
const HOST_TICK_INTERVAL: Duration = Duration::from_secs(1);
/// Interval at which the host repeats the stage announcement on its feed topic.
const ANNOUNCE_INTERVAL: Duration = Duration::from_secs(30);

/// Send `command()` to the actor every `period` until `cancel` is cancelled or the
/// actor is gone. The first command is sent after one period.
fn spawn_ticker(
    cmd_tx: ActorSender,
    cancel: CancellationToken,
    period: Duration,
    command: fn() -> StageCommand,
) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(period);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        ticker.tick().await; // the first tick completes at once
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = ticker.tick() => {
                    if !cmd_tx.send(command()).await {
                        break;
                    }
                }
            }
        }
    });
}

/// Forward verified control messages from the gossip receive task to the actor.
fn spawn_control_forwarder(cmd_tx: ActorSender, mut ctrl_rx: mpsc::Receiver<VerifiedControl>) {
    tokio::spawn(async move {
        while let Some(msg) = ctrl_rx.recv().await {
            if !cmd_tx.send(StageCommand::ControlReceived(msg)).await {
                break;
            }
        }
    });
}

impl StageActor {
    /// Identity values a stage session needs: master pubkey and a `ControlSigner`
    /// that signs with the master key.
    async fn session_identity(&self, stage_id: &str) -> (String, Arc<ControlSigner>) {
        let (my_pubkey, master_key) = {
            let id = self.identity.read().await;
            (id.master_pubkey.clone(), id.master_key.clone())
        };
        let node_id = self.endpoint.id().to_string();
        let signer = Arc::new(ControlSigner::new(
            stage_id.to_string(),
            master_key,
            node_id,
        ));
        (my_pubkey, signer)
    }

    fn runtime(&self) -> StageRuntime {
        StageRuntime {
            endpoint: self.endpoint.clone(),
            app_handle: self.app_handle.clone(),
            cmd_tx: self.cmd_tx(),
        }
    }

    pub(super) async fn create_stage(&mut self, title: String) -> Result<StageTicket, AppError> {
        if self.active.is_some() {
            return Err(AppError::Other("already in a stage".into()));
        }
        let title = title.trim().to_string();
        if title.is_empty() || title.chars().count() > MAX_STAGE_TITLE_CHARS {
            return Err(AppError::Other(format!(
                "stage title must have 1 to {MAX_STAGE_TITLE_CHARS} characters"
            )));
        }

        let stage_id = crate::util::generate_id();
        let (my_pubkey, signer) = self.session_identity(&stage_id).await;
        let node_id = self.endpoint.id().to_string();

        let ticket = StageTicket {
            stage_id: stage_id.clone(),
            host_pubkey: my_pubkey.clone(),
            host_node_id: node_id.clone(),
            title: title.clone(),
        };

        // Cancels the tasks started below if a later step fails.
        let cancel = CancellationToken::new();
        let setup_guard = cancel.clone().drop_guard();
        let (ctrl_tx, ctrl_rx) = mpsc::channel::<VerifiedControl>(64);

        let control_plane = ControlPlane::start(
            &self.gossip,
            &stage_id,
            vec![],
            signer.clone(),
            ctrl_tx,
            cancel.child_token(),
        )
        .await?;

        // Host SFU fanout: carries host's own voice to all connected speakers.
        // Created before the mixer so it can be passed in at spawn time.
        let host_sfu_fanout = Arc::new(Fanout::new());

        // Start the host audio mixer (owns encoders, auth state, per-speaker PCM buffers)
        let (mixer, fanout) = spawn_mixer(
            signer.master_key().clone(),
            stage_id.clone(),
            host_sfu_fanout.clone(),
            cancel.child_token(),
        )?;

        // Host microphone: capture -> AEC -> mixer input. The mixer returns the mix
        // without the host's own voice, which is played locally and is the AEC reference.
        let self_muted = Arc::new(AtomicBool::new(false));
        let host_input_id = self.next_conn_id();
        let (mic_tx, mic_rx) = mpsc::channel::<Vec<f32>>(PCM_CHANNEL_CAPACITY);
        let (far_end_tx, far_end_rx) = mpsc::channel::<Vec<f32>>(20);
        let (host_pcm_tx, host_pcm_rx) = mpsc::channel::<Vec<f32>>(PCM_CHANNEL_CAPACITY);
        mixer
            .add_speaker(host_input_id, my_pubkey.clone(), mic_rx)
            .await?;
        mixer.set_host_speaker(host_input_id, host_pcm_tx).await?;
        tokio::spawn(pipeline::run_host_playback(
            host_pcm_rx,
            far_end_tx,
            cancel.child_token(),
        ));
        let mic_started = spawn_mic_thread(MicConfig {
            device: None,
            shared_capture: Arc::new(Mutex::new(None)),
            muted: self_muted.clone(),
            far_end_rx,
            out_tx: mic_tx,
            cancel: cancel.child_token(),
            label: "[stage-host]",
        });
        tokio::spawn(async move {
            match mic_started.await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => log::warn!("[stage-host] host mic capture failed: {e}"),
                Err(_) => log::warn!("[stage-host] host microphone thread did not start"),
            }
        });

        let now = now_millis();
        let mut participants = HashMap::new();
        participants.insert(my_pubkey.clone(), Participant::new(my_pubkey.clone(), now));
        let mut senders = SenderTable::default();
        senders.admit(&my_pubkey, |_| false);

        self.active = Some(ActiveStage {
            rt: self.runtime(),
            stage_id: stage_id.clone(),
            title: title.clone(),
            my_pubkey: my_pubkey.clone(),
            my_node_id: node_id.clone(),
            my_role: StageRole::Host,
            roles: RoomRoles::new(my_pubkey.clone()),
            host_key: signer.master_key().public(),
            host_node_id: node_id.clone(),
            self_muted,
            hand_raised: false,
            started_at: now,
            participants,
            senders,
            kicked: KickList::default(),
            signer,
            control_plane,
            cancel: cancel.clone(),
            host: Some(HostState {
                mixer,
                host_sfu_fanout,
                speaker_conns: HashMap::new(),
                topology: TopologyManager::new(node_id, LISTENER_CAPACITY),
                relay_owners: HashMap::new(),
                ticket: ticket.clone(),
                room_state_pending: true,
                last_room_state_ms: now,
            }),
            serving: Some(MixedStreamServer::new(
                fanout,
                LISTENER_CAPACITY + LISTENER_CAPACITY_RESERVE,
                LISTENER_CAPACITY_RESERVE,
            )),
            speaker: None,
            listener_upstream_id: None,
            listener_pipeline_cancel: None,
            failed_upstreams: HashSet::new(),
            checkpoint_floor: Arc::new(AtomicU64::new(0)),
        });
        let _ = setup_guard.disarm();

        spawn_ticker(
            self.cmd_tx(),
            cancel.child_token(),
            control::PRESENCE_EXPIRY,
            || StageCommand::SweepPresence,
        );
        spawn_ticker(
            self.cmd_tx(),
            cancel.child_token(),
            HOST_TICK_INTERVAL,
            || StageCommand::HostTick,
        );
        spawn_control_forwarder(self.cmd_tx(), ctrl_rx);

        // Announce the stage on our user feed so followers can discover and join.
        // Repeated every 30 s so followers who connect after the first
        // announcement still see it.
        {
            let gs = self.gossip_service.clone();
            let ann_stage_id = stage_id.clone();
            let ann_ticket = ticket.clone();
            let ann_cancel = cancel.child_token();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(ANNOUNCE_INTERVAL);
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    tokio::select! {
                        _ = ann_cancel.cancelled() => break,
                        _ = interval.tick() => {
                            if let Err(e) = gs
                                .broadcast_stage_announcement(
                                    ann_stage_id.clone(),
                                    title.clone(),
                                    ann_ticket.clone(),
                                    my_pubkey.clone(),
                                    now,
                                )
                                .await
                            {
                                log::warn!("[stage] failed to broadcast stage announcement: {e}");
                            }
                        }
                    }
                }
            });
        }

        log::info!("[stage] created stage {}", short_id(&stage_id));
        if let Some(stage) = self.active.as_ref() {
            stage.emit_snapshot();
        }
        Ok(ticket)
    }

    pub(super) async fn join_stage(&mut self, ticket: StageTicket) -> Result<(), AppError> {
        if self.active.is_some() {
            return Err(AppError::Other("already in a stage".into()));
        }

        // Checks the ticket fields; the returned key is the host's master key, which signs
        // the host's control messages and audio checkpoints.
        let host_key = ticket.verify().map_err(AppError::Other)?;
        let stage_id = ticket.stage_id.clone();
        let (my_pubkey, signer) = self.session_identity(&stage_id).await;
        let my_node_id = self.endpoint.id().to_string();
        if ticket.host_pubkey == my_pubkey || ticket.host_node_id == my_node_id {
            return Err(AppError::Other(
                "this identity hosts the stage; it cannot join as a participant".into(),
            ));
        }
        let host_id: EndpointId = ticket
            .host_node_id
            .parse()
            .map_err(|e| AppError::Other(format!("invalid host node id: {e}")))?;

        let cancel = CancellationToken::new();
        let setup_guard = cancel.clone().drop_guard();
        let (ctrl_tx, ctrl_rx) = mpsc::channel::<VerifiedControl>(64);

        let control_plane = ControlPlane::start(
            &self.gossip,
            &stage_id,
            vec![host_id],
            signer.clone(),
            ctrl_tx,
            cancel.child_token(),
        )
        .await?;

        let now = now_millis();
        // The local user and the host are listed before their first Presence arrives.
        let mut participants = HashMap::new();
        participants.insert(my_pubkey.clone(), Participant::new(my_pubkey.clone(), now));
        participants.insert(
            ticket.host_pubkey.clone(),
            Participant::new(ticket.host_pubkey.clone(), now),
        );
        // The ticket names the host, so host messages are accepted before the host's
        // Presence arrives.
        let mut senders = SenderTable::default();
        senders.admit(&ticket.host_pubkey, |_| false);
        senders.admit(&my_pubkey, |_| false);

        let mut stage = ActiveStage {
            rt: self.runtime(),
            stage_id: stage_id.clone(),
            title: ticket.title.clone(),
            my_pubkey,
            my_node_id,
            my_role: StageRole::Listener,
            roles: RoomRoles::new(ticket.host_pubkey.clone()),
            host_key,
            host_node_id: ticket.host_node_id.clone(),
            self_muted: Arc::new(AtomicBool::new(false)),
            hand_raised: false,
            started_at: now,
            participants,
            senders,
            kicked: KickList::default(),
            signer,
            control_plane,
            cancel: cancel.clone(),
            host: None,
            serving: None,
            speaker: None,
            listener_upstream_id: None,
            listener_pipeline_cancel: None,
            failed_upstreams: HashSet::new(),
            checkpoint_floor: Arc::new(AtomicU64::new(0)),
        };
        // Start the listener audio pipeline (connect to host, receive mixed stream).
        stage.start_listener(ticket.host_node_id.clone());
        stage.emit_snapshot();
        self.active = Some(stage);
        let _ = setup_guard.disarm();

        spawn_ticker(
            self.cmd_tx(),
            cancel.child_token(),
            control::PRESENCE_EXPIRY,
            || StageCommand::SweepPresence,
        );
        spawn_control_forwarder(self.cmd_tx(), ctrl_rx);

        log::info!("[stage] joined stage {}", short_id(&stage_id));
        Ok(())
    }

    /// Leave the active stage. When the host leaves, the stage ends.
    pub(super) async fn leave_stage(&mut self) -> Result<(), AppError> {
        let stage = self
            .active
            .as_ref()
            .ok_or_else(|| AppError::Other("not in a stage".into()))?;
        if stage.host.is_some() {
            return self.end_stage().await;
        }

        let stage_id = stage.stage_id.clone();
        log::info!("[stage] left stage {}", short_id(&stage_id));
        self.close_stage(StageEvent::Ended { stage_id });
        Ok(())
    }

    pub(super) async fn end_stage(&mut self) -> Result<(), AppError> {
        let stage = self
            .active
            .as_ref()
            .ok_or_else(|| AppError::Other("not in a stage".into()))?;
        if stage.host.is_none() {
            return Err(AppError::Other("only the host can end the stage".into()));
        }

        // Broadcast End while the control plane is still running.
        stage.broadcast(StageControl::End).await;
        let stage_id = stage.stage_id.clone();
        self.close_stage(StageEvent::Ended {
            stage_id: stage_id.clone(),
        });

        // Broadcast the stage-ended message on our user feed topic.
        let gs = self.gossip_service.clone();
        let ended_stage_id = stage_id.clone();
        tokio::spawn(async move {
            if let Err(e) = gs.broadcast_stage_ended(ended_stage_id).await {
                log::warn!("[stage] failed to broadcast stage ended: {e}");
            }
        });

        log::info!("[stage] ended stage {}", short_id(&stage_id));
        Ok(())
    }
}

impl ActiveStage {
    /// Start the listener pipeline at `upstream_node_id`, replacing a running one.
    pub(super) fn start_listener(&mut self, upstream_node_id: String) {
        self.stop_listener();
        let cancel = self.cancel.child_token();
        self.listener_pipeline_cancel = Some(cancel.clone());
        self.listener_upstream_id = Some(upstream_node_id.clone());
        tokio::spawn(pipeline::start_listener_pipeline(ListenerConfig {
            endpoint: self.rt.endpoint.clone(),
            stage_id: self.stage_id.clone(),
            upstream_node_id,
            host_node_id: self.host_node_id.clone(),
            host_key: self.host_key,
            signer: self.signer.clone(),
            checkpoint_floor: self.checkpoint_floor.clone(),
            app_handle: self.rt.app_handle.clone(),
            cmd_tx: self.rt.cmd_tx.clone(),
            cancel,
        }));
    }

    pub(super) fn stop_listener(&mut self) {
        if let Some(cancel) = self.listener_pipeline_cancel.take() {
            cancel.cancel();
        }
        self.listener_upstream_id = None;
    }

    /// Start the speaker pipeline: a playback mixer for the other speakers' streams and
    /// a connection to the host that carries the microphone. No effect if it is running.
    pub(super) fn start_speaker(&mut self) {
        if self.speaker.is_some() {
            return;
        }
        let cancel = self.cancel.child_token();
        let mixer = spawn_speaker_mixer(cancel.clone());
        tokio::spawn(pipeline::start_speaker_pipeline(SpeakerConfig {
            endpoint: self.rt.endpoint.clone(),
            app_handle: self.rt.app_handle.clone(),
            host_node_id: self.host_node_id.clone(),
            signer: self.signer.clone(),
            speaker_mixer: mixer,
            muted: self.self_muted.clone(),
            cancel: cancel.clone(),
        }));
        self.speaker = Some(SpeakerSession { cancel });
    }

    /// Stop the speaker pipeline, the microphone thread and the speaker mixer.
    pub(super) fn stop_speaker(&mut self) {
        if let Some(session) = self.speaker.take() {
            session.cancel.cancel();
        }
    }
}
