//! The MCP server: four tools over any rmcp transport.

use recipe_core::{LocalizedRecipe, Recipe};
use recipe_db::Db;
use recipe_scraper::RecipeImporter;
use recipe_scraper::images::ImageStore;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ServerHandler, tool, tool_handler, tool_router};

use crate::tools::details::{GetRecipeDetailsParams, get_details};
use crate::tools::import::{ImportRecipeParams, import};
use crate::tools::search::{SearchRecipesOutput, SearchRecipesParams, search};
use crate::tools::shopping::{BuildShoppingListParams, ShoppingList, build};
use crate::{ImportService, McpToolError, TranslationService};

const INSTRUCTIONS: &str = "This node stores a home cook's recipes with normalized metric ingredients. \
Use search_recipes to find candidates (optionally by pantry contents, exclusions and time budget), \
get_recipe_details for full steps scaled to a serving count, import_recipe to add recipes from a URL or pasted text, \
and build_smart_shopping_list to aggregate ingredients for several recipes into basic and specialty buckets.";

/// MCP server exposing the recipe node. Cheap to clone; one instance per session.
#[derive(Clone)]
pub struct RecipeMcpServer {
    db: Db,
    imports: ImportService,
    translations: TranslationService,
}

#[tool_router]
impl RecipeMcpServer {
    pub fn new(db: Db, importer: RecipeImporter, images: ImageStore) -> Self {
        Self {
            imports: ImportService::new(db.clone(), importer.clone(), images),
            translations: TranslationService::new(db.clone(), importer),
            db,
        }
    }

    #[tool(
        name = "search_recipes",
        description = "Search the recipe node by keywords, available pantry ingredients (canonical ids), excluded allergens and maximum total time. Returns summaries with the ingredients still missing from the pantry."
    )]
    async fn search_recipes(
        &self,
        Parameters(params): Parameters<SearchRecipesParams>,
    ) -> Result<Json<SearchRecipesOutput>, McpToolError> {
        search(&self.db, params).await.map(Json)
    }

    #[tool(
        name = "get_recipe_details",
        description = "Fetch the complete recipe (ingredients, steps with timers, tags). Pass target_servings to scale ingredient amounts and language (e.g. ru) to get translated wording and localized ingredient labels."
    )]
    async fn get_recipe_details(
        &self,
        Parameters(params): Parameters<GetRecipeDetailsParams>,
    ) -> Result<Json<LocalizedRecipe>, McpToolError> {
        get_details(&self.db, &self.translations, params).await.map(Json)
    }

    #[tool(
        name = "import_recipe",
        description = "Import and normalize a recipe from a web page URL or from raw unstructured text, then save it to the node. Returns the stored recipe."
    )]
    async fn import_recipe(&self, Parameters(params): Parameters<ImportRecipeParams>) -> Result<Json<Recipe>, McpToolError> {
        import(&self.imports, params).await.map(Json)
    }

    #[tool(
        name = "build_smart_shopping_list",
        description = "Aggregate ingredients for several recipes at chosen servings, sum identical items, drop pantry stock, and split into basic_groceries (supermarket) and specialty_items (online/ethnic stores)."
    )]
    async fn build_smart_shopping_list(
        &self,
        Parameters(params): Parameters<BuildShoppingListParams>,
    ) -> Result<Json<ShoppingList>, McpToolError> {
        build(&self.db, &self.translations, params).await.map(Json)
    }
}

#[tool_handler]
impl ServerHandler for RecipeMcpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("recipe-node", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }
}
