//! List taxonomies tool — lets the AI agent discover all taxonomies and their categories.

use rig::{completion::ToolDefinition, tool::Tool};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::env::AiEnvironment;
use crate::error::AiError;

// ============================================================================
// Tool Arguments and Output
// ============================================================================

/// Arguments for the list_taxonomies tool (no parameters needed).
#[derive(Debug, Deserialize)]
pub struct ListTaxonomiesArgs {}

/// DTO for a single category within a taxonomy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryDto {
    pub category_id: String,
    pub category_name: String,
    pub parent_id: Option<String>,
    pub sort_order: i32,
}

/// DTO for a single taxonomy with its categories.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaxonomyDto {
    pub taxonomy_id: String,
    pub taxonomy_name: String,
    pub is_single_select: bool,
    pub categories: Vec<CategoryDto>,
}

/// Output envelope for the list_taxonomies tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListTaxonomiesOutput {
    pub taxonomies: Vec<TaxonomyDto>,
}

// ============================================================================
// Tool Implementation
// ============================================================================

/// Tool to list all taxonomies and their categories.
pub struct ListTaxonomiesTool<E: AiEnvironment> {
    env: Arc<E>,
}

impl<E: AiEnvironment> ListTaxonomiesTool<E> {
    pub fn new(env: Arc<E>) -> Self {
        Self { env }
    }
}

impl<E: AiEnvironment> Clone for ListTaxonomiesTool<E> {
    fn clone(&self) -> Self {
        Self {
            env: self.env.clone(),
        }
    }
}

impl<E: AiEnvironment + 'static> Tool for ListTaxonomiesTool<E> {
    const NAME: &'static str = "list_taxonomies";

    type Error = AiError;
    type Args = ListTaxonomiesArgs;
    type Output = ListTaxonomiesOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "List all taxonomies and their categories. Use this to discover available classification schemes (e.g., regions, sectors, asset classes) before assigning assets to categories.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        }
    }

    async fn call(&self, _args: Self::Args) -> Result<Self::Output, Self::Error> {
        let taxonomies_with_categories = self
            .env
            .taxonomy_service()
            .get_taxonomies_with_categories()
            .map_err(|e| AiError::ToolExecutionFailed(e.to_string()))?;

        let taxonomy_dtos = taxonomies_with_categories
            .into_iter()
            .map(|twc| TaxonomyDto {
                taxonomy_id: twc.taxonomy.id,
                taxonomy_name: twc.taxonomy.name,
                is_single_select: twc.taxonomy.is_single_select,
                categories: twc
                    .categories
                    .into_iter()
                    .map(|c| CategoryDto {
                        category_id: c.id,
                        category_name: c.name,
                        parent_id: c.parent_id,
                        sort_order: c.sort_order,
                    })
                    .collect(),
            })
            .collect();

        Ok(ListTaxonomiesOutput {
            taxonomies: taxonomy_dtos,
        })
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::test_env::{MockEnvironment, MockTaxonomyService};
    use chrono::NaiveDateTime;
    use wealthfolio_core::taxonomies::{Category, Taxonomy, TaxonomyWithCategories};

    fn make_taxonomy(id: &str, name: &str, is_single_select: bool) -> Taxonomy {
        let now = NaiveDateTime::default();
        Taxonomy {
            id: id.to_string(),
            name: name.to_string(),
            color: "#ffffff".to_string(),
            description: None,
            is_system: false,
            is_single_select,
            sort_order: 0,
            created_at: now,
            updated_at: now,
        }
    }

    fn make_category(
        id: &str,
        taxonomy_id: &str,
        name: &str,
        parent_id: Option<&str>,
        sort_order: i32,
    ) -> Category {
        let now = NaiveDateTime::default();
        Category {
            id: id.to_string(),
            taxonomy_id: taxonomy_id.to_string(),
            parent_id: parent_id.map(|s| s.to_string()),
            name: name.to_string(),
            key: id.to_string(),
            color: "#808080".to_string(),
            description: None,
            sort_order,
            created_at: now,
            updated_at: now,
        }
    }

    // Test 1: tool returns empty list when there are no taxonomies.
    #[tokio::test]
    async fn test_list_taxonomies_empty() {
        let mut env = MockEnvironment::new();
        env.taxonomy_service = Arc::new(MockTaxonomyService { taxonomies: vec![] });
        let tool = ListTaxonomiesTool::new(Arc::new(env));

        let result = tool.call(ListTaxonomiesArgs {}).await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.taxonomies.is_empty());
    }

    // Test 2: tool maps one taxonomy with flat categories correctly.
    #[tokio::test]
    async fn test_list_taxonomies_flat_categories() {
        let taxonomy = make_taxonomy("regions", "Regions", true);
        let cat_na = make_category("NORTH_AMERICA", "regions", "North America", None, 0);
        let cat_eu = make_category("EUROPE", "regions", "Europe", None, 1);

        let mut env = MockEnvironment::new();
        env.taxonomy_service = Arc::new(MockTaxonomyService {
            taxonomies: vec![TaxonomyWithCategories {
                taxonomy,
                categories: vec![cat_na, cat_eu],
            }],
        });
        let tool = ListTaxonomiesTool::new(Arc::new(env));

        let result = tool.call(ListTaxonomiesArgs {}).await;
        assert!(result.is_ok());
        let output = result.unwrap();

        assert_eq!(output.taxonomies.len(), 1);
        let t = &output.taxonomies[0];
        assert_eq!(t.taxonomy_id, "regions");
        assert_eq!(t.taxonomy_name, "Regions");
        assert!(t.is_single_select);
        assert_eq!(t.categories.len(), 2);
        assert!(t.categories.iter().all(|c| c.parent_id.is_none()));
        assert_eq!(t.categories[0].category_id, "NORTH_AMERICA");
        assert_eq!(t.categories[1].sort_order, 1);
    }

    // Test 3: hierarchical categories — parent_id flows through correctly.
    #[tokio::test]
    async fn test_list_taxonomies_hierarchical_categories() {
        let taxonomy = make_taxonomy("industries_gics", "Industries (GICS)", false);
        let parent = make_category("TECHNOLOGY", "industries_gics", "Technology", None, 0);
        let child = make_category(
            "SOFTWARE",
            "industries_gics",
            "Software",
            Some("TECHNOLOGY"),
            0,
        );

        let mut env = MockEnvironment::new();
        env.taxonomy_service = Arc::new(MockTaxonomyService {
            taxonomies: vec![TaxonomyWithCategories {
                taxonomy,
                categories: vec![parent, child],
            }],
        });
        let tool = ListTaxonomiesTool::new(Arc::new(env));

        let result = tool.call(ListTaxonomiesArgs {}).await;
        assert!(result.is_ok());
        let output = result.unwrap();

        let categories = &output.taxonomies[0].categories;
        let parent_cat = categories
            .iter()
            .find(|c| c.category_id == "TECHNOLOGY")
            .unwrap();
        let child_cat = categories
            .iter()
            .find(|c| c.category_id == "SOFTWARE")
            .unwrap();

        assert!(parent_cat.parent_id.is_none());
        assert_eq!(child_cat.parent_id.as_deref(), Some("TECHNOLOGY"));
    }
}
