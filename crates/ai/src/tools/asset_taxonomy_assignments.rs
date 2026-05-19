//! Get asset taxonomy assignments tool.
//!
//! Resolves a user-facing symbol to an asset_id, then returns all
//! taxonomy assignments for that asset (optionally filtered by taxonomy_id).

use rig::{completion::ToolDefinition, tool::Tool};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::env::AiEnvironment;
use crate::error::AiError;

// ============================================================================
// Symbol Resolver
// ============================================================================

/// Resolves a user-visible symbol (e.g. "VWRL" or "VWRL.MI") to an opaque
/// `asset_id` using exact, case-insensitive `instrument_symbol` matching.
///
/// Tie-break rules:
/// - 0 matches → Err("Asset not found for symbol '{symbol}'")
/// - 1 match   → Ok(asset_id)
/// - 2+ matches → Err("Ambiguous symbol '{symbol}', please specify with exchange suffix (e.g. 'VWRL.MI')")
pub fn resolve_symbol(
    assets_service: &Arc<dyn wealthfolio_core::assets::AssetServiceTrait>,
    symbol: &str,
) -> Result<String, AiError> {
    let matches = assets_service
        .search_by_symbol(symbol)
        .map_err(|e| AiError::ToolExecutionFailed(e.to_string()))?;

    match matches.len() {
        0 => Err(AiError::ToolExecutionFailed(format!(
            "Asset not found for symbol '{symbol}'"
        ))),
        1 => Ok(matches.into_iter().next().unwrap().id),
        _ => Err(AiError::ToolExecutionFailed(format!(
            "Ambiguous symbol '{symbol}', please specify with exchange suffix (e.g. 'VWRL.MI')"
        ))),
    }
}

// ============================================================================
// Tool Arguments and Output
// ============================================================================

/// Arguments for the get_asset_taxonomy_assignments tool.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetAssetTaxonomyAssignmentsArgs {
    /// Ticker symbol for the asset (e.g. "AAPL", "VWRL.MI").
    pub symbol: String,

    /// Optional taxonomy ID filter. If omitted, returns all taxonomies.
    pub taxonomy_id: Option<String>,
}

/// DTO for a single taxonomy assignment row.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaxonomyAssignmentDto {
    pub taxonomy_id: String,
    pub taxonomy_name: String,
    pub category_id: String,
    pub category_name: String,
    /// Weight in basis points (10 000 = 100%).
    pub weight_basis_points: i32,
    pub source: String,
}

/// Output envelope for the get_asset_taxonomy_assignments tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetAssetTaxonomyAssignmentsOutput {
    pub symbol: String,
    pub asset_id: String,
    pub assignments: Vec<TaxonomyAssignmentDto>,
}

// ============================================================================
// Tool Implementation
// ============================================================================

/// Tool to get taxonomy assignments for an asset identified by symbol.
pub struct GetAssetTaxonomyAssignmentsTool<E: AiEnvironment> {
    env: Arc<E>,
}

impl<E: AiEnvironment> GetAssetTaxonomyAssignmentsTool<E> {
    pub fn new(env: Arc<E>) -> Self {
        Self { env }
    }
}

impl<E: AiEnvironment> Clone for GetAssetTaxonomyAssignmentsTool<E> {
    fn clone(&self) -> Self {
        Self {
            env: self.env.clone(),
        }
    }
}

impl<E: AiEnvironment + 'static> Tool for GetAssetTaxonomyAssignmentsTool<E> {
    const NAME: &'static str = "get_asset_taxonomy_assignments";

    type Error = AiError;
    type Args = GetAssetTaxonomyAssignmentsArgs;
    type Output = GetAssetTaxonomyAssignmentsOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Get taxonomy assignments for an asset. Returns how the asset is classified across taxonomies (regions, sectors, asset classes, etc.). Optionally filter to a single taxonomy.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "symbol": {
                        "type": "string",
                        "description": "Ticker symbol for the asset (e.g. 'AAPL', 'VWRL.MI')"
                    },
                    "taxonomyId": {
                        "type": "string",
                        "description": "Optional: taxonomy ID to filter results (use list_taxonomies to discover IDs)"
                    }
                },
                "required": ["symbol"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        // Resolve symbol → asset_id
        let asset_id = resolve_symbol(&self.env.assets_service(), &args.symbol)?;

        // Fetch raw assignments
        let raw_assignments = self
            .env
            .taxonomy_service()
            .get_asset_assignments(&asset_id)
            .map_err(|e| AiError::ToolExecutionFailed(e.to_string()))?;

        // Optionally filter by taxonomy_id
        let raw_assignments: Vec<_> = if let Some(ref tid) = args.taxonomy_id {
            raw_assignments
                .into_iter()
                .filter(|a| &a.taxonomy_id == tid)
                .collect()
        } else {
            raw_assignments
        };

        // Enrich with names by loading all taxonomies once
        let taxonomies = self
            .env
            .taxonomy_service()
            .get_taxonomies_with_categories()
            .map_err(|e| AiError::ToolExecutionFailed(e.to_string()))?;

        // Build lookup: taxonomy_id → (taxonomy_name, category_id → category_name)
        let lookup: std::collections::HashMap<
            String,
            (String, std::collections::HashMap<String, String>),
        > = taxonomies
            .into_iter()
            .map(|twc| {
                let cat_map: std::collections::HashMap<String, String> =
                    twc.categories.into_iter().map(|c| (c.id, c.name)).collect();
                (twc.taxonomy.id, (twc.taxonomy.name, cat_map))
            })
            .collect();

        let assignments = raw_assignments
            .into_iter()
            .map(|a| {
                let taxonomy_name = lookup
                    .get(&a.taxonomy_id)
                    .map(|(tn, _)| tn.clone())
                    .unwrap_or_else(|| a.taxonomy_id.clone());
                let category_name = lookup
                    .get(&a.taxonomy_id)
                    .and_then(|(_, cm)| cm.get(&a.category_id))
                    .cloned()
                    .unwrap_or_else(|| a.category_id.clone());
                TaxonomyAssignmentDto {
                    taxonomy_id: a.taxonomy_id,
                    taxonomy_name,
                    category_id: a.category_id,
                    category_name,
                    weight_basis_points: a.weight,
                    source: a.source,
                }
            })
            .collect();

        Ok(GetAssetTaxonomyAssignmentsOutput {
            symbol: args.symbol,
            asset_id,
            assignments,
        })
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::test_env::{MockAssetsService, MockEnvironment, MockTaxonomyService};
    use chrono::NaiveDateTime;
    use wealthfolio_core::{
        assets::Asset,
        taxonomies::{AssetTaxonomyAssignment, Category, Taxonomy, TaxonomyWithCategories},
    };

    fn make_asset(id: &str, symbol: &str) -> Asset {
        Asset {
            id: id.to_string(),
            kind: wealthfolio_core::assets::AssetKind::Investment,
            name: Some(id.to_string()),
            display_code: None,
            notes: None,
            metadata: None,
            is_active: true,
            quote_mode: wealthfolio_core::assets::QuoteMode::Market,
            quote_ccy: "USD".to_string(),
            instrument_type: None,
            instrument_symbol: Some(symbol.to_string()),
            instrument_exchange_mic: None,
            instrument_key: None,
            provider_config: None,
            exchange_name: None,
            created_at: NaiveDateTime::default(),
            updated_at: NaiveDateTime::default(),
        }
    }

    fn make_taxonomy(id: &str, name: &str) -> Taxonomy {
        let now = NaiveDateTime::default();
        Taxonomy {
            id: id.to_string(),
            name: name.to_string(),
            color: "#ffffff".to_string(),
            description: None,
            is_system: false,
            is_single_select: true,
            sort_order: 0,
            created_at: now,
            updated_at: now,
        }
    }

    fn make_category(id: &str, taxonomy_id: &str, name: &str) -> Category {
        let now = NaiveDateTime::default();
        Category {
            id: id.to_string(),
            taxonomy_id: taxonomy_id.to_string(),
            parent_id: None,
            name: name.to_string(),
            key: id.to_string(),
            color: "#808080".to_string(),
            description: None,
            sort_order: 0,
            created_at: now,
            updated_at: now,
        }
    }

    fn make_assignment(
        asset_id: &str,
        taxonomy_id: &str,
        category_id: &str,
        weight: i32,
    ) -> AssetTaxonomyAssignment {
        AssetTaxonomyAssignment {
            id: format!("{}-{}-{}", asset_id, taxonomy_id, category_id),
            asset_id: asset_id.to_string(),
            taxonomy_id: taxonomy_id.to_string(),
            category_id: category_id.to_string(),
            weight,
            source: "manual".to_string(),
            created_at: NaiveDateTime::default(),
            updated_at: NaiveDateTime::default(),
        }
    }

    // --- SymbolResolver tests ---

    // Test 1 (RED→GREEN): 0 matches → error
    #[test]
    fn symbol_resolver_no_match_returns_error() {
        let svc = Arc::new(MockAssetsService { assets: vec![] });
        let result = resolve_symbol(
            &(svc as Arc<dyn wealthfolio_core::assets::AssetServiceTrait>),
            "UNKNOWN",
        );
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("Asset not found for symbol 'UNKNOWN'"),
            "got: {msg}"
        );
    }

    // Test 2 (RED→GREEN): 1 match → Ok with correct asset_id
    #[test]
    fn symbol_resolver_single_match_returns_asset_id() {
        let asset = make_asset("AAPL", "AAPL");
        let svc = Arc::new(MockAssetsService {
            assets: vec![asset],
        });
        let result = resolve_symbol(
            &(svc as Arc<dyn wealthfolio_core::assets::AssetServiceTrait>),
            "AAPL",
        );
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "AAPL");
    }

    // Test 3 (RED→GREEN): 2+ matches → ambiguous error
    #[test]
    fn symbol_resolver_multiple_matches_returns_ambiguous_error() {
        let a1 = make_asset("VWRL", "VWRL");
        let a2 = make_asset("VWRL.MI", "VWRL");
        let svc = Arc::new(MockAssetsService {
            assets: vec![a1, a2],
        });
        let result = resolve_symbol(
            &(svc as Arc<dyn wealthfolio_core::assets::AssetServiceTrait>),
            "VWRL",
        );
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("Ambiguous symbol 'VWRL'"), "got: {msg}");
        assert!(msg.contains("exchange suffix"), "got: {msg}");
    }

    // --- Tool smoke test ---

    // Test 4 (RED→GREEN): tool returns enriched DTO with all fields populated
    #[tokio::test]
    async fn tool_returns_enriched_assignments() {
        let asset = make_asset("VWRL.MI", "VWRL.MI");
        let taxonomy = make_taxonomy("regions", "Regions");
        let category = make_category("EUROPE", "regions", "Europe");
        let assignment = make_assignment("VWRL.MI", "regions", "EUROPE", 10000);

        let mut taxonomy_svc = MockTaxonomyService::default();
        // Override get_asset_assignments to return our assignment
        taxonomy_svc.taxonomies = vec![TaxonomyWithCategories {
            taxonomy,
            categories: vec![category],
        }];

        let mut env = MockEnvironment::new();
        env.assets_service = Arc::new(MockAssetsService {
            assets: vec![asset],
        });
        env.taxonomy_service = Arc::new(MockTaxonomyServiceWithAssignments {
            inner: taxonomy_svc,
            assignments: vec![assignment],
        });

        let tool = GetAssetTaxonomyAssignmentsTool::new(Arc::new(env));
        let result = tool
            .call(GetAssetTaxonomyAssignmentsArgs {
                symbol: "VWRL.MI".to_string(),
                taxonomy_id: None,
            })
            .await;

        assert!(result.is_ok(), "tool call failed: {:?}", result.err());
        let output = result.unwrap();
        assert_eq!(output.symbol, "VWRL.MI");
        assert_eq!(output.asset_id, "VWRL.MI");
        assert_eq!(output.assignments.len(), 1);

        let a = &output.assignments[0];
        assert_eq!(a.taxonomy_id, "regions");
        assert_eq!(a.taxonomy_name, "Regions");
        assert_eq!(a.category_id, "EUROPE");
        assert_eq!(a.category_name, "Europe");
        assert_eq!(a.weight_basis_points, 10000);
        assert_eq!(a.source, "manual");
    }

    /// Extended mock taxonomy service that also serves assignments.
    struct MockTaxonomyServiceWithAssignments {
        inner: MockTaxonomyService,
        assignments: Vec<AssetTaxonomyAssignment>,
    }

    #[async_trait::async_trait]
    impl wealthfolio_core::taxonomies::TaxonomyServiceTrait for MockTaxonomyServiceWithAssignments {
        fn get_taxonomies(
            &self,
        ) -> wealthfolio_core::errors::Result<Vec<wealthfolio_core::taxonomies::Taxonomy>> {
            self.inner.get_taxonomies()
        }

        fn get_taxonomy(
            &self,
            id: &str,
        ) -> wealthfolio_core::errors::Result<
            Option<wealthfolio_core::taxonomies::TaxonomyWithCategories>,
        > {
            self.inner.get_taxonomy(id)
        }

        fn get_taxonomies_with_categories(
            &self,
        ) -> wealthfolio_core::errors::Result<
            Vec<wealthfolio_core::taxonomies::TaxonomyWithCategories>,
        > {
            self.inner.get_taxonomies_with_categories()
        }

        async fn create_taxonomy(
            &self,
            _taxonomy: wealthfolio_core::taxonomies::NewTaxonomy,
        ) -> wealthfolio_core::errors::Result<wealthfolio_core::taxonomies::Taxonomy> {
            unimplemented!()
        }

        async fn update_taxonomy(
            &self,
            _taxonomy: wealthfolio_core::taxonomies::Taxonomy,
        ) -> wealthfolio_core::errors::Result<wealthfolio_core::taxonomies::Taxonomy> {
            unimplemented!()
        }

        async fn delete_taxonomy(&self, _id: &str) -> wealthfolio_core::errors::Result<usize> {
            unimplemented!()
        }

        async fn create_category(
            &self,
            _category: wealthfolio_core::taxonomies::NewCategory,
        ) -> wealthfolio_core::errors::Result<wealthfolio_core::taxonomies::Category> {
            unimplemented!()
        }

        async fn update_category(
            &self,
            _category: wealthfolio_core::taxonomies::Category,
        ) -> wealthfolio_core::errors::Result<wealthfolio_core::taxonomies::Category> {
            unimplemented!()
        }

        async fn delete_category(
            &self,
            _taxonomy_id: &str,
            _category_id: &str,
        ) -> wealthfolio_core::errors::Result<usize> {
            unimplemented!()
        }

        async fn move_category(
            &self,
            _taxonomy_id: &str,
            _category_id: &str,
            _new_parent_id: Option<String>,
            _position: i32,
        ) -> wealthfolio_core::errors::Result<wealthfolio_core::taxonomies::Category> {
            unimplemented!()
        }

        async fn import_taxonomy_json(
            &self,
            _json_str: &str,
        ) -> wealthfolio_core::errors::Result<wealthfolio_core::taxonomies::Taxonomy> {
            unimplemented!()
        }

        fn export_taxonomy_json(&self, _id: &str) -> wealthfolio_core::errors::Result<String> {
            unimplemented!()
        }

        fn get_asset_assignments(
            &self,
            _asset_id: &str,
        ) -> wealthfolio_core::errors::Result<
            Vec<wealthfolio_core::taxonomies::AssetTaxonomyAssignment>,
        > {
            Ok(self.assignments.clone())
        }

        fn get_category_assignments(
            &self,
            _taxonomy_id: &str,
            _category_id: &str,
        ) -> wealthfolio_core::errors::Result<
            Vec<wealthfolio_core::taxonomies::AssetTaxonomyAssignment>,
        > {
            Ok(vec![])
        }

        async fn assign_asset_to_category(
            &self,
            _assignment: wealthfolio_core::taxonomies::NewAssetTaxonomyAssignment,
        ) -> wealthfolio_core::errors::Result<wealthfolio_core::taxonomies::AssetTaxonomyAssignment>
        {
            unimplemented!()
        }

        async fn remove_asset_assignment(
            &self,
            _id: &str,
        ) -> wealthfolio_core::errors::Result<usize> {
            unimplemented!()
        }
    }
}
