//! Daemon E2E: BrowserIntegration + LspIntegration host_process fixtures
//! answer honest health/negotiate and coding IPC (closes #391).

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::DaemonFixture;
use impetus_client::{HarnessClient, UnixSocketTransport};
use impetus_protocol::{BrowserHealthStatus, IpcRequest, IpcResponse, SourceRange, SymbolKind};

fn copy_host_process_fixture(data_dir: &Path, pack_id: &str) {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../impetus-extension-sdk/fixtures")
        .join(pack_id);
    let src = src.canonicalize().unwrap_or_else(|e| {
        panic!("{pack_id} fixture missing at {}: {e}", src.display());
    });
    let dst = data_dir.join("extensions").join("packages").join(pack_id);
    fs::create_dir_all(&dst).unwrap_or_else(|e| panic!("mkdir {pack_id}: {e}"));
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_unix_browser_fixture_health_and_negotiate() {
    let daemon = DaemonFixture::spawn();
    copy_host_process_fixture(daemon.data_dir.path(), "host-process-browser");

    let client = UnixSocketTransport::connect(&daemon.socket)
        .await
        .expect("connect");

    let hello = client.hello().await.expect("hello");
    let IpcResponse::Hello {
        version,
        capabilities,
        ..
    } = hello
    else {
        panic!("expected Hello, got {hello:?}");
    };
    assert!(version >= 14, "need IPC v14, got {version}");
    assert!(
        capabilities.iter().any(|c| c == "extension_manage"),
        "missing extension_manage"
    );
    assert!(
        capabilities.iter().any(|c| c == "browser"),
        "missing browser capability: {capabilities:?}"
    );

    let reloaded = client
        .request(IpcRequest::ReloadExtensionPackages)
        .await
        .expect("reload");
    let IpcResponse::ExtensionPackagesReloaded { loaded, failed } = reloaded else {
        panic!("expected ExtensionPackagesReloaded, got {reloaded:?}");
    };
    assert_eq!(failed, 0, "browser fixture must not fail load");
    assert!(loaded >= 1, "expected host-process-browser loaded");

    let listed = client
        .request(IpcRequest::ListExtensionPackages)
        .await
        .expect("list");
    let IpcResponse::ExtensionPackages { packages } = listed else {
        panic!("expected ExtensionPackages, got {listed:?}");
    };
    let pack = packages
        .iter()
        .find(|p| p.id == "host-process-browser")
        .expect("host-process-browser in inventory");
    assert_eq!(pack.phase, "active", "browser pack Active: {pack:?}");

    let health = client
        .request(IpcRequest::GetBrowserHealth)
        .await
        .expect("GetBrowserHealth");
    let IpcResponse::BrowserHealth { status } = health else {
        panic!("expected BrowserHealth, got {health:?}");
    };
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

    let ok = client
        .request(IpcRequest::NegotiateBrowser {
            protocol_version: "0.1".into(),
        })
        .await
        .expect("NegotiateBrowser 0.1");
    let IpcResponse::BrowserNegotiate { result } = ok else {
        panic!("expected BrowserNegotiate, got {ok:?}");
    };
    assert!(result.compatible, "0.1 must be compatible: {result:?}");
    assert_eq!(result.protocol_version, "0.1");

    let bad = client
        .request(IpcRequest::NegotiateBrowser {
            protocol_version: "9.9".into(),
        })
        .await
        .expect("NegotiateBrowser 9.9");
    let IpcResponse::BrowserNegotiate { result } = bad else {
        panic!("expected BrowserNegotiate, got {bad:?}");
    };
    assert!(!result.compatible, "9.9 must be incompatible: {result:?}");
    assert_eq!(result.protocol_version, "9.9");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_unix_lsp_fixture_goto_definition() {
    let daemon = DaemonFixture::spawn();
    copy_host_process_fixture(daemon.data_dir.path(), "host-process-lsp");

    let client = UnixSocketTransport::connect(&daemon.socket)
        .await
        .expect("connect");

    let hello = client.hello().await.expect("hello");
    let IpcResponse::Hello {
        version,
        capabilities,
        ..
    } = hello
    else {
        panic!("expected Hello, got {hello:?}");
    };
    assert!(version >= 14, "need IPC v14, got {version}");
    assert!(
        capabilities.iter().any(|c| c == "coding_definition"),
        "missing coding_definition: {capabilities:?}"
    );

    let reloaded = client
        .request(IpcRequest::ReloadExtensionPackages)
        .await
        .expect("reload");
    let IpcResponse::ExtensionPackagesReloaded { loaded, failed } = reloaded else {
        panic!("expected ExtensionPackagesReloaded, got {reloaded:?}");
    };
    assert_eq!(failed, 0, "lsp fixture must not fail load");
    assert!(loaded >= 1, "expected host-process-lsp loaded");

    let listed = client
        .request(IpcRequest::ListExtensionPackages)
        .await
        .expect("list");
    let IpcResponse::ExtensionPackages { packages } = listed else {
        panic!("expected ExtensionPackages, got {listed:?}");
    };
    let pack = packages
        .iter()
        .find(|p| p.id == "host-process-lsp")
        .expect("host-process-lsp in inventory");
    assert_eq!(pack.phase, "active", "lsp pack Active: {pack:?}");

    let def = client
        .request(IpcRequest::GotoDefinition {
            path: PathBuf::from("src/lib.rs"),
            line: 10,
            character: 0,
        })
        .await
        .expect("GotoDefinition");
    let IpcResponse::Definition { locations } = def else {
        panic!("expected Definition, got {def:?}");
    };
    assert_eq!(locations.len(), 1);
    assert_eq!(locations[0].path, PathBuf::from("src/lib.rs"));
    assert_eq!(locations[0].range, SourceRange::new(10, 0, 10, 3));

    let hover = client
        .request(IpcRequest::Hover {
            path: PathBuf::from("src/lib.rs"),
            line: 10,
            character: 0,
        })
        .await
        .expect("Hover");
    let IpcResponse::Hover { info } = hover else {
        panic!("expected Hover, got {hover:?}");
    };
    let info = info.expect("hover Some");
    assert_eq!(info.contents, "fn fixture()");

    let diags = client
        .request(IpcRequest::CodingDiagnostics {
            path: PathBuf::from("src/lib.rs"),
        })
        .await
        .expect("CodingDiagnostics");
    let IpcResponse::CodingDiagnostics { diagnostics } = diags else {
        panic!("expected CodingDiagnostics, got {diags:?}");
    };
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].message, "fixture diagnostic");

    let syms = client
        .request(IpcRequest::CodingSymbols {
            path: PathBuf::from("src/lib.rs"),
        })
        .await
        .expect("CodingSymbols");
    let IpcResponse::CodingSymbols { symbols } = syms else {
        panic!("expected CodingSymbols, got {syms:?}");
    };
    assert_eq!(symbols.len(), 1);
    assert_eq!(symbols[0].name, "fixture");
    assert_eq!(symbols[0].kind, SymbolKind::Function);

    let cancel = client
        .request(IpcRequest::CancelCodingRequest { request_id: 7 })
        .await
        .expect("CancelCodingRequest");
    let IpcResponse::CodingCancelAccepted { request_id } = cancel else {
        panic!("expected CodingCancelAccepted, got {cancel:?}");
    };
    assert_eq!(request_id, 7);
}
