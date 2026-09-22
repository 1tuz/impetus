//! Extension lifecycle trait (fixtures / trusted in-process only).
//!
//! Production hosts load declarative `instruction_pack` / `mcp_bridge` packages
//! and optional `host_process` binaries. This trait is for test fixtures and
//! trusted in-process adapters wrapped with `catch_unwind` by the host.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Health / status reported by an extension instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum ExtensionHealth {
    Healthy,
    Degraded { reason: String },
    Failed { reason: String },
}

/// Lifecycle / runtime failure for an extension instance.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ExtensionError {
    #[error("initialize failed: {0}")]
    Initialize(String),
    #[error("activate failed: {0}")]
    Activate(String),
    #[error("deactivate failed: {0}")]
    Deactivate(String),
    #[error("shutdown failed: {0}")]
    Shutdown(String),
    #[error("health check failed: {0}")]
    Health(String),
    #[error("configuration reload failed: {0}")]
    ReloadConfig(String),
    #[error("{0}")]
    Other(String),
}

/// Lifecycle handlers for an extension instance.
///
/// Host order: `initialize` → `activate` → operate → optional `reload_config`
/// → `deactivate` → `shutdown`.
pub trait Extension: Send {
    /// Allocate resources after manifest validation / permission grant.
    fn initialize(&mut self) -> Result<(), ExtensionError>;

    /// Start serving capabilities to the host registry.
    fn activate(&mut self) -> Result<(), ExtensionError>;

    /// Stop serving capabilities; keep durable state if any.
    fn deactivate(&mut self) -> Result<(), ExtensionError>;

    /// Release resources before unload.
    fn shutdown(&mut self) -> Result<(), ExtensionError>;

    /// Report current health.
    fn health(&self) -> Result<ExtensionHealth, ExtensionError>;

    /// Apply validated configuration after host schema check.
    fn reload_config(&mut self, config: &serde_json::Value) -> Result<(), ExtensionError>;
}
