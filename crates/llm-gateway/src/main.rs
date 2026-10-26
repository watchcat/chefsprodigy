//! llm-gateway binary: `serve`, `add-hub`, `healthcheck`.

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};
use llm_gateway::{AppState, Config, Db, build_router, health};

#[derive(Parser, Debug)]
#[command(
    name = "llm-gateway",
    version,
    about = "LLM gateway for Chef's Prodigy hubs"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run the gateway (vendor keys and GATEWAY_ADMIN_TOKEN from the environment)
    Serve {
        /// TCP port to listen on
        #[arg(long, default_value_t = 8080, env = "GATEWAY_PORT")]
        port: u16,
        /// Interface to bind
        #[arg(long, default_value = "0.0.0.0", env = "GATEWAY_HOST")]
        host: String,
        /// Directory for gateway.db
        #[arg(long, default_value = "data", env = "GATEWAY_DATA_DIR")]
        data_dir: PathBuf,
        /// Path to the SQLite database file (defaults to <data-dir>/gateway.db)
        #[arg(long, env = "GATEWAY_DB_PATH")]
        db: Option<PathBuf>,
    },
    /// Register a hub and print its token once
    AddHub {
        /// Hub name (subdomain label)
        name: String,
        /// Directory for gateway.db
        #[arg(long, default_value = "data", env = "GATEWAY_DATA_DIR")]
        data_dir: PathBuf,
        /// Path to the SQLite database file (defaults to <data-dir>/gateway.db)
        #[arg(long, env = "GATEWAY_DB_PATH")]
        db: Option<PathBuf>,
    },
    /// Probe a running gateway on loopback; exit 0 when healthy, 1 otherwise (container HEALTHCHECK)
    Healthcheck {
        /// TCP port the gateway listens on
        #[arg(long, default_value_t = 8080, env = "GATEWAY_PORT")]
        port: u16,
        /// Address to probe
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Serve {
            port,
            host,
            data_dir,
            db,
        } => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| "info,tower_http=info".into()),
                )
                .init();
            serve(
                host,
                port,
                db.unwrap_or_else(|| data_dir.join("gateway.db")),
            )
            .await
        }
        Command::AddHub { name, data_dir, db } => {
            let db = open_db(db.unwrap_or_else(|| data_dir.join("gateway.db"))).await?;
            let hub = db.create_hub(name.trim()).await?;
            println!("hub {} registered", hub.name);
            println!(
                "token (shown once, store it in the hub's env as MINIMAX_API_KEY): {}",
                hub.token
            );
            Ok(())
        }
        Command::Healthcheck { port, host } => {
            health::check(&host, port, health::DEFAULT_TIMEOUT).await
        }
    }
}

async fn open_db(path: PathBuf) -> anyhow::Result<Db> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    Db::open(&path)
        .await
        .with_context(|| format!("opening database {}", path.display()))
}

async fn serve(host: String, port: u16, db_path: PathBuf) -> anyhow::Result<()> {
    let config = Config::from_env();
    for vendor in &config.vendors {
        tracing::info!(
            vendor = %vendor.name,
            base = %vendor.base_url,
            configured = vendor.api_key.is_some(),
            "vendor"
        );
    }
    match config.admin_token {
        Some(_) => tracing::info!("admin API enabled at /admin (bearer protected)"),
        None => tracing::warn!(
            "GATEWAY_ADMIN_TOKEN not set: admin API disabled; use `llm-gateway add-hub`"
        ),
    }
    let db = open_db(db_path).await?;
    let app = build_router(AppState::new(db, config));
    let addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .context("invalid host/port")?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    tracing::info!(%addr, "llm-gateway listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            if let Err(e) = tokio::signal::ctrl_c().await {
                tracing::warn!(error = %e, "failed to listen for ctrl-c");
            }
            tracing::info!("shutdown requested");
        })
        .await?;
    Ok(())
}
