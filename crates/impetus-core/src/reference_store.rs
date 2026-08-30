//! Reference Store — YAML-based persistent storage for long-term agent data.
//!
//! Stores reference datasets (e.g., Jira Tempo worklogs) that the agent can
//! search and use as context without loading entire datasets into prompts.
//!
//! Core design:
//! - YAML is the authoritative storage (human-readable, version-control friendly)
//! - Partitioned storage (shard by month/year/project to avoid giant files)
//! - Content-addressed records with stable IDs
//! - Schema versioning for migrations
//! - Privacy-aware (public/internal/private sensitivity levels)
//! - Atomic writes (temp file + rename)
//! - Lazy loading (only top-K results reach the model)

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Sensitivity level for reference datasets
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Sensitivity {
    Public,
    Internal,
    Private,
}

/// Dataset scope
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DatasetScope {
    Global,
    Workspace,
    Session,
}

/// Partitioning strategy for dataset records
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PartitionStrategy {
    Single,
    Monthly,
    Yearly,
    ByProject,
    Custom(String),
}

/// Dataset manifest (stored as manifest.yaml in dataset directory)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetManifest {
    pub schema_version: u32,
    pub id: String,
    pub kind: String,
    pub scope: DatasetScope,
    pub sensitivity: Sensitivity,
    pub partitioning: PartitionStrategy,
    pub created_at: u64,
    pub updated_at: u64,
    #[serde(default)]
    pub metadata: HashMap<String, serde_json::Value>,
}

/// Reference record with typed fields
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferenceRecord {
    pub id: String,
    pub timestamp: Option<u64>,
    pub source: RecordSource,
    pub fields: HashMap<String, serde_json::Value>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub provenance: RecordProvenance,
}

/// Source of a reference record
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordSource {
    pub kind: String,
    pub external_id: Option<String>,
}

/// Record import/creation metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordProvenance {
    pub imported_at: u64,
    pub importer: String,
    #[serde(default)]
    pub source_file: Option<String>,
}

/// Search filters for reference records
#[derive(Debug, Clone, Default)]
pub struct SearchFilters {
    pub dataset_id: Option<String>,
    pub kind: Option<String>,
    pub tags: Vec<String>,
    pub text_query: Option<String>,
    pub date_from: Option<u64>,
    pub date_to: Option<u64>,
    pub field_filters: HashMap<String, serde_json::Value>,
}

/// Search result with ranking
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub record: ReferenceRecord,
    pub score: f64,
}

/// Service contract for reference storage
#[async_trait]
pub trait ReferenceService: Send + Sync {
    /// Import records into a dataset
    async fn import(&self, dataset_id: &str, records: Vec<ReferenceRecord>)
    -> Result<ImportResult>;

    /// List available datasets
    async fn list_datasets(&self) -> Result<Vec<DatasetManifest>>;

    /// Get dataset manifest
    async fn get_dataset(&self, dataset_id: &str) -> Result<Option<DatasetManifest>>;

    /// Search records with filters
    async fn search(&self, filters: SearchFilters, limit: usize) -> Result<Vec<SearchResult>>;

    /// Get a specific record by ID
    async fn get(&self, dataset_id: &str, record_id: &str) -> Result<Option<ReferenceRecord>>;

    /// Upsert a record (create or update)
    async fn upsert(&self, dataset_id: &str, record: ReferenceRecord) -> Result<()>;

    /// Delete a record
    async fn delete(&self, dataset_id: &str, record_id: &str) -> Result<()>;

    /// Refresh from disk (detect external modifications)
    async fn refresh(&self, dataset_id: &str) -> Result<()>;

    /// Create a new dataset
    async fn create_dataset(&self, manifest: DatasetManifest) -> Result<()>;
}

/// Result of import operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportResult {
    pub imported: usize,
    pub updated: usize,
    pub skipped: usize,
}

/// YAML-based reference service implementation
pub struct YamlReferenceService {
    root: PathBuf,
}

impl YamlReferenceService {
    /// Create a new YAML reference service
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root).context("failed to create reference store root")?;
        Ok(Self { root })
    }

    /// Default reference store location
    pub fn default_root() -> PathBuf {
        std::env::var_os("IMPETUS_DATA_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|home| PathBuf::from(home).join("Library/Application Support/Impetus"))
            })
            .unwrap_or_else(|| PathBuf::from("data"))
            .join("references")
    }

    fn dataset_dir(&self, dataset_id: &str) -> PathBuf {
        self.root.join(dataset_id)
    }

    fn manifest_path(&self, dataset_id: &str) -> PathBuf {
        self.dataset_dir(dataset_id).join("manifest.yaml")
    }

    fn records_dir(&self, dataset_id: &str) -> PathBuf {
        self.dataset_dir(dataset_id).join("records")
    }

    fn load_manifest(&self, dataset_id: &str) -> Result<Option<DatasetManifest>> {
        let path = self.manifest_path(dataset_id);
        if !path.exists() {
            return Ok(None);
        }

        let content = std::fs::read_to_string(&path).context("failed to read manifest")?;
        let manifest: DatasetManifest =
            serde_yaml::from_str(&content).context("failed to parse manifest YAML")?;

        Ok(Some(manifest))
    }

    fn save_manifest(&self, manifest: &DatasetManifest) -> Result<()> {
        let dir = self.dataset_dir(&manifest.id);
        std::fs::create_dir_all(&dir)?;

        let path = self.manifest_path(&manifest.id);
        let yaml = serde_yaml::to_string(manifest)?;

        // Atomic write: temp file + rename
        let temp_path = path.with_extension("yaml.tmp");
        std::fs::write(&temp_path, yaml)?;
        std::fs::rename(&temp_path, &path)?;

        Ok(())
    }

    fn partition_key(&self, manifest: &DatasetManifest, record: &ReferenceRecord) -> String {
        match &manifest.partitioning {
            PartitionStrategy::Single => "all".to_string(),
            PartitionStrategy::Monthly => {
                if let Some(ts) = record.timestamp {
                    // Simple approximation: divide by ~30 days
                    let month = ts / (30 * 24 * 60 * 60);
                    format!("month-{:06}", month)
                } else {
                    "no-date".to_string()
                }
            }
            PartitionStrategy::Yearly => {
                if let Some(ts) = record.timestamp {
                    let year = 1970 + (ts / (365 * 24 * 60 * 60));
                    format!("{}", year)
                } else {
                    "no-date".to_string()
                }
            }
            PartitionStrategy::ByProject => record
                .fields
                .get("project")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string(),
            PartitionStrategy::Custom(field) => record
                .fields
                .get(field)
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string(),
        }
    }

    fn partition_path(&self, dataset_id: &str, partition_key: &str) -> PathBuf {
        self.records_dir(dataset_id)
            .join(format!("{}.yaml", partition_key))
    }

    fn load_partition(&self, path: &Path) -> Result<Vec<ReferenceRecord>> {
        if !path.exists() {
            return Ok(Vec::new());
        }

        let content = std::fs::read_to_string(path)?;
        let records: Vec<ReferenceRecord> =
            serde_yaml::from_str(&content).context("failed to parse partition YAML")?;

        Ok(records)
    }

    fn save_partition(&self, path: &Path, records: &[ReferenceRecord]) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let yaml = serde_yaml::to_string(records)?;

        // Atomic write
        let temp_path = path.with_extension("yaml.tmp");
        std::fs::write(&temp_path, yaml)?;
        std::fs::rename(&temp_path, path)?;

        Ok(())
    }

    fn load_all_records(&self, dataset_id: &str) -> Result<Vec<ReferenceRecord>> {
        let records_dir = self.records_dir(dataset_id);
        if !records_dir.exists() {
            return Ok(Vec::new());
        }

        let mut all_records = Vec::new();

        for entry in std::fs::read_dir(&records_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("yaml") {
                let records = self.load_partition(&path)?;
                all_records.extend(records);
            }
        }

        Ok(all_records)
    }

    fn search_records(
        &self,
        records: Vec<ReferenceRecord>,
        filters: &SearchFilters,
    ) -> Vec<ReferenceRecord> {
        records
            .into_iter()
            .filter(|rec| {
                // Kind filter
                if let Some(kind) = &filters.kind
                    && rec.source.kind != *kind
                {
                    return false;
                }

                // Tags filter
                if !filters.tags.is_empty() {
                    let has_all_tags = filters.tags.iter().all(|t| rec.tags.contains(t));
                    if !has_all_tags {
                        return false;
                    }
                }

                // Date range
                if let (Some(from), Some(ts)) = (filters.date_from, rec.timestamp)
                    && ts < from
                {
                    return false;
                }

                if let (Some(to), Some(ts)) = (filters.date_to, rec.timestamp)
                    && ts > to
                {
                    return false;
                }

                // Field filters
                for (key, expected) in &filters.field_filters {
                    if rec.fields.get(key) != Some(expected) {
                        return false;
                    }
                }

                // Text query (simple substring match in fields)
                if let Some(query) = &filters.text_query {
                    let query_lower = query.to_lowercase();
                    let matches = rec.fields.values().any(|v| {
                        if let Some(s) = v.as_str() {
                            s.to_lowercase().contains(&query_lower)
                        } else {
                            false
                        }
                    }) || rec
                        .tags
                        .iter()
                        .any(|t| t.to_lowercase().contains(&query_lower));

                    if !matches {
                        return false;
                    }
                }

                true
            })
            .collect()
    }

    fn rank_results(&self, records: Vec<ReferenceRecord>) -> Vec<SearchResult> {
        // Simple ranking: most recent first
        let mut results: Vec<SearchResult> = records
            .into_iter()
            .map(|record| {
                let score = record.timestamp.unwrap_or(0) as f64;
                SearchResult { record, score }
            })
            .collect();

        results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
        results
    }
}

#[async_trait]
impl ReferenceService for YamlReferenceService {
    async fn import(
        &self,
        dataset_id: &str,
        records: Vec<ReferenceRecord>,
    ) -> Result<ImportResult> {
        let manifest = self
            .load_manifest(dataset_id)?
            .ok_or_else(|| anyhow::anyhow!("dataset not found: {}", dataset_id))?;

        let mut result = ImportResult {
            imported: 0,
            updated: 0,
            skipped: 0,
        };

        // Group records by partition
        let mut partitions: HashMap<String, Vec<ReferenceRecord>> = HashMap::new();
        for record in records {
            let key = self.partition_key(&manifest, &record);
            partitions.entry(key).or_default().push(record);
        }

        // Process each partition
        for (partition_key, new_records) in partitions {
            let path = self.partition_path(dataset_id, &partition_key);
            let mut existing = self.load_partition(&path)?;

            for new_rec in new_records {
                if let Some(pos) = existing.iter().position(|r| r.id == new_rec.id) {
                    existing[pos] = new_rec;
                    result.updated += 1;
                } else {
                    existing.push(new_rec);
                    result.imported += 1;
                }
            }

            self.save_partition(&path, &existing)?;
        }

        // Update manifest timestamp
        let mut updated_manifest = manifest;
        updated_manifest.updated_at = now_unix();
        self.save_manifest(&updated_manifest)?;

        Ok(result)
    }

    async fn list_datasets(&self) -> Result<Vec<DatasetManifest>> {
        let mut datasets = Vec::new();

        if !self.root.exists() {
            return Ok(datasets);
        }

        for entry in std::fs::read_dir(&self.root)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                let dataset_id = entry.file_name().to_string_lossy().to_string();
                if let Some(manifest) = self.load_manifest(&dataset_id)? {
                    datasets.push(manifest);
                }
            }
        }

        Ok(datasets)
    }

    async fn get_dataset(&self, dataset_id: &str) -> Result<Option<DatasetManifest>> {
        self.load_manifest(dataset_id)
    }

    async fn search(&self, filters: SearchFilters, limit: usize) -> Result<Vec<SearchResult>> {
        let datasets = if let Some(dataset_id) = &filters.dataset_id {
            vec![dataset_id.clone()]
        } else {
            self.list_datasets()
                .await?
                .into_iter()
                .map(|m| m.id)
                .collect()
        };

        let mut all_records = Vec::new();
        for dataset_id in datasets {
            let records = self.load_all_records(&dataset_id)?;
            all_records.extend(records);
        }

        let filtered = self.search_records(all_records, &filters);
        let mut ranked = self.rank_results(filtered);

        ranked.truncate(limit);
        Ok(ranked)
    }

    async fn get(&self, dataset_id: &str, record_id: &str) -> Result<Option<ReferenceRecord>> {
        let records = self.load_all_records(dataset_id)?;
        Ok(records.into_iter().find(|r| r.id == record_id))
    }

    async fn upsert(&self, dataset_id: &str, record: ReferenceRecord) -> Result<()> {
        let manifest = self
            .load_manifest(dataset_id)?
            .ok_or_else(|| anyhow::anyhow!("dataset not found: {}", dataset_id))?;

        let partition_key = self.partition_key(&manifest, &record);
        let path = self.partition_path(dataset_id, &partition_key);
        let mut records = self.load_partition(&path)?;

        if let Some(pos) = records.iter().position(|r| r.id == record.id) {
            records[pos] = record;
        } else {
            records.push(record);
        }

        self.save_partition(&path, &records)?;

        // Update manifest timestamp
        let mut updated_manifest = manifest;
        updated_manifest.updated_at = now_unix();
        self.save_manifest(&updated_manifest)?;

        Ok(())
    }

    async fn delete(&self, dataset_id: &str, record_id: &str) -> Result<()> {
        let manifest = self
            .load_manifest(dataset_id)?
            .ok_or_else(|| anyhow::anyhow!("dataset not found: {}", dataset_id))?;

        let records_dir = self.records_dir(dataset_id);
        if !records_dir.exists() {
            return Ok(());
        }

        let mut found = false;
        for entry in std::fs::read_dir(&records_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("yaml") {
                let mut records = self.load_partition(&path)?;
                if let Some(pos) = records.iter().position(|r| r.id == record_id) {
                    records.remove(pos);
                    self.save_partition(&path, &records)?;
                    found = true;
                    break;
                }
            }
        }

        if found {
            let mut updated_manifest = manifest;
            updated_manifest.updated_at = now_unix();
            self.save_manifest(&updated_manifest)?;
        }

        Ok(())
    }

    async fn refresh(&self, dataset_id: &str) -> Result<()> {
        // For now, YAML is always authoritative, so refresh is a no-op
        // In the future, this could rebuild derived indexes
        let _ = self.load_manifest(dataset_id)?;
        Ok(())
    }

    async fn create_dataset(&self, manifest: DatasetManifest) -> Result<()> {
        let existing = self.load_manifest(&manifest.id)?;
        if existing.is_some() {
            bail!("dataset already exists: {}", manifest.id);
        }

        self.save_manifest(&manifest)?;
        std::fs::create_dir_all(self.records_dir(&manifest.id))?;

        Ok(())
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_service() -> (tempfile::TempDir, YamlReferenceService) {
        let dir = tempfile::tempdir().unwrap();
        let service = YamlReferenceService::new(dir.path()).unwrap();
        (dir, service)
    }

    fn test_manifest(id: &str) -> DatasetManifest {
        DatasetManifest {
            schema_version: 1,
            id: id.to_string(),
            kind: "test".to_string(),
            scope: DatasetScope::Workspace,
            sensitivity: Sensitivity::Private,
            partitioning: PartitionStrategy::Single,
            created_at: now_unix(),
            updated_at: now_unix(),
            metadata: HashMap::new(),
        }
    }

    fn test_record(id: &str) -> ReferenceRecord {
        ReferenceRecord {
            id: id.to_string(),
            timestamp: Some(now_unix()),
            source: RecordSource {
                kind: "test".to_string(),
                external_id: None,
            },
            fields: HashMap::new(),
            tags: vec![],
            provenance: RecordProvenance {
                imported_at: now_unix(),
                importer: "test".to_string(),
                source_file: None,
            },
        }
    }

    #[tokio::test]
    async fn create_and_list_datasets() {
        let (_dir, service) = temp_service();
        let manifest = test_manifest("test-dataset");

        service.create_dataset(manifest.clone()).await.unwrap();

        let datasets = service.list_datasets().await.unwrap();
        assert_eq!(datasets.len(), 1);
        assert_eq!(datasets[0].id, "test-dataset");
    }

    #[tokio::test]
    async fn import_and_search() {
        let (_dir, service) = temp_service();
        let manifest = test_manifest("test-dataset");
        service.create_dataset(manifest).await.unwrap();

        let records = vec![test_record("rec-1"), test_record("rec-2")];
        let result = service.import("test-dataset", records).await.unwrap();

        assert_eq!(result.imported, 2);
        assert_eq!(result.updated, 0);

        let filters = SearchFilters {
            dataset_id: Some("test-dataset".to_string()),
            ..Default::default()
        };

        let results = service.search(filters, 10).await.unwrap();
        assert_eq!(results.len(), 2);
    }

    #[tokio::test]
    async fn upsert_updates_existing() {
        let (_dir, service) = temp_service();
        let manifest = test_manifest("test-dataset");
        service.create_dataset(manifest).await.unwrap();

        let mut record = test_record("rec-1");
        service
            .upsert("test-dataset", record.clone())
            .await
            .unwrap();

        record.tags.push("updated".to_string());
        service.upsert("test-dataset", record).await.unwrap();

        let retrieved = service.get("test-dataset", "rec-1").await.unwrap().unwrap();
        assert!(retrieved.tags.contains(&"updated".to_string()));
    }

    #[tokio::test]
    async fn delete_record() {
        let (_dir, service) = temp_service();
        let manifest = test_manifest("test-dataset");
        service.create_dataset(manifest).await.unwrap();

        let record = test_record("rec-1");
        service.upsert("test-dataset", record).await.unwrap();

        service.delete("test-dataset", "rec-1").await.unwrap();

        let retrieved = service.get("test-dataset", "rec-1").await.unwrap();
        assert!(retrieved.is_none());
    }

    #[tokio::test]
    async fn monthly_partitioning() {
        let (_dir, service) = temp_service();
        let mut manifest = test_manifest("test-dataset");
        manifest.partitioning = PartitionStrategy::Monthly;
        service.create_dataset(manifest.clone()).await.unwrap();

        let mut rec1 = test_record("rec-1");
        rec1.timestamp = Some(1704067200); // 2024-01-01
        let mut rec2 = test_record("rec-2");
        rec2.timestamp = Some(1709251200); // 2024-03-01

        service
            .import("test-dataset", vec![rec1, rec2])
            .await
            .unwrap();

        // Check that different partition files exist
        let records_dir = service.records_dir("test-dataset");
        let entries: Vec<_> = std::fs::read_dir(&records_dir).unwrap().collect();
        assert!(entries.len() >= 2); // At least 2 partition files
    }
}
