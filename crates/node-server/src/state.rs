//! Shared application state injected into every handler.

use std::path::PathBuf;
use std::sync::Arc;

use recipe_db::Db;
use recipe_mcp::{ImportService, RecipeMcpServer, TranslationService};
use recipe_scraper::images::ImageStore;
use recipe_scraper::{HeuristicExtractor, RecipeImporter};

/// Static identity of this node, exposed via the federation manifest.
#[derive(Debug, Clone)]
pub struct NodeInfo {
    /// Decentralized identifier of the node owner, if configured.
    pub author_did: Option<String>,
    /// Display name of the chef running this node.
    pub chef_name: String,
}

/// Cheaply cloneable handle passed to handlers via `State`.
#[derive(Clone)]
pub struct AppState {
    inner: Arc<Inner>,
}

struct Inner {
    db: Db,
    node: NodeInfo,
    importer: RecipeImporter,
    images: ImageStore,
    mcp_api_key: Option<String>,
    mcp_allowed_hosts: Vec<String>,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("node", &self.inner.node)
            .field("images", &self.inner.images)
            .field("mcp_enabled", &self.inner.mcp_api_key.is_some())
            .finish()
    }
}

/// Hosts accepted on `/mcp` unless overridden (DNS-rebinding protection).
pub fn default_mcp_allowed_hosts() -> Vec<String> {
    ["localhost", "127.0.0.1", "::1"].map(String::from).to_vec()
}

impl AppState {
    /// State with a heuristic (no-LLM) importer, images under `data/images`
    /// and the MCP HTTP endpoint disabled until a key is configured.
    pub fn new(db: Db, node: NodeInfo) -> Self {
        Self {
            inner: Arc::new(Inner {
                db,
                node,
                importer: RecipeImporter::new(Arc::new(HeuristicExtractor)),
                images: ImageStore::new(PathBuf::from("data/images")),
                mcp_api_key: None,
                mcp_allowed_hosts: default_mcp_allowed_hosts(),
            }),
        }
    }

    fn edit(mut self, f: impl FnOnce(&mut Inner)) -> Self {
        if let Some(inner) = Arc::get_mut(&mut self.inner) {
            f(inner);
        }
        self
    }

    pub fn with_importer(self, importer: RecipeImporter) -> Self {
        self.edit(|i| i.importer = importer)
    }

    pub fn with_image_dir(self, dir: impl Into<PathBuf>) -> Self {
        let store = ImageStore::new(dir);
        self.edit(|i| i.images = store)
    }

    /// Enable the bearer-protected `/mcp` endpoint with this key.
    pub fn with_mcp_api_key(self, key: impl Into<String>) -> Self {
        let key = key.into();
        self.edit(|i| i.mcp_api_key = Some(key))
    }

    /// Hosts accepted in the `Host` header on `/mcp`; empty disables the check.
    pub fn with_mcp_allowed_hosts(self, hosts: Vec<String>) -> Self {
        self.edit(|i| i.mcp_allowed_hosts = hosts)
    }

    pub fn db(&self) -> &Db {
        &self.inner.db
    }

    pub fn node(&self) -> &NodeInfo {
        &self.inner.node
    }

    pub fn importer(&self) -> &RecipeImporter {
        &self.inner.importer
    }

    pub fn images(&self) -> &ImageStore {
        &self.inner.images
    }

    pub fn mcp_api_key(&self) -> Option<&str> {
        self.inner.mcp_api_key.as_deref()
    }

    pub fn mcp_allowed_hosts(&self) -> &[String] {
        &self.inner.mcp_allowed_hosts
    }

    /// Import pipeline bound to this node's database and image store.
    pub fn imports(&self) -> ImportService {
        ImportService::new(self.inner.db.clone(), self.inner.importer.clone(), self.inner.images.clone())
    }

    /// Translation service bound to this node's database and model.
    pub fn translations(&self) -> TranslationService {
        TranslationService::new(self.inner.db.clone(), self.inner.importer.clone())
    }

    /// A fresh MCP server instance sharing this node's services.
    pub fn mcp_server(&self) -> RecipeMcpServer {
        RecipeMcpServer::new(self.inner.db.clone(), self.inner.importer.clone(), self.inner.images.clone())
    }
}
