//! Set asset taxonomy assignments tool.
//!
//! Atomically replaces all taxonomy assignments for (asset_id, taxonomy_id).
//! Validates weights before any DB writes, then calls the service layer.

use rig::{completion::ToolDefinition, tool::Tool};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use wealthfolio_core::taxonomies::NewAssetTaxonomyAssignment;

use crate::env::AiEnvironment;
use crate::error::AiError;
use crate::tools::asset_taxonomy_assignments::resolve_symbol;

// ============================================================================
// Tool Arguments and Output
// ============================================================================

/// A single assignment row in the tool input.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssignmentInput {
    pub category_id: String,
    /// Weight in basis points (0–10000, where 10000 = 100%).
    pub weight_basis_points: i32,
}

/// Arguments for the set_asset_taxonomy_assignments tool.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetAssetTaxonomyAssignmentsArgs {
    /// Ticker symbol for the asset (e.g. "AAPL", "VWRL.MI").
    pub symbol: String,
    /// Taxonomy ID to replace assignments for.
    pub taxonomy_id: String,
    /// New assignment rows. Empty list clears all assignments.
    pub assignments: Vec<AssignmentInput>,
}

/// A single assignment row in the tool output.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssignmentOutput {
    pub category_id: String,
    pub category_name: String,
    pub weight_basis_points: i32,
    pub source: String,
}

/// Output for the set_asset_taxonomy_assignments tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetAssetTaxonomyAssignmentsOutput {
    pub assignments: Vec<AssignmentOutput>,
    /// Remaining unallocated basis points (10000 - sum). 0 when fully allocated.
    pub unallocated_basis_points: i32,
}

// ============================================================================
// Validation
// ============================================================================

/// Validate assignment weights before any DB writes.
/// Returns the total sum if valid, or an AiError describing the violation.
fn validate_weights(assignments: &[AssignmentInput]) -> Result<i32, AiError> {
    for a in assignments {
        if a.weight_basis_points < 0 {
            return Err(AiError::ToolExecutionFailed(format!(
                "weight_basis_points must be >= 0, got {} for category '{}'",
                a.weight_basis_points, a.category_id
            )));
        }
        if a.weight_basis_points > 10000 {
            return Err(AiError::ToolExecutionFailed(format!(
                "weight_basis_points must be <= 10000, got {} for category '{}'",
                a.weight_basis_points, a.category_id
            )));
        }
    }

    let total: i32 = assignments.iter().map(|a| a.weight_basis_points).sum();
    if total > 10000 {
        return Err(AiError::ToolExecutionFailed(format!(
            "Total weight_basis_points ({total}) exceeds 10000 (100%)"
        )));
    }

    Ok(total)
}

// ============================================================================
// Tool Implementation
// ============================================================================

/// Tool to atomically replace taxonomy assignments for an asset.
pub struct SetAssetTaxonomyAssignmentsTool<E: AiEnvironment> {
    env: Arc<E>,
}

impl<E: AiEnvironment> SetAssetTaxonomyAssignmentsTool<E> {
    pub fn new(env: Arc<E>) -> Self {
        Self { env }
    }
}

impl<E: AiEnvironment> Clone for SetAssetTaxonomyAssignmentsTool<E> {
    fn clone(&self) -> Self {
        Self {
            env: self.env.clone(),
        }
    }
}

impl<E: AiEnvironment + 'static> Tool for SetAssetTaxonomyAssignmentsTool<E> {
    const NAME: &'static str = "set_asset_taxonomy_assignments";

    type Error = AiError;
    type Args = SetAssetTaxonomyAssignmentsArgs;
    type Output = SetAssetTaxonomyAssignmentsOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Atomically replace all taxonomy assignments for an asset within a given taxonomy. \
                Weights are in basis points (10000 = 100%). Partial allocation is allowed; the response \
                includes unallocated_basis_points. Pass an empty assignments list to clear all assignments. \
                Example: [{\"categoryId\": \"US\", \"weightBasisPoints\": 6000}, {\"categoryId\": \"EU\", \"weightBasisPoints\": 4000}]"
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "symbol": {
                        "type": "string",
                        "description": "Ticker symbol for the asset (e.g. 'AAPL', 'VWRL.MI')"
                    },
                    "taxonomyId": {
                        "type": "string",
                        "description": "Taxonomy ID to replace assignments for (use list_taxonomies to discover IDs)"
                    },
                    "assignments": {
                        "type": "array",
                        "description": "New assignment rows. Empty array clears all assignments.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "categoryId": {
                                    "type": "string",
                                    "description": "Category ID within the taxonomy"
                                },
                                "weightBasisPoints": {
                                    "type": "integer",
                                    "description": "Weight in basis points (0–10000, where 10000 = 100%)"
                                }
                            },
                            "required": ["categoryId", "weightBasisPoints"]
                        }
                    }
                },
                "required": ["symbol", "taxonomyId", "assignments"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        // 1. Validate weights before any DB writes
        let total = validate_weights(&args.assignments)?;

        // 2. Resolve symbol → asset_id
        let asset_id = resolve_symbol(&self.env.assets_service(), &args.symbol)?;

        // 3. Build NewAssetTaxonomyAssignment list with source = "ai"
        let new_assignments: Vec<NewAssetTaxonomyAssignment> = args
            .assignments
            .iter()
            .map(|a| NewAssetTaxonomyAssignment {
                id: None,
                asset_id: asset_id.clone(),
                taxonomy_id: args.taxonomy_id.clone(),
                category_id: a.category_id.clone(),
                weight: a.weight_basis_points,
                source: "ai".to_string(),
            })
            .collect();

        // 4. Atomic replace via service
        let persisted = self
            .env
            .taxonomy_service()
            .replace_asset_assignments(&asset_id, &args.taxonomy_id, new_assignments)
            .await
            .map_err(|e| AiError::ToolExecutionFailed(e.to_string()))?;

        // 5. Enrich with category names by loading taxonomies
        let taxonomies = self
            .env
            .taxonomy_service()
            .get_taxonomies_with_categories()
            .map_err(|e| AiError::ToolExecutionFailed(e.to_string()))?;

        let cat_name_lookup: std::collections::HashMap<String, String> = taxonomies
            .into_iter()
            .flat_map(|twc| twc.categories.into_iter().map(|c| (c.id, c.name)))
            .collect();

        let assignments = persisted
            .into_iter()
            .map(|a| {
                let category_name = cat_name_lookup
                    .get(&a.category_id)
                    .cloned()
                    .unwrap_or_else(|| a.category_id.clone());
                AssignmentOutput {
                    category_id: a.category_id,
                    category_name,
                    weight_basis_points: a.weight,
                    source: a.source,
                }
            })
            .collect();

        Ok(SetAssetTaxonomyAssignmentsOutput {
            assignments,
            unallocated_basis_points: 10000 - total,
        })
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::test_env::{MockAssetsService, MockEnvironment};
    use chrono::NaiveDateTime;
    use std::sync::Mutex;
    use wealthfolio_core::{
        assets::Asset,
        taxonomies::{
            AssetTaxonomyAssignment, NewAssetTaxonomyAssignment, NewCategory, NewTaxonomy,
            TaxonomyServiceTrait, TaxonomyWithCategories,
        },
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

    fn input(cat: &str, weight: i32) -> AssignmentInput {
        AssignmentInput {
            category_id: cat.to_string(),
            weight_basis_points: weight,
        }
    }

    fn args(
        symbol: &str,
        taxonomy_id: &str,
        assignments: Vec<AssignmentInput>,
    ) -> SetAssetTaxonomyAssignmentsArgs {
        SetAssetTaxonomyAssignmentsArgs {
            symbol: symbol.to_string(),
            taxonomy_id: taxonomy_id.to_string(),
            assignments,
        }
    }

    // ---- Validation-only tests (no env needed) ----

    // Test 1 (RED→GREEN): negative weight rejected
    #[test]
    fn validation_rejects_negative_weight() {
        let err = validate_weights(&[input("US", -1)]).unwrap_err();
        assert!(
            err.to_string().contains("weight_basis_points must be >= 0"),
            "got: {err}"
        );
    }

    // Test 2 (RED→GREEN): weight > 10000 rejected
    #[test]
    fn validation_rejects_weight_above_10000() {
        let err = validate_weights(&[input("US", 10001)]).unwrap_err();
        assert!(
            err.to_string()
                .contains("weight_basis_points must be <= 10000"),
            "got: {err}"
        );
    }

    // Test 3 (RED→GREEN): sum > 10000 rejected
    #[test]
    fn validation_rejects_sum_above_10000() {
        let err = validate_weights(&[input("US", 6000), input("EU", 5000)]).unwrap_err();
        assert!(err.to_string().contains("exceeds 10000"), "got: {err}");
    }

    // Test 4 (RED→GREEN): sum < 10000 accepted, returns correct total
    #[test]
    fn validation_accepts_partial_allocation() {
        let total = validate_weights(&[input("US", 6000), input("EU", 3000)]).unwrap();
        assert_eq!(total, 9000);
    }

    // Test 5 (RED→GREEN): sum = 10000 accepted
    #[test]
    fn validation_accepts_full_allocation() {
        let total = validate_weights(&[input("US", 6000), input("EU", 4000)]).unwrap();
        assert_eq!(total, 10000);
    }

    // Test 6 (RED→GREEN): empty list accepted
    #[test]
    fn validation_accepts_empty_list() {
        let total = validate_weights(&[]).unwrap();
        assert_eq!(total, 0);
    }

    // ---- Tool integration tests (with mock env) ----

    /// Mock taxonomy service that captures calls to replace_asset_assignments.
    struct MockSetTaxonomyService {
        assignments: Mutex<Vec<AssetTaxonomyAssignment>>,
    }

    impl MockSetTaxonomyService {
        fn new() -> Self {
            Self {
                assignments: Mutex::new(vec![]),
            }
        }
    }

    #[async_trait::async_trait]
    impl TaxonomyServiceTrait for MockSetTaxonomyService {
        fn get_taxonomies(
            &self,
        ) -> wealthfolio_core::errors::Result<Vec<wealthfolio_core::taxonomies::Taxonomy>> {
            Ok(vec![])
        }

        fn get_taxonomy(
            &self,
            _id: &str,
        ) -> wealthfolio_core::errors::Result<Option<TaxonomyWithCategories>> {
            Ok(None)
        }

        fn get_taxonomies_with_categories(
            &self,
        ) -> wealthfolio_core::errors::Result<Vec<TaxonomyWithCategories>> {
            Ok(vec![])
        }

        async fn create_taxonomy(
            &self,
            _taxonomy: NewTaxonomy,
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
            _category: NewCategory,
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
        ) -> wealthfolio_core::errors::Result<Vec<AssetTaxonomyAssignment>> {
            Ok(vec![])
        }

        fn get_category_assignments(
            &self,
            _taxonomy_id: &str,
            _category_id: &str,
        ) -> wealthfolio_core::errors::Result<Vec<AssetTaxonomyAssignment>> {
            Ok(vec![])
        }

        async fn assign_asset_to_category(
            &self,
            _assignment: NewAssetTaxonomyAssignment,
        ) -> wealthfolio_core::errors::Result<AssetTaxonomyAssignment> {
            unimplemented!()
        }

        async fn remove_asset_assignment(
            &self,
            _id: &str,
        ) -> wealthfolio_core::errors::Result<usize> {
            unimplemented!()
        }

        async fn replace_asset_assignments(
            &self,
            asset_id: &str,
            taxonomy_id: &str,
            assignments: Vec<NewAssetTaxonomyAssignment>,
        ) -> wealthfolio_core::errors::Result<Vec<AssetTaxonomyAssignment>> {
            let persisted: Vec<AssetTaxonomyAssignment> = assignments
                .into_iter()
                .map(|a| AssetTaxonomyAssignment {
                    id: uuid::Uuid::new_v4().to_string(),
                    asset_id: asset_id.to_string(),
                    taxonomy_id: taxonomy_id.to_string(),
                    category_id: a.category_id,
                    weight: a.weight,
                    source: a.source,
                    created_at: NaiveDateTime::default(),
                    updated_at: NaiveDateTime::default(),
                })
                .collect();
            *self.assignments.lock().unwrap() = persisted.clone();
            Ok(persisted)
        }
    }

    fn make_env_with_asset(asset: Asset) -> (MockEnvironment, Arc<MockSetTaxonomyService>) {
        let taxonomy_svc = Arc::new(MockSetTaxonomyService::new());
        let mut env = MockEnvironment::new();
        env.assets_service = Arc::new(MockAssetsService {
            assets: vec![asset],
        });
        env.taxonomy_service = taxonomy_svc.clone();
        (env, taxonomy_svc)
    }

    // Test 7 (RED→GREEN): unknown symbol propagates resolver error
    #[tokio::test]
    async fn tool_unknown_symbol_returns_error() {
        let (env, _) = make_env_with_asset(make_asset("AAPL", "AAPL"));
        let tool = SetAssetTaxonomyAssignmentsTool::new(Arc::new(env));
        let result = tool
            .call(args("UNKNOWN", "regions", vec![input("US", 5000)]))
            .await;
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("Asset not found"),
            "expected 'Asset not found'"
        );
    }

    // Test 8 (RED→GREEN): validation runs before symbol resolution (negative weight → error before DB)
    #[tokio::test]
    async fn tool_rejects_invalid_weights_before_db() {
        let (env, svc) = make_env_with_asset(make_asset("AAPL", "AAPL"));
        let tool = SetAssetTaxonomyAssignmentsTool::new(Arc::new(env));
        let result = tool
            .call(args("AAPL", "regions", vec![input("US", -1)]))
            .await;
        assert!(result.is_err());
        // DB was not called (replace_asset_assignments not invoked)
        assert!(svc.assignments.lock().unwrap().is_empty());
    }

    // Test 9 (RED→GREEN): successful path → rows have source = "ai"
    #[tokio::test]
    async fn tool_successful_path_rows_have_source_ai() {
        let (env, _) = make_env_with_asset(make_asset("AAPL", "AAPL"));
        let tool = SetAssetTaxonomyAssignmentsTool::new(Arc::new(env));
        let result = tool
            .call(args(
                "AAPL",
                "regions",
                vec![input("US", 6000), input("EU", 4000)],
            ))
            .await
            .unwrap();
        assert_eq!(result.assignments.len(), 2);
        assert!(result.assignments.iter().all(|a| a.source == "ai"));
        assert_eq!(result.unallocated_basis_points, 0);
    }

    // Test 10 (RED→GREEN): partial allocation → correct unallocated_basis_points
    #[tokio::test]
    async fn tool_partial_allocation_reports_unallocated() {
        let (env, _) = make_env_with_asset(make_asset("AAPL", "AAPL"));
        let tool = SetAssetTaxonomyAssignmentsTool::new(Arc::new(env));
        let result = tool
            .call(args("AAPL", "regions", vec![input("US", 7000)]))
            .await
            .unwrap();
        assert_eq!(result.unallocated_basis_points, 3000);
    }

    // Test 11 (RED→GREEN): empty assignments → clears, unallocated = 10000
    #[tokio::test]
    async fn tool_empty_assignments_clears_and_reports_fully_unallocated() {
        let (env, _) = make_env_with_asset(make_asset("AAPL", "AAPL"));
        let tool = SetAssetTaxonomyAssignmentsTool::new(Arc::new(env));
        let result = tool.call(args("AAPL", "regions", vec![])).await.unwrap();
        assert!(result.assignments.is_empty());
        assert_eq!(result.unallocated_basis_points, 10000);
    }
}
