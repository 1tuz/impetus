//! Optional LSP backend identity seam (rust-analyzer / clangd / …) (#282 / #336).
//!
//! Harness talks through existing [`CodingToolsProvider`] / [`CodingToolsService`]:
//! definition / references / diagnostics / symbols / hover / cancel. This module
//! is the **identity + launch-hint** slot only — no rust-analyzer / clangd crates
//! as required deps, and **no compile-time LSP binary path**. It never launches
//! an LSP process (see [`crate::ProcessLspBackend`] for generic stdio client).
//!
//! Extension-first (#336): concrete language packs belong under extension
//! `LspIntegration` (+ permission `lsp`). Browser CDP/WebDriver stays
//! `BrowserIntegration` / Parked — not this seam.
//!
//! Out of scope: full LSP protocol completeness, multi-language installers,
//! TUI IDE UI (#283 vendor-parity docs), CDP/WebDriver.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::coding_tools::{
    CodingDiagnostic, CodingToolsError, CodingToolsProvider, DocumentSymbol, HoverInfo,
    PositionQuery, SourceLocation,
};

/// Reason when a labeled LSP backend module has no process spawn yet (seam only).
pub const LSP_BACKEND_NOT_IMPLEMENTED: &str = "optional LSP backend spawn not implemented (seam only; no rust-analyzer/clangd binary required in core)";

/// Which replaceable LSP family this slot refers to (identity only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LspBackendFamily {
    RustAnalyzer,
    Clangd,
}

impl LspBackendFamily {
    pub fn id(self) -> &'static str {
        match self {
            Self::RustAnalyzer => "rust-analyzer",
            Self::Clangd => "clangd",
        }
    }

    pub fn family_label(self) -> &'static str {
        self.id()
    }
}

/// Optional runtime launch hint — never a compile-time binary path.
///
/// Callers may pass a discovered path at process start; core does not embed
/// `rust-analyzer`, Homebrew prefixes, or similar constants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspBackendLaunchHint {
    /// Runtime-only absolute or relative path label. Absent = discover later.
    pub binary_path: Option<PathBuf>,
}

impl LspBackendLaunchHint {
    pub fn none() -> Self {
        Self { binary_path: None }
    }

    pub fn with_runtime_binary(path: impl Into<PathBuf>) -> Self {
        Self {
            binary_path: Some(path.into()),
        }
    }
}

/// Result of a mock handshake — no OS process, no stdio, no network.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspBackendHandshake {
    pub family_id: String,
    /// Seam advertises itself as negotiable without spawning.
    pub ready: bool,
    /// Real process spawn is not wired in this module.
    pub spawn_implemented: bool,
    pub binary_path: Option<PathBuf>,
    pub reason: String,
}

/// rust-analyzer / clangd module stub: holds family + optional runtime path
/// hint; handshake succeeds without spawning; coding-tools queries fail-closed
/// until a real LSP process adapter is registered later (not a core dep).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspBackendModule {
    family: LspBackendFamily,
    launch: LspBackendLaunchHint,
}

impl LspBackendModule {
    pub fn rust_analyzer(launch: LspBackendLaunchHint) -> Self {
        Self {
            family: LspBackendFamily::RustAnalyzer,
            launch,
        }
    }

    pub fn clangd(launch: LspBackendLaunchHint) -> Self {
        Self {
            family: LspBackendFamily::Clangd,
            launch,
        }
    }

    pub fn family(&self) -> LspBackendFamily {
        self.family
    }

    /// Runtime path hint only — never treated as a secret; never launched here.
    pub fn launch_hint(&self) -> &LspBackendLaunchHint {
        &self.launch
    }

    /// Mock handshake OK: advertise family + hint without spawning a process.
    pub fn handshake(&self) -> LspBackendHandshake {
        LspBackendHandshake {
            family_id: self.family.id().into(),
            ready: true,
            spawn_implemented: false,
            binary_path: self.launch.binary_path.clone(),
            reason: LSP_BACKEND_NOT_IMPLEMENTED.into(),
        }
    }

    fn unavailable_err(&self) -> CodingToolsError {
        CodingToolsError::Unavailable(LSP_BACKEND_NOT_IMPLEMENTED.into())
    }
}

#[async_trait]
impl CodingToolsProvider for LspBackendModule {
    async fn definition(
        &self,
        _query: &PositionQuery,
    ) -> Result<Vec<SourceLocation>, CodingToolsError> {
        Err(self.unavailable_err())
    }

    async fn references(
        &self,
        _query: &PositionQuery,
    ) -> Result<Vec<SourceLocation>, CodingToolsError> {
        Err(self.unavailable_err())
    }

    async fn diagnostics(&self, _path: &Path) -> Result<Vec<CodingDiagnostic>, CodingToolsError> {
        Err(self.unavailable_err())
    }

    async fn symbols(&self, _path: &Path) -> Result<Vec<DocumentSymbol>, CodingToolsError> {
        Err(self.unavailable_err())
    }

    async fn hover(&self, _query: &PositionQuery) -> Result<Option<HoverInfo>, CodingToolsError> {
        Err(self.unavailable_err())
    }
}

/// Optional slot helper: prove `LspBackendModule` wires through
/// [`CodingToolsService`] with Absent default fail-closed.
pub fn optional_coding_tools_with_lsp(
    module: Option<Arc<LspBackendModule>>,
) -> crate::OptionalCodingToolsService {
    match module {
        Some(m) => crate::OptionalCodingToolsService::with_provider(m),
        None => crate::OptionalCodingToolsService::absent(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coding_tools::{
        ABSENT_CODING_TOOLS_REASON, CodingToolsService, MockCodingToolsProvider,
        OptionalCodingToolsService,
    };

    #[test]
    fn families_expose_stable_ids() {
        assert_eq!(LspBackendFamily::RustAnalyzer.id(), "rust-analyzer");
        assert_eq!(LspBackendFamily::Clangd.id(), "clangd");
    }

    #[test]
    fn launch_hint_is_runtime_only_no_compile_time_path() {
        let none = LspBackendLaunchHint::none();
        assert!(none.binary_path.is_none());

        // Runtime label only — path is caller-supplied, never a core constant.
        let hinted = LspBackendLaunchHint::with_runtime_binary("/tmp/does-not-spawn-ra");
        assert_eq!(
            hinted.binary_path.as_deref(),
            Some(std::path::Path::new("/tmp/does-not-spawn-ra"))
        );
    }

    #[test]
    fn modules_expose_family_and_hint_without_launching() {
        let ra = LspBackendModule::rust_analyzer(LspBackendLaunchHint::none());
        assert_eq!(ra.family(), LspBackendFamily::RustAnalyzer);
        assert!(ra.launch_hint().binary_path.is_none());

        let clangd = LspBackendModule::clangd(LspBackendLaunchHint::with_runtime_binary(
            "/opt/runtime/clangd",
        ));
        assert_eq!(clangd.family(), LspBackendFamily::Clangd);
        assert_eq!(
            clangd.launch_hint().binary_path.as_deref(),
            Some(std::path::Path::new("/opt/runtime/clangd"))
        );
    }

    #[test]
    fn mock_handshake_ok_without_spawning_real_backend() {
        let module = LspBackendModule::rust_analyzer(LspBackendLaunchHint::with_runtime_binary(
            "/tmp/does-not-spawn-ra",
        ));
        let hs = module.handshake();
        assert!(hs.ready);
        assert!(!hs.spawn_implemented);
        assert_eq!(hs.family_id, "rust-analyzer");
        assert_eq!(
            hs.binary_path.as_deref(),
            Some(std::path::Path::new("/tmp/does-not-spawn-ra"))
        );
        assert_eq!(hs.reason, LSP_BACKEND_NOT_IMPLEMENTED);
    }

    #[tokio::test]
    async fn lsp_module_queries_fail_closed_without_spawn() {
        let module = LspBackendModule::rust_analyzer(LspBackendLaunchHint::none());
        let query = PositionQuery::new("src/lib.rs", 0, 0);
        let path = Path::new("src/lib.rs");

        let err = module.definition(&query).await.expect_err("definition");
        assert!(err.is_unavailable());
        assert_eq!(
            err,
            CodingToolsError::Unavailable(LSP_BACKEND_NOT_IMPLEMENTED.into())
        );
        assert!(
            module
                .references(&query)
                .await
                .unwrap_err()
                .is_unavailable()
        );
        assert!(module.diagnostics(path).await.unwrap_err().is_unavailable());
        assert!(module.symbols(path).await.unwrap_err().is_unavailable());
        assert!(module.hover(&query).await.unwrap_err().is_unavailable());
    }

    #[tokio::test]
    async fn optional_slot_wires_lsp_module_fail_closed() {
        let module = Arc::new(LspBackendModule::clangd(LspBackendLaunchHint::none()));
        let service = optional_coding_tools_with_lsp(Some(module));
        assert!(service.has_provider());
        let err = service
            .hover(&PositionQuery::new("a.rs", 0, 0))
            .await
            .expect_err("seam only");
        assert_eq!(
            err,
            CodingToolsError::Unavailable(LSP_BACKEND_NOT_IMPLEMENTED.into())
        );
    }

    #[tokio::test]
    async fn optional_slot_default_absent_still_fail_closed() {
        let service = optional_coding_tools_with_lsp(None);
        assert!(!service.has_provider());
        let err = service
            .definition(&PositionQuery::new("a.rs", 1, 0))
            .await
            .expect_err("no provider");
        assert_eq!(
            err,
            CodingToolsError::Unavailable(ABSENT_CODING_TOOLS_REASON.into())
        );
    }

    #[tokio::test]
    async fn mock_provider_still_usable_alongside_lsp_seam() {
        // Prove Absent/Mock remain default path; LSP module is opt-in only.
        let mock = Arc::new(MockCodingToolsProvider::new().with_hover(
            PositionQuery::new("a.rs", 0, 0),
            HoverInfo {
                contents: "i32".into(),
                range: None,
            },
        ));
        let service = OptionalCodingToolsService::with_provider(mock);
        let hover = service
            .hover(&PositionQuery::new("a.rs", 0, 0))
            .await
            .expect("hover");
        assert_eq!(hover.unwrap().contents, "i32");
    }
}
