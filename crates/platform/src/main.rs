//! platform binary: `serve`, `add-hub`, `remove-hub`, `poll`, `hubs`, `healthcheck`.
//! The registry commands open the same `platform.db` as the server, so hubctl runs
//! them through `kubectl exec deploy/platform -- platform …` without an admin port.

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, bail};
use clap::{Args, Parser, Subcommand};
use platform::{AppState, Config, Db, HubRegistration, PollMode, PollReport, build_router, health};

#[derive(Parser, Debug)]
#[command(
    name = "platform",
    version,
    about = "Apex platform for Chef's Prodigy hubs: registry, federation poller, search"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Args, Debug)]
struct DbArgs {
    /// Directory for platform.db
    #[arg(long, default_value = "data", env = "PLATFORM_DATA_DIR")]
    data_dir: PathBuf,
    /// Path to the SQLite database file (defaults to <data-dir>/platform.db)
    #[arg(long, env = "PLATFORM_DB_PATH")]
    db: Option<PathBuf>,
}

impl DbArgs {
    fn path(&self) -> PathBuf {
        self.db
            .clone()
            .unwrap_or_else(|| self.data_dir.join("platform.db"))
    }
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run the API and the poller (PLATFORM_ADMIN_TOKEN and PLATFORM_POLL_* from the environment)
    Serve {
        /// TCP port to listen on
        #[arg(long, default_value_t = 8080, env = "PLATFORM_PORT")]
        port: u16,
        /// Interface to bind
        #[arg(long, default_value = "0.0.0.0", env = "PLATFORM_HOST")]
        host: String,
        #[command(flatten)]
        db: DbArgs,
    },
    /// Register a hub, or update it when it exists (idempotent)
    AddHub {
        /// Hub name (subdomain label)
        name: String,
        /// Address the poller uses (default: http://hub-<name>.chefs-hubs.svc:8080)
        #[arg(long)]
        internal_url: Option<String>,
        /// Public base URL used in search hits, e.g. https://jacob.chefslegacy.com
        #[arg(long)]
        public_url: String,
        /// Chef name for the directory (default: the hub's manifest)
        #[arg(long)]
        chef: Option<String>,
        /// Print the hub as JSON with a `created` flag (for hubctl)
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        db: DbArgs,
    },
    /// Remove a hub and everything indexed from it
    RemoveHub {
        /// Hub name
        name: String,
        #[command(flatten)]
        db: DbArgs,
    },
    /// Poll now: every active hub, or one (backoff ignored); exit 1 when a hub failed
    Poll {
        /// Only this hub
        #[arg(long)]
        hub: Option<String>,
        /// Print the reports as JSON
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        db: DbArgs,
    },
    /// List registered hubs with their poll status
    Hubs {
        /// Print JSON
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        db: DbArgs,
    },
    /// Probe a running platform on loopback; exit 0 when healthy, 1 otherwise
    Healthcheck {
        /// TCP port the platform listens on
        #[arg(long, default_value_t = 8080, env = "PLATFORM_PORT")]
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
        Command::Serve { port, host, db } => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| "info,tower_http=info".into()),
                )
                .init();
            serve(host, port, db.path()).await
        }
        Command::AddHub {
            name,
            internal_url,
            public_url,
            chef,
            json,
            db,
        } => {
            let db = open_db(db.path()).await?;
            let (hub, created) = db
                .upsert_hub(&HubRegistration {
                    name,
                    internal_url,
                    public_url,
                    chef_name: chef,
                })
                .await?;
            if json {
                let mut value = serde_json::to_value(&hub)?;
                value["created"] = serde_json::Value::Bool(created);
                println!("{}", serde_json::to_string(&value)?);
            } else {
                println!(
                    "hub {} {} (polled at {}, linked as {})",
                    hub.name,
                    if created { "registered" } else { "updated" },
                    hub.internal_url,
                    hub.public_url
                );
            }
            Ok(())
        }
        Command::RemoveHub { name, db } => {
            let db = open_db(db.path()).await?;
            let name = name.trim();
            if db.remove_hub(name).await? {
                println!("hub {name} removed from the platform with its index rows");
            } else {
                println!("hub {name} is not registered");
            }
            Ok(())
        }
        Command::Poll { hub, json, db } => {
            let db = open_db(db.path()).await?;
            let state = AppState::new(db, Config::from_env())?;
            let reports = match hub.as_deref() {
                Some(name) => vec![state.poller().poll_named(name.trim()).await?],
                None => state.poller().poll(PollMode::Force).await?,
            };
            print_reports(&reports, json)?;
            let failed = reports.iter().filter(|r| !r.ok).count();
            if failed > 0 {
                bail!("{failed} of {} hub(s) failed", reports.len());
            }
            Ok(())
        }
        Command::Hubs { json, db } => {
            let db = open_db(db.path()).await?;
            let hubs = db.list_hubs().await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&hubs)?);
                return Ok(());
            }
            for h in &hubs {
                println!(
                    "{:<24} {:<8} {:>5} recipes  last ok {:<20}  {}",
                    h.name,
                    h.status,
                    h.recipe_count,
                    h.last_ok_at.as_deref().unwrap_or("never"),
                    h.last_error.as_deref().unwrap_or("")
                );
            }
            Ok(())
        }
        Command::Healthcheck { port, host } => {
            health::check(&host, port, health::DEFAULT_TIMEOUT).await
        }
    }
}

fn print_reports(reports: &[PollReport], json: bool) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(reports)?);
        return Ok(());
    }
    if reports.is_empty() {
        println!("no active hubs to poll");
    }
    for r in reports {
        match &r.error {
            None => println!(
                "{}: ok, {} fetched, {} unchanged, {} removed",
                r.hub, r.fetched, r.unchanged, r.removed
            ),
            Some(e) => println!(
                "{}: error: {e} ({} fetched, {} unchanged, {} removed)",
                r.hub, r.fetched, r.unchanged, r.removed
            ),
        }
    }
    Ok(())
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
    match config.admin_token {
        Some(_) => tracing::info!("admin API enabled at /admin (bearer protected)"),
        None => tracing::warn!(
            "PLATFORM_ADMIN_TOKEN not set: admin API disabled; use `platform add-hub` and `platform poll`"
        ),
    }
    tracing::info!(
        interval_secs = config.poll_interval.as_secs(),
        jitter_secs = config.poll_jitter.as_secs(),
        "poller configured"
    );
    let db = open_db(db_path).await?;
    let state = AppState::new(db, config)?;
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let poller = tokio::spawn(state.poller().clone().run(stopped));
    let app = build_router(state);
    let addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .context("invalid host/port")?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    tracing::info!(%addr, "platform listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            let _ = stop.send(true);
        })
        .await?;
    poller.await.context("poller task")?;
    Ok(())
}

/// Ctrl-C locally, SIGTERM from Kubernetes: the platform runs as PID 1 in its pod,
/// where an unhandled SIGTERM is ignored and the pod would wait out its grace period.
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
