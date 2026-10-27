//! Standalone Recipe Node entry point.

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};
use node_server::mcp::API_KEY_ENV;
use node_server::state::default_mcp_allowed_hosts_with;
use node_server::{AppState, NodeInfo, build_router};
use recipe_db::Db;
use recipe_scraper::{RecipeImporter, extractor_from_env};
use rmcp::ServiceExt;

#[derive(Parser, Debug)]
#[command(name = "node-server", version, about = "Standalone Recipe Node")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run the HTTP / SSE server with the embedded web UI
    Serve {
        /// TCP port to listen on
        #[arg(long, default_value_t = 8080, env = "NODE_PORT")]
        port: u16,
        /// Interface to bind
        #[arg(long, default_value = "0.0.0.0", env = "NODE_HOST")]
        host: String,
        /// Directory for the database and stored images
        #[arg(long, default_value = "data", env = "NODE_DATA_DIR")]
        data_dir: PathBuf,
        /// Path to the SQLite database file (defaults to <data-dir>/recipes.db)
        #[arg(long, env = "NODE_DB_PATH")]
        db: Option<PathBuf>,
        /// Decentralized identifier of the node owner (federation)
        #[arg(long, env = "NODE_AUTHOR_DID")]
        author_did: Option<String>,
        /// Display name of the chef running this node
        #[arg(long, default_value = "Chef", env = "NODE_CHEF_NAME")]
        chef_name: String,
        /// Bearer token protecting the /mcp endpoint (endpoint disabled when unset)
        #[arg(long, env = API_KEY_ENV, hide_env_values = true)]
        api_key: Option<String>,
        /// Hosts accepted on /mcp (comma separated); defaults to loopback names plus the host of NODE_PUBLIC_URL
        #[arg(long, env = "NODE_MCP_ALLOWED_HOSTS", value_delimiter = ',')]
        mcp_allowed_hosts: Option<Vec<String>>,
        /// Public base URL of this node, e.g. https://jacob.example.com (federation `node_url`)
        #[arg(long, env = "NODE_PUBLIC_URL")]
        public_url: Option<String>,
    },
    /// Detect and store the source language of recipes that lack one
    BackfillLanguage {
        /// Directory for the database and stored images
        #[arg(long, default_value = "data", env = "NODE_DATA_DIR")]
        data_dir: PathBuf,
        /// Path to the SQLite database file (defaults to <data-dir>/recipes.db)
        #[arg(long, env = "NODE_DB_PATH")]
        db: Option<PathBuf>,
    },
    /// Probe a running node on loopback; exit 0 when healthy, 1 otherwise (container HEALTHCHECK)
    Healthcheck {
        /// TCP port the node listens on
        #[arg(long, default_value_t = 8080, env = "NODE_PORT")]
        port: u16,
        /// Address to probe
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
    },
    /// Run the MCP server over stdio for local AI clients
    McpStdio {
        /// Directory for the database and stored images
        #[arg(long, default_value = "data", env = "NODE_DATA_DIR")]
        data_dir: PathBuf,
        /// Path to the SQLite database file (defaults to <data-dir>/recipes.db)
        #[arg(long, env = "NODE_DB_PATH")]
        db: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    // In stdio mode stdout carries the protocol, so logs must go to stderr.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info,tower_http=info".into());
    match cli.command {
        Command::Serve {
            port,
            host,
            data_dir,
            db,
            author_did,
            chef_name,
            api_key,
            mcp_allowed_hosts,
            public_url,
        } => {
            tracing_subscriber::fmt().with_env_filter(filter).init();
            let db = db.unwrap_or_else(|| data_dir.join("recipes.db"));
            serve(ServeOptions {
                host,
                port,
                data_dir,
                db_path: db,
                author_did,
                chef_name,
                api_key,
                mcp_allowed_hosts,
                public_url,
            })
            .await
        }
        Command::BackfillLanguage { data_dir, db } => {
            tracing_subscriber::fmt().with_env_filter(filter).init();
            let db = db.unwrap_or_else(|| data_dir.join("recipes.db"));
            let state = open_state(
                &data_dir,
                &db,
                NodeInfo {
                    author_did: None,
                    chef_name: "Chef".to_string(),
                },
            )
            .await?;
            let report = state.translations().backfill_languages().await?;
            println!("{}", serde_json::to_string(&report)?);
            Ok(())
        }
        Command::Healthcheck { port, host } => {
            node_server::health::check(&host, port, node_server::health::DEFAULT_TIMEOUT).await
        }
        Command::McpStdio { data_dir, db } => {
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_writer(std::io::stderr)
                .init();
            let db = db.unwrap_or_else(|| data_dir.join("recipes.db"));
            mcp_stdio(data_dir, db).await
        }
    }
}

struct ServeOptions {
    host: String,
    port: u16,
    data_dir: PathBuf,
    db_path: PathBuf,
    author_did: Option<String>,
    chef_name: String,
    api_key: Option<String>,
    mcp_allowed_hosts: Option<Vec<String>>,
    public_url: Option<String>,
}

async fn open_state(
    data_dir: &PathBuf,
    db_path: &PathBuf,
    node: NodeInfo,
) -> anyhow::Result<AppState> {
    tokio::fs::create_dir_all(data_dir)
        .await
        .with_context(|| format!("creating {}", data_dir.display()))?;
    if let Some(parent) = db_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let db = Db::open(db_path)
        .await
        .with_context(|| format!("opening database {}", db_path.display()))?;
    Ok(AppState::new(db, node)
        .with_importer(RecipeImporter::new(extractor_from_env()))
        .with_image_dir(data_dir.join("images")))
}

async fn mcp_stdio(data_dir: PathBuf, db_path: PathBuf) -> anyhow::Result<()> {
    let state = open_state(
        &data_dir,
        &db_path,
        NodeInfo {
            author_did: None,
            chef_name: "Chef".to_string(),
        },
    )
    .await?;
    tracing::info!("MCP server ready on stdio");
    let running = state
        .mcp_server()
        .serve(rmcp::transport::stdio())
        .await
        .context("MCP handshake failed")?;
    running
        .waiting()
        .await
        .context("MCP session ended abnormally")?;
    Ok(())
}

async fn serve(opts: ServeOptions) -> anyhow::Result<()> {
    let ServeOptions {
        host,
        port,
        data_dir,
        db_path,
        author_did,
        chef_name,
        api_key,
        mcp_allowed_hosts,
        public_url,
    } = opts;
    let mut state = open_state(
        &data_dir,
        &db_path,
        NodeInfo {
            author_did,
            chef_name,
        },
    )
    .await?;
    match api_key.filter(|k| !k.trim().is_empty()) {
        Some(key) => {
            state = state.with_mcp_api_key(key.trim());
            tracing::info!("MCP endpoint enabled at /mcp (bearer protected)");
        }
        None => tracing::warn!(
            "{API_KEY_ENV} not set: running in single-user mode, every caller on the network is the owner (full read/write API access) and /mcp is disabled"
        ),
    }
    let public_url = public_url
        .map(|u| u.trim().trim_end_matches('/').to_string())
        .filter(|u| !u.is_empty());
    if let Some(url) = &public_url {
        let parsed = url::Url::parse(url)
            .with_context(|| format!("NODE_PUBLIC_URL {url:?} is not a valid URL"))?;
        anyhow::ensure!(
            matches!(parsed.scheme(), "http" | "https") && parsed.host_str().is_some(),
            "NODE_PUBLIC_URL {url:?} must be an absolute http(s) URL"
        );
        state = state.with_public_url(url);
        tracing::info!(node_url = %url, "public URL configured");
    }
    let hosts = mcp_allowed_hosts
        .map(|h| {
            h.into_iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| default_mcp_allowed_hosts_with(public_url.as_deref()));
    state = state.with_mcp_allowed_hosts(hosts);
    // Recipes imported before source languages existed get one in the background.
    let backfill = state.translations();
    tokio::spawn(async move {
        match backfill.backfill_languages().await {
            Ok(r) if r.scanned > 0 => tracing::info!(
                scanned = r.scanned,
                detected = r.detected,
                undetected = r.undetected,
                "language backfill"
            ),
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "language backfill failed"),
        }
    });
    let app = build_router(state);

    let addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .context("invalid host/port")?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    tracing::info!(%addr, "recipe node listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    if let Err(e) = tokio::signal::ctrl_c().await {
        tracing::warn!(error = %e, "failed to listen for ctrl-c");
    }
    tracing::info!("shutdown requested");
}
