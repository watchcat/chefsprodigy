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
        /// Issue a new token when the hub exists (the old one stops working), re-enable it
        /// when it was disabled, or register it when it is unknown
        #[arg(long)]
        rotate: bool,
        /// Print `{"name", "token", "created_at"}` as JSON (for hubctl)
        #[arg(long)]
        json: bool,
        /// Directory for gateway.db
        #[arg(long, default_value = "data", env = "GATEWAY_DATA_DIR")]
        data_dir: PathBuf,
        /// Path to the SQLite database file (defaults to <data-dir>/gateway.db)
        #[arg(long, env = "GATEWAY_DB_PATH")]
        db: Option<PathBuf>,
    },
    /// Disable a hub so its token is refused (usage history is kept)
    DisableHub {
        /// Hub name
        name: String,
        /// Directory for gateway.db
        #[arg(long, default_value = "data", env = "GATEWAY_DATA_DIR")]
        data_dir: PathBuf,
        /// Path to the SQLite database file (defaults to <data-dir>/gateway.db)
        #[arg(long, env = "GATEWAY_DB_PATH")]
        db: Option<PathBuf>,
    },
    /// Print usage rows (requests, tokens, cost per hub and UTC day) as JSON
    Usage {
        /// Only this hub
        #[arg(long)]
        hub: Option<String>,
        /// First day, YYYY-MM-DD (inclusive)
        #[arg(long)]
        from: Option<String>,
        /// Last day, YYYY-MM-DD (inclusive)
        #[arg(long)]
        to: Option<String>,
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
        Command::AddHub {
            name,
            rotate,
            json,
            data_dir,
            db,
        } => {
            let db = open_db(db.unwrap_or_else(|| data_dir.join("gateway.db"))).await?;
            let name = name.trim();
            let hub = if rotate {
                db.rotate_hub(name).await?
            } else {
                db.create_hub(name).await?
            };
            if json {
                println!("{}", serde_json::to_string(&hub)?);
            } else {
                println!("hub {} registered", hub.name);
                println!(
                    "token (shown once, store it in the hub's env as MINIMAX_API_KEY): {}",
                    hub.token
                );
            }
            Ok(())
        }
        Command::DisableHub { name, data_dir, db } => {
            let db = open_db(db.unwrap_or_else(|| data_dir.join("gateway.db"))).await?;
            if db.disable_hub(name.trim()).await? {
                println!("hub {} disabled", name.trim());
            } else {
                println!("hub {} is not registered", name.trim());
            }
            Ok(())
        }
        Command::Usage {
            hub,
            from,
            to,
            data_dir,
            db,
        } => {
            let db = open_db(db.unwrap_or_else(|| data_dir.join("gateway.db"))).await?;
            let rows = db
                .usage(hub.as_deref(), from.as_deref(), to.as_deref())
                .await?;
            println!("{}", serde_json::to_string_pretty(&rows)?);
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
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

/// Ctrl-C locally, SIGTERM from Kubernetes: the gateway runs as PID 1 in its pod, where
/// an unhandled SIGTERM is ignored and the pod would wait out its grace period.
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::warn!(error = %e, "failed to listen for ctrl-c");
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    tracing::info!("shutdown requested");
}
