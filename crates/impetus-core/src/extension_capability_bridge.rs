//! Public host routing for declareable extension capability kinds (#362 / #363).
//!
//! Ownership boundary:
//! - **LSP:** core `ProcessLspBackend` = generic fallback; Active `LspIntegration`
//!   host_process preferred when present.
//! - **Browser:** core IPC stays Absent until Active `BrowserIntegration` answers
//!   `browser/health` / `browser/negotiate`.
//! - **Memory:** core `MemoryStore` IPC remains SoT for session durability;
//!   `MemoryProvider` operate (`memory/recall` / `memory/store`) is optional
//!   long-term — never a second session store.
//! - **Context:** `context/contribute` optional; skill roots stay SkillProvider.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use impetus_extension_sdk::{ExtensionCapabilityKind, ExtensionPermission, host_protocol::ops};
use impetus_protocol::{
    BrowserHealthStatus, BrowserNegotiateInfo, CodingDiagnostic, DocumentSymbol, HoverInfo,
    SourceLocation,
};
use serde_json::{Value, json};

use crate::coding_tools::{CodingToolsError, CodingToolsProvider, PositionQuery};
use crate::extension_host::ExtensionHost;

/// Prefer Active `LspIntegration` operate; else optional process/fallback backend.
pub struct PreferExtensionCodingTools {
    host: Arc<Mutex<ExtensionHost>>,
    fallback: Option<Arc<dyn CodingToolsProvider>>,
}

impl PreferExtensionCodingTools {
    pub fn new(
        host: Arc<Mutex<ExtensionHost>>,
        fallback: Option<Arc<dyn CodingToolsProvider>>,
    ) -> Self {
        Self { host, fallback }
    }

    fn operate_json(&self, op: &str, params: Value) -> Result<Option<Value>, CodingToolsError> {
        let mut guard = self
            .host
            .lock()
            .map_err(|_| CodingToolsError::Provider("extension host lock poisoned".into()))?;
        let Some(id) = guard
            .capability_registry()
            .active_host_for(ExtensionCapabilityKind::LspIntegration)
            .map(str::to_string)
        else {
            return Ok(None);
        };
        let request_id = next_request_id("coding");
        match guard.operate(
            &id,
            &request_id,
            op,
            params,
            Some(ExtensionPermission::Lsp),
            None,
        ) {
            Ok(result) => Ok(Some(result.data)),
            Err(err) => Err(CodingToolsError::Provider(err.to_string())),
        }
    }
}

#[async_trait]
impl CodingToolsProvider for PreferExtensionCodingTools {
    async fn definition(
        &self,
        query: &PositionQuery,
    ) -> Result<Vec<SourceLocation>, CodingToolsError> {
        if let Some(data) = self.operate_json(
            ops::CODING_DEFINITION,
            json!({
                "path": query.path,
                "line": query.position.line,
                "character": query.position.character,
            }),
        )? {
            return serde_json::from_value(data)
                .map_err(|e| CodingToolsError::Provider(e.to_string()));
        }
        match &self.fallback {
            Some(fb) => fb.definition(query).await,
            None => Err(CodingToolsError::absent()),
        }
    }

    async fn references(
        &self,
        query: &PositionQuery,
    ) -> Result<Vec<SourceLocation>, CodingToolsError> {
        if let Some(data) = self.operate_json(
            ops::CODING_REFERENCES,
            json!({
                "path": query.path,
                "line": query.position.line,
                "character": query.position.character,
            }),
        )? {
            return serde_json::from_value(data)
                .map_err(|e| CodingToolsError::Provider(e.to_string()));
        }
        match &self.fallback {
            Some(fb) => fb.references(query).await,
            None => Err(CodingToolsError::absent()),
        }
    }

    async fn diagnostics(&self, path: &Path) -> Result<Vec<CodingDiagnostic>, CodingToolsError> {
        if let Some(data) = self.operate_json(ops::CODING_DIAGNOSTICS, json!({ "path": path }))? {
            return serde_json::from_value(data)
                .map_err(|e| CodingToolsError::Provider(e.to_string()));
        }
        match &self.fallback {
            Some(fb) => fb.diagnostics(path).await,
            None => Err(CodingToolsError::absent()),
        }
    }

    async fn symbols(&self, path: &Path) -> Result<Vec<DocumentSymbol>, CodingToolsError> {
        if let Some(data) = self.operate_json(ops::CODING_SYMBOLS, json!({ "path": path }))? {
            return serde_json::from_value(data)
                .map_err(|e| CodingToolsError::Provider(e.to_string()));
        }
        match &self.fallback {
            Some(fb) => fb.symbols(path).await,
            None => Err(CodingToolsError::absent()),
        }
    }

    async fn hover(&self, query: &PositionQuery) -> Result<Option<HoverInfo>, CodingToolsError> {
        if let Some(data) = self.operate_json(
            ops::CODING_HOVER,
            json!({
                "path": query.path,
                "line": query.position.line,
                "character": query.position.character,
            }),
        )? {
            return serde_json::from_value(data)
                .map_err(|e| CodingToolsError::Provider(e.to_string()));
        }
        match &self.fallback {
            Some(fb) => fb.hover(query).await,
            None => Err(CodingToolsError::absent()),
        }
    }

    async fn cancel_request(&self, request_id: u64) -> Result<(), CodingToolsError> {
        if self
            .operate_json(ops::CODING_CANCEL, json!({ "request_id": request_id }))?
            .is_some()
        {
            return Ok(());
        }
        match &self.fallback {
            Some(fb) => fb.cancel_request(request_id).await,
            None => Err(CodingToolsError::Unavailable(
                "coding-tools cancel not implemented by this provider".into(),
            )),
        }
    }
}

/// Query Active `BrowserIntegration` for health; Absent when none.
pub fn browser_health_via_extension(
    host: Option<&Arc<Mutex<ExtensionHost>>>,
) -> BrowserHealthStatus {
    let Some(host) = host else {
        return BrowserHealthStatus::absent();
    };
    let Ok(mut guard) = host.lock() else {
        return BrowserHealthStatus::Unavailable {
            reason: "extension host lock poisoned".into(),
        };
    };
    let Some(id) = guard
        .capability_registry()
        .active_host_for(ExtensionCapabilityKind::BrowserIntegration)
        .map(str::to_string)
    else {
        return BrowserHealthStatus::absent();
    };
    let request_id = next_request_id("browser-health");
    match guard.operate(
        &id,
        &request_id,
        ops::BROWSER_HEALTH,
        json!({}),
        Some(ExtensionPermission::Browser),
        None,
    ) {
        Ok(result) => serde_json::from_value(result.data).unwrap_or_else(|e| {
            BrowserHealthStatus::Misconfigured {
                reason: format!("browser/health payload: {e}"),
            }
        }),
        Err(err) => BrowserHealthStatus::Unavailable {
            reason: err.to_string(),
        },
    }
}

/// Negotiate via Active `BrowserIntegration`; incompatible Absent otherwise.
pub fn browser_negotiate_via_extension(
    host: Option<&Arc<Mutex<ExtensionHost>>>,
    protocol_version: String,
) -> BrowserNegotiateInfo {
    let Some(host) = host else {
        return absent_negotiate(protocol_version);
    };
    let Ok(mut guard) = host.lock() else {
        return BrowserNegotiateInfo {
            protocol_version,
            compatible: false,
            reason: "extension host lock poisoned".into(),
        };
    };
    let Some(id) = guard
        .capability_registry()
        .active_host_for(ExtensionCapabilityKind::BrowserIntegration)
        .map(str::to_string)
    else {
        return absent_negotiate(protocol_version);
    };
    let request_id = next_request_id("browser-negotiate");
    match guard.operate(
        &id,
        &request_id,
        ops::BROWSER_NEGOTIATE,
        json!({ "protocol_version": protocol_version }),
        Some(ExtensionPermission::Browser),
        None,
    ) {
        Ok(result) => {
            serde_json::from_value(result.data).unwrap_or_else(|e| BrowserNegotiateInfo {
                protocol_version,
                compatible: false,
                reason: format!("browser/negotiate payload: {e}"),
            })
        }
        Err(err) => BrowserNegotiateInfo {
            protocol_version,
            compatible: false,
            reason: err.to_string(),
        },
    }
}

/// Optional long-term MemoryProvider recall (not MemoryStore).
pub fn memory_recall_via_extension(
    host: &Arc<Mutex<ExtensionHost>>,
    query: &str,
) -> Result<Option<Value>, String> {
    let mut guard = host
        .lock()
        .map_err(|_| "extension host lock poisoned".to_string())?;
    let Some(id) = guard
        .capability_registry()
        .active_host_for(ExtensionCapabilityKind::MemoryProvider)
        .map(str::to_string)
    else {
        return Ok(None);
    };
    let request_id = next_request_id("memory-recall");
    let result = guard
        .operate(
            &id,
            &request_id,
            ops::MEMORY_RECALL,
            json!({ "query": query }),
            Some(ExtensionPermission::Memory),
            None,
        )
        .map_err(|e| e.to_string())?;
    Ok(Some(result.data))
}

/// Optional ContextProvider contribution block.
pub fn context_contribute_via_extension(
    host: &Arc<Mutex<ExtensionHost>>,
) -> Result<Option<String>, String> {
    let mut guard = host
        .lock()
        .map_err(|_| "extension host lock poisoned".to_string())?;
    let Some(id) = guard
        .capability_registry()
        .active_host_for(ExtensionCapabilityKind::ContextProvider)
        .map(str::to_string)
    else {
        return Ok(None);
    };
    let request_id = next_request_id("context");
    let result = guard
        .operate(
            &id,
            &request_id,
            ops::CONTEXT_CONTRIBUTE,
            json!({}),
            Some(ExtensionPermission::FilesystemRead),
            None,
        )
        .map_err(|e| e.to_string())?;
    match result.data {
        Value::String(s) => Ok(Some(s)),
        Value::Object(map) => Ok(map.get("text").and_then(|v| v.as_str()).map(str::to_string)),
        _ => Ok(None),
    }
}

fn absent_negotiate(protocol_version: String) -> BrowserNegotiateInfo {
    BrowserNegotiateInfo {
        protocol_version,
        compatible: false,
        reason: BrowserHealthStatus::ABSENT_REASON.into(),
    }
}

fn next_request_id(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{prefix}-{nanos}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};

    use impetus_protocol::{DiagnosticSeverity, SourcePosition, SourceRange, SymbolKind};

    use crate::block_on_coding_tools;
    use crate::extension_host::{ExtensionDiscoveryRoots, ExtensionHost, ExtensionHostPhase};

    #[test]
    fn browser_absent_without_host() {
        let status = browser_health_via_extension(None);
        assert_eq!(status, BrowserHealthStatus::absent());
        let neg = browser_negotiate_via_extension(None, "1".into());
        assert!(!neg.compatible);
    }

    fn fixture_pack_src(name: &str) -> PathBuf {
        let src = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../impetus-extension-sdk/fixtures")
            .join(name);
        src.canonicalize().unwrap_or_else(|e| {
            panic!("{} fixture missing at {}: {e}", name, src.display());
        })
    }

    fn copy_host_process_fixture(data_dir: &Path, pack_id: &str) {
        let src = fixture_pack_src(pack_id);
        let roots = ExtensionDiscoveryRoots::from_data_and_workspace(data_dir, None);
        fs::create_dir_all(&roots.global_packages).expect("mkdir packages");
        let dst = roots.global_packages.join(pack_id);
        fs::create_dir_all(&dst).expect("mkdir pack");
        for name in ["extension.toml", "ext.sh"] {
            fs::copy(src.join(name), dst.join(name)).unwrap_or_else(|e| {
                panic!("copy {name} from {}: {e}", src.display());
            });
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let script = dst.join("ext.sh");
            let mut perms = fs::metadata(&script).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&script, perms).unwrap();
        }
    }

    fn load_fixture_host(pack_id: &str) -> (tempfile::TempDir, Arc<Mutex<ExtensionHost>>) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let data = tmp.path().join("data");
        copy_host_process_fixture(&data, pack_id);
        let roots = ExtensionDiscoveryRoots::from_data_and_workspace(&data, None);
        let mut host = ExtensionHost::with_persist_root(&data);
        // BrowserIntegration declares `browser`; keep scope explicit for fixtures.
        host.set_scope(crate::SandboxScope::local_workspace(&data).with_network(true));
        let _ = host.reload(&roots);
        let entry = host.get(pack_id);
        assert_eq!(
            entry.map(|e| e.phase),
            Some(ExtensionHostPhase::Active),
            "{pack_id} must activate after initialize handshake (last_error={:?})",
            entry.and_then(|e| e.last_error.clone())
        );
        (tmp, Arc::new(Mutex::new(host)))
    }

    #[test]
    fn browser_health_and_negotiate_via_host_process_fixture() {
        let (_tmp, host) = load_fixture_host("host-process-browser");

        let status = browser_health_via_extension(Some(&host));
        match status {
            BrowserHealthStatus::Available {
                provider_id,
                capabilities,
            } => {
                assert_eq!(provider_id, "host-process-browser");
                assert!(
                    capabilities.iter().any(|c| c == "health"),
                    "capabilities: {capabilities:?}"
                );
            }
            other => panic!("expected Available, got {other:?}"),
        }

        let ok = browser_negotiate_via_extension(Some(&host), "0.1".into());
        assert!(ok.compatible, "0.1 must be compatible: {ok:?}");
        assert_eq!(ok.protocol_version, "0.1");

        let bad = browser_negotiate_via_extension(Some(&host), "9.9".into());
        assert!(!bad.compatible, "9.9 must be incompatible: {bad:?}");
        assert_eq!(bad.protocol_version, "9.9");
    }

    #[test]
    fn lsp_coding_tools_via_host_process_fixture() {
        let (_tmp, host) = load_fixture_host("host-process-lsp");
        let provider = Arc::new(PreferExtensionCodingTools::new(host, None));
        let query = PositionQuery::new("src/lib.rs", 10, 0);
        let path = PathBuf::from("src/lib.rs");

        let defs = {
            let provider = Arc::clone(&provider);
            let query = query.clone();
            block_on_coding_tools(async move { provider.definition(&query).await })
        };
        let defs = defs.expect("definition");
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].path, PathBuf::from("src/lib.rs"));
        assert_eq!(defs[0].range, SourceRange::new(10, 0, 10, 3));

        let refs = {
            let provider = Arc::clone(&provider);
            let query = query.clone();
            block_on_coding_tools(async move { provider.references(&query).await })
        };
        let refs = refs.expect("references");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].path, PathBuf::from("src/main.rs"));
        assert_eq!(refs[0].range, SourceRange::new(1, 4, 1, 7));

        let hover = {
            let provider = Arc::clone(&provider);
            let query = query.clone();
            block_on_coding_tools(async move { provider.hover(&query).await })
        };
        let hover = hover.expect("hover").expect("hover Some");
        assert_eq!(hover.contents, "fn fixture()");
        assert_eq!(
            hover.range,
            Some(SourceRange {
                start: SourcePosition {
                    line: 10,
                    character: 0
                },
                end: SourcePosition {
                    line: 10,
                    character: 3
                },
            })
        );

        let diags = {
            let provider = Arc::clone(&provider);
            let path = path.clone();
            block_on_coding_tools(async move { provider.diagnostics(&path).await })
        };
        let diags = diags.expect("diagnostics");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, DiagnosticSeverity::Warning);
        assert_eq!(diags[0].message, "fixture diagnostic");
        assert_eq!(diags[0].code.as_deref(), Some("F001"));

        let symbols = {
            let provider = Arc::clone(&provider);
            let path = path.clone();
            block_on_coding_tools(async move { provider.symbols(&path).await })
        };
        let symbols = symbols.expect("symbols");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "fixture");
        assert_eq!(symbols[0].kind, SymbolKind::Function);
        assert_eq!(symbols[0].location.range, SourceRange::new(40, 0, 50, 1));

        {
            let provider = Arc::clone(&provider);
            block_on_coding_tools(async move { provider.cancel_request(42).await })
                .expect("cancel");
        }
    }
}
