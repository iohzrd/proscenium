mod api;
#[cfg(test)]
mod api_tests;
mod config;
mod content;
mod ingestion;
mod node;
#[cfg(test)]
mod peer_tests;
mod storage;
mod sync;
#[cfg(test)]
mod test_util;
mod trending;
mod web;

use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Interval between two runs of `run_maintenance`.
const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(3600);
/// Time an inactive registration row (its replay and downgrade floors) is kept after
/// deactivation.
const INACTIVE_REGISTRATION_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);

#[derive(Parser)]
#[command(name = "proscenium-server", about = "Proscenium discovery server")]
struct Cli {
    /// Config file path
    #[arg(short, long, default_value = "config.toml")]
    config: PathBuf,

    /// Override data directory
    #[arg(long)]
    data_dir: Option<PathBuf>,

    /// Override the port of server.listen_addr
    #[arg(long)]
    port: Option<u16>,
}

/// Completes on SIGINT or SIGTERM.
async fn shutdown_signal() {
    let interrupt = async {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = interrupt => {}
        _ = terminate => {}
    }
}

/// Delete posts and interactions older than `retention_days` (if not 0), deactivate
/// registrations inactive for `registration_expiry_days` (if not 0), and delete inactive
/// registration rows older than `INACTIVE_REGISTRATION_TTL`.
async fn run_maintenance(
    storage: &storage::Storage,
    retention_days: u64,
    registration_expiry_days: u64,
) {
    let now = proscenium_types::now_millis();
    let cutoff = |age_ms: u64| i64::try_from(now.saturating_sub(age_ms)).unwrap_or(i64::MAX);
    let days_ms = |days: u64| days.saturating_mul(24 * 60 * 60 * 1000);

    if retention_days > 0 {
        match storage
            .prune_older_than(cutoff(days_ms(retention_days)))
            .await
        {
            Ok((0, 0)) => {}
            Ok((posts, interactions)) => tracing::info!(
                "pruned {posts} posts and {interactions} interactions older than {retention_days} days"
            ),
            Err(e) => tracing::error!("retention pruning failed: {e}"),
        }
    }

    if registration_expiry_days > 0 {
        let now_ms = i64::try_from(now).unwrap_or(i64::MAX);
        match storage
            .expire_inactive_registrations(cutoff(days_ms(registration_expiry_days)), now_ms)
            .await
        {
            Ok(expired) if expired.is_empty() => {}
            Ok(expired) => tracing::info!(
                "deactivated {} registrations without activity for {registration_expiry_days} days",
                expired.len()
            ),
            Err(e) => tracing::error!("expiry of inactive registrations failed: {e}"),
        }
    }

    let ttl_ms = u64::try_from(INACTIVE_REGISTRATION_TTL.as_millis()).unwrap_or(u64::MAX);
    match storage.prune_inactive_registrations(cutoff(ttl_ms)).await {
        Ok(0) => {}
        Ok(rows) => tracing::info!("deleted {rows} inactive registration rows"),
        Err(e) => tracing::error!("pruning of inactive registrations failed: {e}"),
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();

    // The file is required: registration requests are verified against
    // `server.public_url`, which has no usable default.
    if !cli.config.exists() {
        anyhow::bail!(
            "config file {} not found; a config file that sets server.public_url is required",
            cli.config.display()
        );
    }
    let config = config::Config::load(&cli.config)
        .map_err(|e| anyhow::anyhow!("invalid config file {}: {e}", cli.config.display()))?;
    if config.server.public_url.starts_with("http://") {
        tracing::warn!(
            "server.public_url uses http: signed registration requests are sent unencrypted"
        );
    }

    let data_dir = cli
        .data_dir
        .unwrap_or_else(|| config.server.data_dir.clone());
    std::fs::create_dir_all(&data_dir)?;
    tracing::info!("data directory: {}", data_dir.display());

    let db_path = data_dir.join("server.db");
    let storage = Arc::new(storage::Storage::open(&db_path).await?);
    tracing::info!("database opened at {}", db_path.display());

    let node = node::Node::start(&data_dir).await?;
    tracing::info!("iroh node started");

    let ingestion_mgr = ingestion::IngestionManager::new(&node, storage.clone(), &config);
    ingestion_mgr.start(config.sync.startup_sync).await;
    ingestion_mgr.start_periodic_sync(config.sync.interval_minutes);
    tracing::info!("ingestion manager started");

    trending::start_trending_task(
        storage.clone(),
        config.trending.recompute_interval_minutes,
        config.trending.window_hours,
    );
    tracing::info!("trending computation task started");

    let retention_days = config.limits.retention_days;
    let registration_expiry_days = config.limits.registration_expiry_days;
    if retention_days > 0 {
        tracing::info!("retention: {retention_days} days (pruning hourly)");
    } else {
        tracing::info!("retention: unlimited");
    }
    let maintenance_storage = storage.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(MAINTENANCE_INTERVAL).await;
            run_maintenance(
                &maintenance_storage,
                retention_days,
                registration_expiry_days,
            )
            .await;
        }
    });

    let listen_addr = config.listen_addr(cli.port)?;

    let app_state = api::AppState {
        storage,
        config: Arc::new(config),
        ingestion: ingestion_mgr.clone(),
        start_time: std::time::Instant::now(),
    };

    let router = api::build_router(app_state);

    let listener = tokio::net::TcpListener::bind(&listen_addr).await?;
    tracing::info!("HTTP server listening on {listen_addr}");

    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    tracing::info!("shutting down");
    ingestion_mgr.shutdown().await;
    node.shutdown().await;

    Ok(())
}
