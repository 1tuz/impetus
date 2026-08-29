//! Structured observations from tool execution.
//!
//! Observations are normalized, token-bounded representations of tool outputs
//! returned to the model. They provide structured metadata alongside content.

use crate::ci::PipelineStatus;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Test execution observation
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestObservation {
    pub command: String,
    pub exit_code: i32,
    pub passed: usize,
    pub failed: usize,
    pub duration_ms: u64,
    pub summary: String,
    pub failures: Vec<TestFailure>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_ref: Option<String>,
}

/// Individual test failure detail
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestFailure {
    pub test_name: String,
    pub message: String,
    pub location: Option<String>,
}

/// Diff observation from git or file comparison
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffObservation {
    pub source: DiffSource,
    pub files_changed: usize,
    pub insertions: usize,
    pub deletions: usize,
    pub summary: String,
    pub hunks: Vec<DiffHunk>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffSource {
    Git { commit_range: Option<String> },
    Files { before: PathBuf, after: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffHunk {
    pub file: PathBuf,
    pub old_start: usize,
    pub old_lines: usize,
    pub new_start: usize,
    pub new_lines: usize,
    pub preview: String,
}

/// Search observation from repository search
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchObservation {
    pub query: String,
    pub total_matches: usize,
    pub files_matched: usize,
    pub matches: Vec<SearchMatch>,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchMatch {
    pub file: PathBuf,
    pub line: usize,
    pub column: usize,
    pub content: String,
    pub context_before: Vec<String>,
    pub context_after: Vec<String>,
}

/// Pipeline observation from CI backends
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PipelineObservation {
    pub pipeline_id: String,
    pub status: PipelineStatus,
    pub jobs: Vec<PipelineJob>,
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PipelineJob {
    pub name: String,
    pub status: PipelineStatus,
    pub duration_ms: Option<u64>,
    pub failure_reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_observation_serialization() {
        let obs = TestObservation {
            command: "cargo test".to_string(),
            exit_code: 0,
            passed: 10,
            failed: 0,
            duration_ms: 1234,
            summary: "All tests passed".to_string(),
            failures: vec![],
            artifact_ref: None,
        };

        let json = serde_json::to_string(&obs).unwrap();
        let deserialized: TestObservation = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.passed, 10);
        assert_eq!(deserialized.exit_code, 0);
    }

    #[test]
    fn diff_observation_with_git_source() {
        let obs = DiffObservation {
            source: DiffSource::Git {
                commit_range: Some("HEAD~1..HEAD".to_string()),
            },
            files_changed: 3,
            insertions: 42,
            deletions: 15,
            summary: "3 files changed, 42 insertions(+), 15 deletions(-)".to_string(),
            hunks: vec![],
            artifact_ref: None,
        };

        let json = serde_json::to_string(&obs).unwrap();
        assert!(json.contains("git"));
    }

    #[test]
    fn search_observation_truncation() {
        let obs = SearchObservation {
            query: "TODO".to_string(),
            total_matches: 150,
            files_matched: 25,
            matches: vec![],
            truncated: true,
            artifact_ref: Some("artifact-123".to_string()),
        };

        assert!(obs.truncated);
        assert!(obs.artifact_ref.is_some());
    }

    #[test]
    fn pipeline_observation_with_failures() {
        let obs = PipelineObservation {
            pipeline_id: "pipeline-456".to_string(),
            status: PipelineStatus::Failed,
            jobs: vec![
                PipelineJob {
                    name: "test".to_string(),
                    status: PipelineStatus::Success,
                    duration_ms: Some(5000),
                    failure_reason: None,
                },
                PipelineJob {
                    name: "build".to_string(),
                    status: PipelineStatus::Failed,
                    duration_ms: Some(1200),
                    failure_reason: Some("compilation error".to_string()),
                },
            ],
            duration_ms: Some(6200),
            artifact_ref: None,
        };

        assert_eq!(obs.status, PipelineStatus::Failed);
        assert_eq!(obs.jobs.len(), 2);
    }

    #[test]
    fn test_failure_detail() {
        let failure = TestFailure {
            test_name: "test_validation".to_string(),
            message: "assertion failed: left == right".to_string(),
            location: Some("src/lib.rs:42".to_string()),
        };

        assert!(failure.location.is_some());
    }
}
