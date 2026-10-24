//! `recipe-mcp`: MCP tool definitions (`search_recipes`, `get_recipe_details`,
//! `import_recipe`, `build_smart_shopping_list`) shared by the stdio and
//! Streamable HTTP transports, plus the application-level [`ImportService`]
//! that both the MCP tools and the REST API use to persist imports.

pub mod error;
pub mod server;
pub mod services;
pub mod tools;
pub mod translations;

pub use error::McpToolError;
pub use server::RecipeMcpServer;
pub use services::{ImportError, ImportService, MealieImportReport};
pub use translations::{BackfillReport, TranslationService};
