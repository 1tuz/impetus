//! Reference store CLI example
//!
//! Demonstrates the complete vertical slice:
//! - Import Tempo CSV
//! - Search records
//! - Get specific record
//! - List datasets
//!
//! Usage:
//!   cargo run --example reference_store import tempo_export.csv
//!   cargo run --example reference_store search --query "OAuth" --limit 5
//!   cargo run --example reference_store list
//!   cargo run --example reference_store get tempo-123456

use anyhow::Result;
use impetus_core::{
    ReferenceSearchRequest, ReferenceTools, TempoImporter, TempoImporterConfig,
    YamlReferenceService,
};
use std::collections::HashMap;
use std::env;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        print_usage();
        return Ok(());
    }

    let command = &args[1];

    // Create store in temp directory for demo
    let temp_dir = std::env::temp_dir().join("impetus-reference-demo");
    std::fs::create_dir_all(&temp_dir)?;

    let store = Arc::new(YamlReferenceService::new(&temp_dir)?);
    let tools = ReferenceTools::new(Some(store.clone()));

    match command.as_str() {
        "import" => {
            if args.len() < 3 {
                eprintln!("Usage: reference_store import <csv_file>");
                return Ok(());
            }

            let csv_path = &args[2];
            let config = TempoImporterConfig::default();
            let importer = TempoImporter::new(store.clone(), config);

            println!("Importing from: {}", csv_path);
            let result = importer.import_csv(std::path::Path::new(csv_path)).await?;

            println!("✓ Import complete:");
            println!("  Imported: {}", result.imported);
            println!("  Updated:  {}", result.updated);
            println!("  Skipped:  {}", result.skipped);
        }

        "search" => {
            let mut query = None;
            let mut limit = 10;
            let mut dataset_id = Some("tempo-history".to_string());

            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--query" | "-q" => {
                        if i + 1 < args.len() {
                            query = Some(args[i + 1].clone());
                            i += 2;
                        } else {
                            i += 1;
                        }
                    }
                    "--limit" | "-l" => {
                        if i + 1 < args.len() {
                            limit = args[i + 1].parse().unwrap_or(10);
                            i += 2;
                        } else {
                            i += 1;
                        }
                    }
                    "--dataset" | "-d" if i + 1 < args.len() => {
                        dataset_id = Some(args[i + 1].clone());
                        i += 2;
                    }
                    _ => {
                        i += 1;
                    }
                }
            }

            let request = ReferenceSearchRequest {
                dataset_id,
                kind: None,
                tags: vec![],
                text_query: query.clone(),
                date_from: None,
                date_to: None,
                field_filters: HashMap::new(),
                limit,
            };

            println!(
                "Searching for: {:?}",
                query.unwrap_or_else(|| "all".to_string())
            );
            let response = tools.search(request).await?;

            println!("\n✓ Found {} results:", response.results.len());
            for (idx, result) in response.results.iter().enumerate() {
                let rec = &result.record;
                println!("\n{}. {} (score: {:.2})", idx + 1, rec.id, result.score);

                if let Some(issue) = rec.fields.get("issue_key") {
                    println!("   Issue: {}", issue);
                }
                if let Some(desc) = rec.fields.get("description") {
                    println!("   Description: {}", desc);
                }
                if let Some(hours) = rec.fields.get("hours") {
                    println!("   Hours: {}", hours);
                }
                println!("   Tags: {}", rec.tags.join(", "));
            }
        }

        "get" => {
            if args.len() < 3 {
                eprintln!("Usage: reference_store get <record_id>");
                return Ok(());
            }

            let record_id = &args[2];
            let dataset_id = if args.len() > 3 {
                args[3].clone()
            } else {
                "tempo-history".to_string()
            };

            println!("Getting record: {} from {}", record_id, dataset_id);

            let request = impetus_core::ReferenceGetRequest {
                dataset_id,
                record_id: record_id.clone(),
            };

            let response = tools.get(request).await?;

            if let Some(rec) = response.record {
                println!("\n✓ Record found:");
                println!("  ID: {}", rec.id);
                println!("  Source: {:?}", rec.source);
                println!("  Timestamp: {:?}", rec.timestamp);
                println!("  Fields:");
                for (key, value) in &rec.fields {
                    println!("    {}: {}", key, value);
                }
                println!("  Tags: {}", rec.tags.join(", "));
                println!("  Provenance:");
                println!("    Imported at: {}", rec.provenance.imported_at);
                println!("    Importer: {}", rec.provenance.importer);
                if let Some(src) = &rec.provenance.source_file {
                    println!("    Source file: {}", src);
                }
            } else {
                println!("✗ Record not found");
            }
        }

        "list" => {
            println!("Listing datasets...");
            let response = tools.list_datasets().await?;

            if response.datasets.is_empty() {
                println!("No datasets found. Import some data first.");
            } else {
                println!("\n✓ Found {} dataset(s):", response.datasets.len());
                for dataset in &response.datasets {
                    println!("\n- {} ({})", dataset.id, dataset.kind);
                    println!("  Scope: {:?}", dataset.scope);
                    println!("  Sensitivity: {:?}", dataset.sensitivity);
                    println!("  Partitioning: {:?}", dataset.partitioning);
                    println!("  Created: {}", dataset.created_at);
                    println!("  Updated: {}", dataset.updated_at);
                }
            }
        }

        _ => {
            print_usage();
        }
    }

    println!("\nStore location: {}", temp_dir.display());
    Ok(())
}

fn print_usage() {
    println!("Reference Store CLI Demo");
    println!();
    println!("Usage:");
    println!("  reference_store import <csv_file>");
    println!("  reference_store search [--query <text>] [--limit <n>] [--dataset <id>]");
    println!("  reference_store get <record_id> [dataset_id]");
    println!("  reference_store list");
    println!();
    println!("Examples:");
    println!("  reference_store import tempo_export.csv");
    println!("  reference_store search --query OAuth --limit 5");
    println!("  reference_store get tempo-123456");
    println!("  reference_store list");
}
