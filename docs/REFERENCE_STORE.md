# Reference Store

YAML-based persistent storage for long-term agent reference data.

## Overview

The Reference Store provides a simple, human-readable way to store and retrieve reference datasets that the agent can use as context without loading entire datasets into prompts.

**Key features:**
- YAML as authoritative storage (human-readable, version-control friendly)
- Partitioned storage (shard by month/year/project to avoid giant files)
- Lazy loading (only top-K relevant records reach the model)
- Privacy-aware (public/internal/private sensitivity levels)
- Atomic writes (temp file + rename pattern)
- Schema versioning for migrations

## Architecture

```
$IMPETUS_DATA_DIR/
  references/
    tempo-history/
      manifest.yaml
      records/
        2024-01.yaml
        2024-02.yaml
        2024-03.yaml
```

### Manifest

Each dataset has a `manifest.yaml`:

```yaml
schema_version: 1
id: tempo-history
kind: jira-tempo
scope: workspace
sensitivity: private
partitioning: monthly
created_at: 1704067200
updated_at: 1704067200
metadata: {}
```

### Records

Records are stored in partitioned YAML files:

```yaml
- id: tempo-123456
  timestamp: 1704067200
  source:
    kind: jira-tempo
    external_id: "123456"
  fields:
    project: IMP
    issue_key: IMP-42
    issue_summary: "Fix OAuth authorization"
    hours: 6
    description: "Fixed OAuth authorization flow"
    date: "2024-01-15"
  tags:
    - tempo
    - jira
    - imp
  provenance:
    imported_at: 1704067200
    importer: tempo-csv
    source_file: tempo_export_2024.csv
```

## Usage

### Service Integration

```rust
use impetus_core::{YamlReferenceService, ReferenceService, ServiceRegistry};
use std::sync::Arc;

// Create service
let store = Arc::new(YamlReferenceService::new("/path/to/data/references")?);

// Register in ServiceRegistry
let mut registry = ServiceRegistry::new();
registry.register_reference_store(store.clone());
```

### Tempo Import

```rust
use impetus_core::{TempoImporter, TempoImporterConfig};

let config = TempoImporterConfig {
    dataset_id: "tempo-history".to_string(),
    sensitivity: Sensitivity::Private,
    auto_tag: true,
};

let importer = TempoImporter::new(store.clone(), config);

// Import from CSV
let result = importer.import_csv("tempo_export.csv").await?;
println!("Imported: {}, Updated: {}", result.imported, result.updated);
```

### Search

```rust
use impetus_core::{ReferenceTools, ReferenceSearchRequest};
use std::collections::HashMap;

let tools = ReferenceTools::new(Some(store.clone()));

let request = ReferenceSearchRequest {
    dataset_id: Some("tempo-history".to_string()),
    kind: Some("jira-tempo".to_string()),
    tags: vec!["imp".to_string()],
    text_query: Some("OAuth".to_string()),
    date_from: None,
    date_to: None,
    field_filters: HashMap::new(),
    limit: 10,
};

let response = tools.search(request).await?;

for result in response.results {
    println!("Record: {:?}", result.record);
    println!("Score: {}", result.score);
}
```

### Get Record

```rust
use impetus_core::ReferenceGetRequest;

let request = ReferenceGetRequest {
    dataset_id: "tempo-history".to_string(),
    record_id: "tempo-123456".to_string(),
};

let response = tools.get(request).await?;
if let Some(record) = response.record {
    println!("Found: {:?}", record);
}
```

### List Datasets

```rust
let response = tools.list_datasets().await?;

for dataset in response.datasets {
    println!("Dataset: {} ({})", dataset.id, dataset.kind);
    println!("  Sensitivity: {:?}", dataset.sensitivity);
    println!("  Partitioning: {:?}", dataset.partitioning);
}
```

## Tempo CSV Format

Expected Tempo CSV export columns:

- `Issue key` (e.g., "IMP-42")
- `Issue summary`
- `Date` (YYYY-MM-DD format)
- `Logged time (h)` (decimal hours)
- `Work description`
- `Project key` (optional)
- `Work author` (optional)
- `Work ID` (optional)

Example CSV:

```csv
Issue key,Issue summary,Date,Logged time (h),Work description,Project key,Work author,Work ID
IMP-42,Fix OAuth,2024-01-15,6,Fixed OAuth authorization,IMP,john.doe,123456
IMP-43,Add tests,2024-01-16,4,Added unit tests for OAuth,IMP,john.doe,123457
```

## Agent Integration

The Reference Store integrates with the agent loop through `ReferenceTools`:

1. Agent receives a task (e.g., "Log 6 hours to IMP-42 for OAuth work")
2. Agent searches reference store for similar past worklogs
3. Agent receives top-K matching records as context
4. Agent uses past examples to format new worklog

### Example Agent Flow

```rust
// Agent needs to log time to Jira
let similar = importer
    .find_similar(
        Some("IMP"),           // project
        Some("IMP"),           // issue prefix
        &["OAuth".to_string()], // keywords
        5                      // limit
    )
    .await?;

// Agent receives similar records as context:
// - IMP-42: "Fixed OAuth authorization" (6h)
// - IMP-43: "Added unit tests for OAuth" (4h)
// - IMP-38: "OAuth integration with backend" (8h)

// Agent uses these examples to format new worklog
```

## Privacy

Datasets have sensitivity levels:

- **Public**: Can be shared externally
- **Internal**: Organization-only
- **Private**: User-only, not sent to cloud models by default

The agent respects sensitivity when escalating to cloud models:

```rust
match dataset.sensitivity {
    Sensitivity::Private => {
        // Only use with local models or sanitized cloud requests
    }
    Sensitivity::Internal => {
        // Organization-approved cloud models only
    }
    Sensitivity::Public => {
        // Any model
    }
}
```

## Schema Versioning

Each dataset has a `schema_version` field. Future migrations can transform old formats:

```rust
match manifest.schema_version {
    1 => { /* current format */ }
    2 => { /* future format with new fields */ }
    _ => bail!("unsupported schema version"),
}
```

## Atomic Writes

All YAML writes use atomic pattern:

```rust
let temp_path = path.with_extension("yaml.tmp");
std::fs::write(&temp_path, yaml)?;
std::fs::rename(&temp_path, &path)?;  // Atomic on POSIX
```

This ensures no half-written YAML files after crashes.

## Partitioning Strategies

- **Single**: All records in one file (small datasets)
- **Monthly**: Partition by month (time-series data like worklogs)
- **Yearly**: Partition by year (long-term historical data)
- **ByProject**: Partition by project field
- **Custom(field)**: Partition by arbitrary field

## Testing

Run tests:

```bash
cargo test --package impetus-core --lib reference_store
cargo test --package impetus-core --lib reference_tools
cargo test --package impetus-core --lib tempo_importer
```

## Future Enhancements

Not in initial implementation (YAGNI):

- Vector DB / embeddings
- Semantic search
- Derived indexes (may be added later, but YAML remains authoritative)
- Real-time synchronization
- Multi-user collaboration
- Marketplace / sharing

The Reference Store is intentionally simple: YAML files + search filters + lazy loading.
