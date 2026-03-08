mod api;
#[allow(dead_code)]
mod config;
mod ingestion;
mod node;
mod storage;
mod trending;
mod web;

use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "iroh-social-server", about = "Iroh Social discovery server")]
struct Cli {
    /// Config file path
    #[arg(short, long, default_value = "config.toml")]
    config: PathBuf,

    /// Override data directory
    #[arg(long)]
    data_dir: Option<PathBuf>,

    /// Override listen port
    #[arg(long)]
    port: Option<u16>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();

    let config = if cli.config.exists() {
        config::Config::load(&cli.config)?
    } else {
        tracing::warn!(
            "config file {} not found, using defaults (you must set server.public_url)",
            cli.config.display()
        );
        toml::from_str(
            r#"
            [server]
            public_url = "http://localhost:3000"
            "#,
        )?
    };

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

    let ingestion_mgr = ingestion::IngestionManager::new(&node, storage.clone());
    ingestion_mgr.start(config.sync.startup_sync).await;
    ingestion_mgr.start_periodic_sync(config.sync.interval_minutes);
    tracing::info!("ingestion manager started");

    trending::start_trending_task(
        storage.clone(),
        config.trending.recompute_interval_minutes,
        config.trending.window_hours,
    );
    tracing::info!("trending computation task started");

    if config.limits.retention_days > 0 {
        let retention_days = config.limits.retention_days;
        let prune_storage = storage.clone();
        tokio::spawn(async move {
            let interval = tokio::time::Duration::from_secs(3600);
            loop {
                tokio::time::sleep(interval).await;
                let cutoff_ms =
                    iroh_social_types::now_millis() - (retention_days * 24 * 60 * 60 * 1000);
                match prune_storage.prune_old_posts(cutoff_ms as i64).await {
                    Ok(0) => {}
                    Ok(n) => tracing::info!("pruned {n} posts older than {retention_days} days"),
                    Err(e) => tracing::error!("post pruning failed: {e}"),
                }
            }
        });
        tracing::info!("post retention: {retention_days} days (pruning hourly)");
    } else {
        tracing::info!("post retention: unlimited");
    }

    let listen_addr = if let Some(port) = cli.port {
        format!("0.0.0.0:{port}")
    } else {
        config.server.listen_addr.clone()
    };

    let app_state = api::AppState {
        storage,
        config: Arc::new(config),
        ingestion: ingestion_mgr,
        start_time: std::time::Instant::now(),
    };

    let router = api::build_router(app_state);

    let listener = tokio::net::TcpListener::bind(&listen_addr).await?;
    tracing::info!("HTTP server listening on {listen_addr}");

    axum::serve(listener, router).await?;

    Ok(())
}
