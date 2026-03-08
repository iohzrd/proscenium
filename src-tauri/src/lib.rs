mod commands;
mod constants;
mod crypto;
mod dm;
mod ext;
mod gossip;
mod peer;
mod push;
mod setup;
mod state;
mod storage;
mod sync;

use commands::*;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let mut builder = tauri::Builder::default();

    #[cfg(not(mobile))]
    {
        use tauri::Emitter;
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            for arg in &argv {
                if arg.starts_with("iroh-social://") {
                    let _ = app.emit("deep-link-received", vec![arg.clone()]);
                    break;
                }
            }
        }));
    }

    #[cfg(mobile)]
    {
        builder = builder
            .plugin(tauri_plugin_barcode_scanner::init())
            .plugin(tauri_plugin_haptics::init());
    }

    builder
        .plugin(
            tauri_plugin_log::Builder::new()
                .level(log::LevelFilter::Info)
                .targets([
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Stdout),
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::LogDir {
                        file_name: None,
                    }),
                ])
                .build(),
        )
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_os::init())
        .setup(|app| setup::initialize(app))
        .invoke_handler(tauri::generate_handler![
            get_node_id,
            get_my_profile,
            save_my_profile,
            get_remote_profile,
            create_post,
            delete_post,
            get_feed,
            get_notifications,
            get_unread_notification_count,
            mark_notifications_read,
            get_user_posts,
            sync_posts,
            get_sync_status,
            fetch_older_posts,
            like_post,
            unlike_post,
            repost,
            unrepost,
            get_post_counts,
            get_replies,
            get_post,
            follow_user,
            unfollow_user,
            update_follow_alias,
            get_follows,
            get_followers,
            add_blob,
            fetch_blob,
            add_blob_bytes,
            fetch_blob_bytes,
            get_node_status,
            send_dm,
            get_conversations,
            get_dm_messages,
            mark_dm_read,
            delete_dm_message,
            flush_dm_outbox,
            get_unread_dm_count,
            send_dm_signal,
            toggle_bookmark,
            is_bookmarked,
            mute_user,
            unmute_user,
            is_muted,
            get_muted_pubkeys,
            block_user,
            unblock_user,
            is_blocked,
            get_blocked_pubkeys,
            get_follow_requests,
            get_pending_follow_request_count,
            approve_follow_request,
            deny_follow_request,
            send_follow_request_to_peer,
            add_server,
            remove_server,
            list_servers,
            refresh_server_info,
            register_with_server,
            unregister_from_server,
            server_get_feed,
            server_get_trending,
            server_search_users,
            server_search_posts,
            server_list_users,
            sync_profile_to_server,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
