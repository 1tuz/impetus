//! Jira Tempo worklog importer.
//!
//! Imports Tempo CSV exports into the reference store for use as agent context.

use crate::reference_store::{
    DatasetManifest, DatasetScope, ImportResult, PartitionStrategy, RecordProvenance, RecordSource,
    ReferenceRecord, ReferenceService, Sensitivity,
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Tempo worklog record from CSV export
#[derive(Debug, Clone, Deserialize)]
pub struct TempoWorklog {
    #[serde(rename = "Issue key")]
    pub issue_key: String,
    #[serde(rename = "Issue summary")]
    pub issue_summary: String,
    #[serde(rename = "Date")]
    pub date: String, // YYYY-MM-DD format
    #[serde(rename = "Logged time (h)")]
    pub hours: f64,
    #[serde(rename = "Work description")]
    pub description: String,
    #[serde(rename = "Project key", default)]
    pub project_key: String,
    #[serde(rename = "Work author", default)]
    pub author: String,
    #[serde(rename = "Work ID", default)]
    pub work_id: String,
}

/// Tempo importer configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TempoImporterConfig {
    pub dataset_id: String,
    pub sensitivity: Sensitivity,
    pub auto_tag: bool,
}

impl Default for TempoImporterConfig {
    fn default() -> Self {
        Self {
            dataset_id: "tempo-history".to_string(),
            sensitivity: Sensitivity::Private,
            auto_tag: true,
        }
    }
}

/// Tempo worklog importer
pub struct TempoImporter {
    store: Arc<dyn ReferenceService>,
    config: TempoImporterConfig,
}

impl TempoImporter {
    pub fn new(store: Arc<dyn ReferenceService>, config: TempoImporterConfig) -> Self {
        Self { store, config }
    }

    /// Import Tempo worklogs from CSV file
    pub async fn import_csv(&self, csv_path: &Path) -> Result<ImportResult> {
        let csv_content = std::fs::read_to_string(csv_path).context("failed to read CSV file")?;

        self.import_csv_content(&csv_content, Some(csv_path.to_string_lossy().to_string()))
            .await
    }

    /// Import Tempo worklogs from CSV content
    pub async fn import_csv_content(
        &self,
        csv_content: &str,
        source_file: Option<String>,
    ) -> Result<ImportResult> {
        // Ensure dataset exists
        if self
            .store
            .get_dataset(&self.config.dataset_id)
            .await?
            .is_none()
        {
            self.create_dataset().await?;
        }

        // Parse CSV
        let mut reader = csv::Reader::from_reader(csv_content.as_bytes());
        let mut worklogs = Vec::new();

        for result in reader.deserialize() {
            let worklog: TempoWorklog = result.context("failed to parse CSV row")?;
            worklogs.push(worklog);
        }

        // Convert to reference records
        let records: Vec<ReferenceRecord> = worklogs
            .into_iter()
            .map(|w| self.worklog_to_record(w, source_file.as_deref()))
            .collect::<Result<Vec<_>>>()?;

        // Import into store
        let result = self.store.import(&self.config.dataset_id, records).await?;

        Ok(result)
    }

    async fn create_dataset(&self) -> Result<()> {
        let manifest = DatasetManifest {
            schema_version: 1,
            id: self.config.dataset_id.clone(),
            kind: "jira-tempo".to_string(),
            scope: DatasetScope::Workspace,
            sensitivity: self.config.sensitivity,
            partitioning: PartitionStrategy::Monthly,
            created_at: now_unix(),
            updated_at: now_unix(),
            metadata: HashMap::new(),
        };

        self.store.create_dataset(manifest).await?;
        Ok(())
    }

    fn worklog_to_record(
        &self,
        worklog: TempoWorklog,
        source_file: Option<&str>,
    ) -> Result<ReferenceRecord> {
        // Use Work ID as record ID, fallback to generated ID
        let id = if worklog.work_id.is_empty() {
            format!("tempo-{}-{}", worklog.issue_key, worklog.date)
        } else {
            format!("tempo-{}", worklog.work_id)
        };

        // Parse date to timestamp
        let timestamp = parse_tempo_date(&worklog.date)?;

        // Build fields
        let mut fields = HashMap::new();
        fields.insert(
            "issue_key".to_string(),
            serde_json::json!(worklog.issue_key),
        );
        fields.insert(
            "issue_summary".to_string(),
            serde_json::json!(worklog.issue_summary),
        );
        fields.insert("date".to_string(), serde_json::json!(worklog.date));
        fields.insert("hours".to_string(), serde_json::json!(worklog.hours));
        fields.insert(
            "description".to_string(),
            serde_json::json!(worklog.description),
        );

        if !worklog.project_key.is_empty() {
            fields.insert(
                "project".to_string(),
                serde_json::json!(worklog.project_key),
            );
        }

        if !worklog.author.is_empty() {
            fields.insert("author".to_string(), serde_json::json!(worklog.author));
        }

        // Auto-generate tags
        let mut tags = vec!["tempo".to_string(), "jira".to_string()];

        if self.config.auto_tag {
            if !worklog.project_key.is_empty() {
                tags.push(worklog.project_key.to_lowercase());
            }

            // Extract issue prefix as tag (e.g., "IMP" from "IMP-42")
            if let Some(prefix) = worklog.issue_key.split('-').next()
                && !prefix.is_empty()
            {
                tags.push(prefix.to_lowercase());
            }
        }

        tags.sort();
        tags.dedup();

        Ok(ReferenceRecord {
            id,
            timestamp: Some(timestamp),
            source: RecordSource {
                kind: "jira-tempo".to_string(),
                external_id: Some(worklog.work_id),
            },
            fields,
            tags,
            provenance: RecordProvenance {
                imported_at: now_unix(),
                importer: "tempo-csv".to_string(),
                source_file: source_file.map(|s| s.to_string()),
            },
        })
    }

    /// Search for similar worklogs (for context/examples)
    pub async fn find_similar(
        &self,
        project: Option<&str>,
        issue_prefix: Option<&str>,
        keywords: &[String],
        limit: usize,
    ) -> Result<Vec<ReferenceRecord>> {
        let mut filters = crate::reference_store::SearchFilters {
            dataset_id: Some(self.config.dataset_id.clone()),
            kind: Some("jira-tempo".to_string()),
            ..Default::default()
        };

        // Filter by project
        if let Some(proj) = project {
            filters
                .field_filters
                .insert("project".to_string(), serde_json::json!(proj));
        }

        // Build text query from keywords
        if !keywords.is_empty() {
            filters.text_query = Some(keywords.join(" "));
        }

        // Add issue prefix as tag filter
        if let Some(prefix) = issue_prefix {
            filters.tags.push(prefix.to_lowercase());
        }

        let results = self.store.search(filters, limit).await?;

        Ok(results.into_iter().map(|r| r.record).collect())
    }
}

/// Parse Tempo date format (YYYY-MM-DD) to Unix timestamp
fn parse_tempo_date(date: &str) -> Result<u64> {
    let parts: Vec<&str> = date.split('-').collect();
    if parts.len() != 3 {
        bail!("invalid date format: expected YYYY-MM-DD, got {}", date);
    }

    let year: i32 = parts[0].parse().context("invalid year")?;
    let month: u32 = parts[1].parse().context("invalid month")?;
    let day: u32 = parts[2].parse().context("invalid day")?;

    if !(1..=12).contains(&month) {
        bail!("invalid month: {}", month);
    }
    if !(1..=31).contains(&day) {
        bail!("invalid day: {}", day);
    }

    // Simple conversion: days since epoch
    let years_since_1970 = year - 1970;
    let mut days = years_since_1970 as u64 * 365;
    days += ((years_since_1970 - 2) / 4) as u64; // Leap years (approximate)

    // Days in months (non-leap year approximation)
    let days_in_month = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    for &day_count in days_in_month.iter().take((month - 1) as usize) {
        days += day_count;
    }
    days += day as u64 - 1;

    Ok(days * 24 * 60 * 60)
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
    use crate::reference_store::YamlReferenceService;

    #[tokio::test]
    async fn test_import_tempo_csv() {
        let temp_dir = tempfile::tempdir().unwrap();
        let store = Arc::new(YamlReferenceService::new(temp_dir.path()).unwrap());

        let importer = TempoImporter::new(store.clone(), TempoImporterConfig::default());

        let csv_content = r#"Issue key,Issue summary,Date,Logged time (h),Work description,Project key,Work author,Work ID
IMP-42,Fix OAuth,2024-01-15,6,Fixed OAuth authorization,IMP,john.doe,123456
IMP-43,Add tests,2024-01-16,4,Added unit tests for OAuth,IMP,john.doe,123457"#;

        let result = importer
            .import_csv_content(csv_content, Some("test.csv".to_string()))
            .await
            .unwrap();

        assert_eq!(result.imported, 2);

        // Verify records exist
        let record = store
            .get("tempo-history", "tempo-123456")
            .await
            .unwrap()
            .unwrap();

        assert_eq!(record.fields["issue_key"], "IMP-42");
        assert_eq!(record.fields["hours"], 6.0);
        assert!(record.tags.contains(&"imp".to_string()));
    }

    #[tokio::test]
    async fn test_find_similar() {
        let temp_dir = tempfile::tempdir().unwrap();
        let store = Arc::new(YamlReferenceService::new(temp_dir.path()).unwrap());

        let importer = TempoImporter::new(store.clone(), TempoImporterConfig::default());

        let csv_content = r#"Issue key,Issue summary,Date,Logged time (h),Work description,Project key,Work author,Work ID
IMP-42,Fix OAuth,2024-01-15,6,Fixed OAuth authorization,IMP,john.doe,123456
IMP-43,Add tests,2024-01-16,4,Added unit tests for OAuth,IMP,john.doe,123457
OTHER-1,Unrelated,2024-01-17,2,Something else,OTHER,jane.doe,123458"#;

        importer
            .import_csv_content(csv_content, None)
            .await
            .unwrap();

        // Find IMP project worklogs
        let similar = importer
            .find_similar(Some("IMP"), None, &[], 10)
            .await
            .unwrap();

        assert_eq!(similar.len(), 2);
        assert!(similar.iter().all(|r| r.fields["project"] == "IMP"));

        // Find OAuth-related worklogs
        let oauth_logs = importer
            .find_similar(None, None, &["OAuth".to_string()], 10)
            .await
            .unwrap();

        assert_eq!(oauth_logs.len(), 2);
    }

    #[test]
    fn test_parse_tempo_date() {
        let ts = parse_tempo_date("2024-01-15").unwrap();
        assert!(ts > 0);

        // Check approximate value (2024-01-15 is roughly 19737 days since epoch)
        let expected_days = 19737u64;
        let actual_days = ts / (24 * 60 * 60);
        assert!((actual_days as i64 - expected_days as i64).abs() < 5); // Allow small error
    }

    #[test]
    fn test_parse_invalid_date() {
        assert!(parse_tempo_date("invalid").is_err());
        assert!(parse_tempo_date("2024-13-01").is_err()); // Invalid month
        assert!(parse_tempo_date("2024-01-32").is_err()); // Invalid day
    }
}
