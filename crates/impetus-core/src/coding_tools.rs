//! Typed coding-tool capability seam (TODO P1 §11 / #261 / #282).
//!
//! Harness requests language intelligence (definition, references, diagnostics,
//! symbols, hover) through [`CodingToolsProvider`] — a replaceable backend.
//! Runtime does **not** compile against or require a single LSP binary path
//! (`rust-analyzer`, `clangd`, …). Optional [`crate::LspBackendModule`] carries a
//! runtime path hint only; Absent/Mock remain the default. Real process spawn
//! stays Planned (not a core dep).
//!
//! Payloads are path/range/label only — no secrets, tokens, or raw credentials.
//!
//! Out of scope: full LSP protocol completeness, IDE UI, language installers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Reason when no coding-tools backend is registered.
pub const ABSENT_CODING_TOOLS_REASON: &str =
    "no coding-tools provider registered (optional; no LSP binary required)";

pub use impetus_protocol::{HoverInfo, SourceLocation, SourcePosition, SourceRange};

/// Query at a point in a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PositionQuery {
    pub path: PathBuf,
    pub position: SourcePosition,
}

impl PositionQuery {
    pub fn new(path: impl Into<PathBuf>, line: u32, character: u32) -> Self {
        Self {
            path: path.into(),
            position: SourcePosition { line, character },
        }
    }
}

/// Diagnostic severity (labels only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Information,
    Hint,
}

/// One diagnostic for a path/range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodingDiagnostic {
    pub path: PathBuf,
    pub range: SourceRange,
    pub severity: DiagnosticSeverity,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

/// Symbol kind (coarse; not full LSP enum).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    File,
    Module,
    Namespace,
    Class,
    Method,
    Function,
    Variable,
    Constant,
    Field,
    Enum,
    Interface,
    Struct,
    TypeParameter,
    Other,
}

/// Document / workspace symbol entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentSymbol {
    pub name: String,
    pub kind: SymbolKind,
    pub location: SourceLocation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_name: Option<String>,
}

/// Failures from the coding-tools seam (fail-closed when backend absent).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CodingToolsError {
    #[error("{0}")]
    Unavailable(String),
    #[error("coding-tools provider error: {0}")]
    Provider(String),
}

impl CodingToolsError {
    pub fn absent() -> Self {
        Self::Unavailable(ABSENT_CODING_TOOLS_REASON.into())
    }

    pub fn is_unavailable(&self) -> bool {
        matches!(self, Self::Unavailable(_))
    }
}

/// Optional language-intelligence backend. Implementations may wrap an LSP
/// process later; core never hard-codes a binary path.
#[async_trait]
pub trait CodingToolsProvider: Send + Sync {
    async fn definition(
        &self,
        query: &PositionQuery,
    ) -> Result<Vec<SourceLocation>, CodingToolsError>;

    async fn references(
        &self,
        query: &PositionQuery,
    ) -> Result<Vec<SourceLocation>, CodingToolsError>;

    async fn diagnostics(&self, path: &Path) -> Result<Vec<CodingDiagnostic>, CodingToolsError>;

    async fn symbols(&self, path: &Path) -> Result<Vec<DocumentSymbol>, CodingToolsError>;

    async fn hover(&self, query: &PositionQuery) -> Result<Option<HoverInfo>, CodingToolsError>;
}

/// Facade seen by harness callers — never a concrete LSP binary.
#[async_trait]
pub trait CodingToolsService: Send + Sync {
    async fn definition(
        &self,
        query: &PositionQuery,
    ) -> Result<Vec<SourceLocation>, CodingToolsError>;

    async fn references(
        &self,
        query: &PositionQuery,
    ) -> Result<Vec<SourceLocation>, CodingToolsError>;

    async fn diagnostics(&self, path: &Path) -> Result<Vec<CodingDiagnostic>, CodingToolsError>;

    async fn symbols(&self, path: &Path) -> Result<Vec<DocumentSymbol>, CodingToolsError>;

    async fn hover(&self, query: &PositionQuery) -> Result<Option<HoverInfo>, CodingToolsError>;
}

/// Thin adapter: registered provider → service surface.
pub struct ProviderBackedCodingToolsService {
    provider: Arc<dyn CodingToolsProvider>,
}

impl ProviderBackedCodingToolsService {
    pub fn new(provider: Arc<dyn CodingToolsProvider>) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl CodingToolsService for ProviderBackedCodingToolsService {
    async fn definition(
        &self,
        query: &PositionQuery,
    ) -> Result<Vec<SourceLocation>, CodingToolsError> {
        self.provider.definition(query).await
    }

    async fn references(
        &self,
        query: &PositionQuery,
    ) -> Result<Vec<SourceLocation>, CodingToolsError> {
        self.provider.references(query).await
    }

    async fn diagnostics(&self, path: &Path) -> Result<Vec<CodingDiagnostic>, CodingToolsError> {
        self.provider.diagnostics(path).await
    }

    async fn symbols(&self, path: &Path) -> Result<Vec<DocumentSymbol>, CodingToolsError> {
        self.provider.symbols(path).await
    }

    async fn hover(&self, query: &PositionQuery) -> Result<Option<HoverInfo>, CodingToolsError> {
        self.provider.hover(query).await
    }
}

/// Default production surface when no optional provider is registered.
/// Fail-closed: every query returns [`CodingToolsError::Unavailable`].
pub struct AbsentCodingToolsService;

#[async_trait]
impl CodingToolsService for AbsentCodingToolsService {
    async fn definition(
        &self,
        _query: &PositionQuery,
    ) -> Result<Vec<SourceLocation>, CodingToolsError> {
        Err(CodingToolsError::absent())
    }

    async fn references(
        &self,
        _query: &PositionQuery,
    ) -> Result<Vec<SourceLocation>, CodingToolsError> {
        Err(CodingToolsError::absent())
    }

    async fn diagnostics(&self, _path: &Path) -> Result<Vec<CodingDiagnostic>, CodingToolsError> {
        Err(CodingToolsError::absent())
    }

    async fn symbols(&self, _path: &Path) -> Result<Vec<DocumentSymbol>, CodingToolsError> {
        Err(CodingToolsError::absent())
    }

    async fn hover(&self, _query: &PositionQuery) -> Result<Option<HoverInfo>, CodingToolsError> {
        Err(CodingToolsError::absent())
    }
}

/// Optional slot: `None` → fail-closed absent; `Some` → delegated provider.
/// Proves runtime can run without any LSP binary registered.
pub struct OptionalCodingToolsService {
    provider: Option<Arc<dyn CodingToolsProvider>>,
}

impl OptionalCodingToolsService {
    pub fn absent() -> Self {
        Self { provider: None }
    }

    pub fn with_provider(provider: Arc<dyn CodingToolsProvider>) -> Self {
        Self {
            provider: Some(provider),
        }
    }

    pub fn has_provider(&self) -> bool {
        self.provider.is_some()
    }
}

#[async_trait]
impl CodingToolsService for OptionalCodingToolsService {
    async fn definition(
        &self,
        query: &PositionQuery,
    ) -> Result<Vec<SourceLocation>, CodingToolsError> {
        match &self.provider {
            Some(p) => p.definition(query).await,
            None => Err(CodingToolsError::absent()),
        }
    }

    async fn references(
        &self,
        query: &PositionQuery,
    ) -> Result<Vec<SourceLocation>, CodingToolsError> {
        match &self.provider {
            Some(p) => p.references(query).await,
            None => Err(CodingToolsError::absent()),
        }
    }

    async fn diagnostics(&self, path: &Path) -> Result<Vec<CodingDiagnostic>, CodingToolsError> {
        match &self.provider {
            Some(p) => p.diagnostics(path).await,
            None => Err(CodingToolsError::absent()),
        }
    }

    async fn symbols(&self, path: &Path) -> Result<Vec<DocumentSymbol>, CodingToolsError> {
        match &self.provider {
            Some(p) => p.symbols(path).await,
            None => Err(CodingToolsError::absent()),
        }
    }

    async fn hover(&self, query: &PositionQuery) -> Result<Option<HoverInfo>, CodingToolsError> {
        match &self.provider {
            Some(p) => p.hover(query).await,
            None => Err(CodingToolsError::absent()),
        }
    }
}

/// In-memory mock for unit tests — no process, no binary, no network.
#[derive(Debug, Default, Clone)]
pub struct MockCodingToolsProvider {
    definitions: HashMap<(PathBuf, SourcePosition), Vec<SourceLocation>>,
    references: HashMap<(PathBuf, SourcePosition), Vec<SourceLocation>>,
    diagnostics: HashMap<PathBuf, Vec<CodingDiagnostic>>,
    symbols: HashMap<PathBuf, Vec<DocumentSymbol>>,
    hovers: HashMap<(PathBuf, SourcePosition), HoverInfo>,
}

impl MockCodingToolsProvider {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_definition(mut self, query: PositionQuery, locations: Vec<SourceLocation>) -> Self {
        self.definitions
            .insert((query.path, query.position), locations);
        self
    }

    pub fn with_references(mut self, query: PositionQuery, locations: Vec<SourceLocation>) -> Self {
        self.references
            .insert((query.path, query.position), locations);
        self
    }

    pub fn with_diagnostics(
        mut self,
        path: impl Into<PathBuf>,
        items: Vec<CodingDiagnostic>,
    ) -> Self {
        self.diagnostics.insert(path.into(), items);
        self
    }

    pub fn with_symbols(mut self, path: impl Into<PathBuf>, items: Vec<DocumentSymbol>) -> Self {
        self.symbols.insert(path.into(), items);
        self
    }

    pub fn with_hover(mut self, query: PositionQuery, hover: HoverInfo) -> Self {
        self.hovers.insert((query.path, query.position), hover);
        self
    }
}

#[async_trait]
impl CodingToolsProvider for MockCodingToolsProvider {
    async fn definition(
        &self,
        query: &PositionQuery,
    ) -> Result<Vec<SourceLocation>, CodingToolsError> {
        Ok(self
            .definitions
            .get(&(query.path.clone(), query.position))
            .cloned()
            .unwrap_or_default())
    }

    async fn references(
        &self,
        query: &PositionQuery,
    ) -> Result<Vec<SourceLocation>, CodingToolsError> {
        Ok(self
            .references
            .get(&(query.path.clone(), query.position))
            .cloned()
            .unwrap_or_default())
    }

    async fn diagnostics(&self, path: &Path) -> Result<Vec<CodingDiagnostic>, CodingToolsError> {
        Ok(self.diagnostics.get(path).cloned().unwrap_or_default())
    }

    async fn symbols(&self, path: &Path) -> Result<Vec<DocumentSymbol>, CodingToolsError> {
        Ok(self.symbols.get(path).cloned().unwrap_or_default())
    }

    async fn hover(&self, query: &PositionQuery) -> Result<Option<HoverInfo>, CodingToolsError> {
        Ok(self
            .hovers
            .get(&(query.path.clone(), query.position))
            .cloned())
    }
}

/// Run a coding-tools future on an isolated current-thread runtime.
///
/// Used by sync IPC harness dispatch; agent-loop path calls the async API directly.
pub fn block_on_coding_tools<T, F>(fut: F) -> T
where
    T: Send + 'static,
    F: std::future::Future<Output = T> + Send + 'static,
{
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("coding-tools runtime")
            .block_on(fut)
    })
    .join()
    .unwrap_or_else(|_| panic!("coding-tools worker panicked"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_mock() -> MockCodingToolsProvider {
        let path = PathBuf::from("src/lib.rs");
        let def_range = SourceRange::new(10, 0, 10, 3);
        let def_loc = SourceLocation::new(&path, def_range);
        let query = PositionQuery::new(&path, 42, 5);

        MockCodingToolsProvider::new()
            .with_definition(query.clone(), vec![def_loc.clone()])
            .with_references(
                query.clone(),
                vec![
                    def_loc.clone(),
                    SourceLocation::new("src/main.rs", SourceRange::new(1, 4, 1, 7)),
                ],
            )
            .with_diagnostics(
                &path,
                vec![CodingDiagnostic {
                    path: path.clone(),
                    range: SourceRange::new(42, 0, 42, 10),
                    severity: DiagnosticSeverity::Warning,
                    message: "unused variable `x`".into(),
                    code: Some("unused_variables".into()),
                }],
            )
            .with_symbols(
                &path,
                vec![DocumentSymbol {
                    name: "run".into(),
                    kind: SymbolKind::Function,
                    location: SourceLocation::new(&path, SourceRange::new(40, 0, 50, 1)),
                    container_name: None,
                }],
            )
            .with_hover(
                query,
                HoverInfo {
                    contents: "fn run()".into(),
                    range: Some(SourceRange::new(42, 3, 42, 6)),
                },
            )
    }

    #[tokio::test]
    async fn mock_provider_returns_seeded_payloads() {
        let provider = Arc::new(sample_mock());
        let service = ProviderBackedCodingToolsService::new(provider);
        let path = PathBuf::from("src/lib.rs");
        let query = PositionQuery::new(&path, 42, 5);

        let defs = service.definition(&query).await.expect("definition");
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].path, path);
        assert_eq!(defs[0].range, SourceRange::new(10, 0, 10, 3));

        let refs = service.references(&query).await.expect("references");
        assert_eq!(refs.len(), 2);

        let diags = service.diagnostics(&path).await.expect("diagnostics");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, DiagnosticSeverity::Warning);
        assert_eq!(diags[0].code.as_deref(), Some("unused_variables"));

        let syms = service.symbols(&path).await.expect("symbols");
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name, "run");
        assert_eq!(syms[0].kind, SymbolKind::Function);

        let hover = service.hover(&query).await.expect("hover");
        assert_eq!(
            hover.as_ref().map(|h| h.contents.as_str()),
            Some("fn run()")
        );
    }

    #[tokio::test]
    async fn missing_provider_fail_closed() {
        let absent = AbsentCodingToolsService;
        let query = PositionQuery::new("src/lib.rs", 0, 0);
        let path = Path::new("src/lib.rs");

        let err = absent.definition(&query).await.expect_err("definition");
        assert!(err.is_unavailable());
        assert_eq!(err, CodingToolsError::absent());

        assert!(
            absent
                .references(&query)
                .await
                .unwrap_err()
                .is_unavailable()
        );
        assert!(absent.diagnostics(path).await.unwrap_err().is_unavailable());
        assert!(absent.symbols(path).await.unwrap_err().is_unavailable());
        assert!(absent.hover(&query).await.unwrap_err().is_unavailable());
    }

    #[tokio::test]
    async fn optional_slot_absent_fail_closed_without_lsp_binary() {
        let service = OptionalCodingToolsService::absent();
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
    async fn optional_slot_with_mock_delegates() {
        let mock = Arc::new(MockCodingToolsProvider::new().with_hover(
            PositionQuery::new("a.rs", 0, 0),
            HoverInfo {
                contents: "i32".into(),
                range: None,
            },
        ));
        let service = OptionalCodingToolsService::with_provider(mock);
        assert!(service.has_provider());
        let hover = service
            .hover(&PositionQuery::new("a.rs", 0, 0))
            .await
            .expect("hover");
        assert_eq!(hover.unwrap().contents, "i32");
    }
}
