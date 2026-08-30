//! Reference store tools for agent access to long-term data.

use crate::reference_store::{
    DatasetManifest, ReferenceRecord, ReferenceService, SearchFilters, SearchResult,
};
use crate::{EventPayload, NoticeEvent};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ReferenceToolError {
    #[error("reference store not available")]
    NotAvailable,
    #[error("dataset not found: {0}")]
    DatasetNotFound(String),
    #[error("record not found: {0}")]
    RecordNotFound(String),
    #[error("invalid filters: {0}")]
    InvalidFilters(String),
    #[error("store error: {0}")]
    StoreError(#[from] anyhow::Error),
}

/// Reference store tools
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceToolKind {
    Search,
    Get,
    ListDatasets,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferenceSearchRequest {
    pub dataset_id: Option<String>,
    pub kind: Option<String>,
    pub tags: Vec<String>,
    pub text_query: Option<String>,
    pub date_from: Option<u64>,
    pub date_to: Option<u64>,
    pub field_filters: HashMap<String, serde_json::Value>,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

fn default_limit() -> usize {
    10
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferenceSearchResponse {
    pub results: Vec<SearchResult>,
    pub total_found: usize,
    pub limited: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferenceGetRequest {
    pub dataset_id: String,
    pub record_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferenceGetResponse {
    pub record: Option<ReferenceRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferenceListDatasetsResponse {
    pub datasets: Vec<DatasetManifest>,
}

/// Reference tools for agent
pub struct ReferenceTools {
    store: Option<Arc<dyn ReferenceService>>,
}

impl ReferenceTools {
    pub fn new(store: Option<Arc<dyn ReferenceService>>) -> Self {
        Self { store }
    }

    /// Search reference records
    pub async fn search(
        &self,
        request: ReferenceSearchRequest,
    ) -> Result<ReferenceSearchResponse, ReferenceToolError> {
        let store = self
            .store
            .as_ref()
            .ok_or(ReferenceToolError::NotAvailable)?;

        let filters = SearchFilters {
            dataset_id: request.dataset_id,
            kind: request.kind,
            tags: request.tags,
            text_query: request.text_query,
            date_from: request.date_from,
            date_to: request.date_to,
            field_filters: request.field_filters,
        };

        let results = store.search(filters, request.limit).await?;
        let total = results.len();
        let limited = total >= request.limit;

        Ok(ReferenceSearchResponse {
            results,
            total_found: total,
            limited,
        })
    }

    /// Get a specific record
    pub async fn get(
        &self,
        request: ReferenceGetRequest,
    ) -> Result<ReferenceGetResponse, ReferenceToolError> {
        let store = self
            .store
            .as_ref()
            .ok_or(ReferenceToolError::NotAvailable)?;

        let record = store.get(&request.dataset_id, &request.record_id).await?;

        Ok(ReferenceGetResponse { record })
    }

    /// List available datasets
    pub async fn list_datasets(&self) -> Result<ReferenceListDatasetsResponse, ReferenceToolError> {
        let store = self
            .store
            .as_ref()
            .ok_or(ReferenceToolError::NotAvailable)?;

        let datasets = store.list_datasets().await?;

        Ok(ReferenceListDatasetsResponse { datasets })
    }

    /// Create a notice event for logging
    pub fn notice_event(&self, kind: ReferenceToolKind, message: String) -> EventPayload {
        EventPayload::Notice(NoticeEvent::Runtime {
            message: format!(
                "[reference_{}] {}",
                format!("{:?}", kind).to_lowercase(),
                message
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reference_store::{
        DatasetScope, PartitionStrategy, RecordProvenance, RecordSource, Sensitivity,
        YamlReferenceService,
    };
    use std::time::{SystemTime, UNIX_EPOCH};

    fn now_unix() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    async fn setup_test_store() -> (tempfile::TempDir, Arc<dyn ReferenceService>) {
        let dir = tempfile::tempdir().unwrap();
        let store = YamlReferenceService::new(dir.path()).unwrap();

        let manifest = DatasetManifest {
            schema_version: 1,
            id: "test-dataset".to_string(),
            kind: "test".to_string(),
            scope: DatasetScope::Workspace,
            sensitivity: Sensitivity::Private,
            partitioning: PartitionStrategy::Single,
            created_at: now_unix(),
            updated_at: now_unix(),
            metadata: HashMap::new(),
        };

        store.create_dataset(manifest).await.unwrap();

        let mut fields = HashMap::new();
        fields.insert("title".to_string(), serde_json::json!("Test Record"));
        fields.insert("project".to_string(), serde_json::json!("IMP"));

        let record = ReferenceRecord {
            id: "rec-1".to_string(),
            timestamp: Some(now_unix()),
            source: RecordSource {
                kind: "test".to_string(),
                external_id: Some("ext-1".to_string()),
            },
            fields,
            tags: vec!["test".to_string(), "example".to_string()],
            provenance: RecordProvenance {
                imported_at: now_unix(),
                importer: "test".to_string(),
                source_file: None,
            },
        };

        store.import("test-dataset", vec![record]).await.unwrap();

        (dir, Arc::new(store))
    }

    #[tokio::test]
    async fn test_search() {
        let (_dir, store) = setup_test_store().await;
        let tools = ReferenceTools::new(Some(store));

        let request = ReferenceSearchRequest {
            dataset_id: Some("test-dataset".to_string()),
            kind: None,
            tags: vec![],
            text_query: Some("Test".to_string()),
            date_from: None,
            date_to: None,
            field_filters: HashMap::new(),
            limit: 10,
        };

        let response = tools.search(request).await.unwrap();
        assert_eq!(response.results.len(), 1);
        assert_eq!(response.results[0].record.id, "rec-1");
    }

    #[tokio::test]
    async fn test_get() {
        let (_dir, store) = setup_test_store().await;
        let tools = ReferenceTools::new(Some(store));

        let request = ReferenceGetRequest {
            dataset_id: "test-dataset".to_string(),
            record_id: "rec-1".to_string(),
        };

        let response = tools.get(request).await.unwrap();
        assert!(response.record.is_some());
        assert_eq!(response.record.unwrap().id, "rec-1");
    }

    #[tokio::test]
    async fn test_list_datasets() {
        let (_dir, store) = setup_test_store().await;
        let tools = ReferenceTools::new(Some(store));

        let response = tools.list_datasets().await.unwrap();
        assert_eq!(response.datasets.len(), 1);
        assert_eq!(response.datasets[0].id, "test-dataset");
    }

    #[tokio::test]
    async fn test_not_available() {
        let tools = ReferenceTools::new(None);

        let request = ReferenceSearchRequest {
            dataset_id: Some("test".to_string()),
            kind: None,
            tags: vec![],
            text_query: None,
            date_from: None,
            date_to: None,
            field_filters: HashMap::new(),
            limit: 10,
        };

        let result = tools.search(request).await;
        assert!(matches!(result, Err(ReferenceToolError::NotAvailable)));
    }
}
