//! Real `impetusd` E2E for ExtensionHost packages:
//! discover → compat isolation → load/active → Context sees skill →
//! disable → skill disappears from Context → enable → skill returns →
//! disable → restart → disable durable (phase disabled, skill absent).

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{DaemonFixture, workspace_root};
use impetus_client::{HarnessClient, UnixSocketTransport};
use impetus_protocol::{InstructionKind, IpcRequest, IpcResponse};

fn write_pack(data_dir: &Path, id: &str, api_version: u32, skill_id: &str) {
    let pack = data_dir.join("extensions").join("packages").join(id);
    fs::create_dir_all(pack.join("skills")).expect("mkdir pack");
    fs::write(
        pack.join("extension.toml"),
        format!(
            r#"
schema_version = 1
id = "{id}"
name = "{id}"
version = "0.1.0"
description = "e2e fixture"
author = "impetus"
extension_api_version = {api_version}
capabilities = ["skill_provider"]
permissions = ["filesystem_read"]

[entrypoint]
kind = "instruction_pack"
root = "skills"
"#
        ),
    )
    .expect("write manifest");
    fs::write(
        pack.join("skills").join("SKILL.md"),
        format!("---\nid: {skill_id}\nscope: global\n---\n# {skill_id}\n"),
    )
    .expect("write skill");
}

fn context_has_skill(context: &impetus_protocol::ResolvedInstructions, skill_id: &str) -> bool {
    context.references.iter().any(|r| {
        r.kind == InstructionKind::Skill && (r.id == skill_id || r.text.contains(skill_id))
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_unix_extension_package_lifecycle() {
    let mut daemon = DaemonFixture::spawn();
    write_pack(
        daemon.data_dir.path(),
        "demo-pack",
        1,
        "demo-extension-skill",
    );
    write_pack(daemon.data_dir.path(), "bad-api-pack", 99, "bad-skill");

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
    assert!(
        version >= 14,
        "need IPC v14 for extension_manage, got {version}"
    );
    assert!(
        capabilities.iter().any(|c| c == "extension_manage"),
        "missing extension_manage in {capabilities:?}"
    );

    let reloaded = client
        .request(IpcRequest::ReloadExtensionPackages)
        .await
        .expect("reload");
    let IpcResponse::ExtensionPackagesReloaded { loaded, failed } = reloaded else {
        panic!("expected ExtensionPackagesReloaded, got {reloaded:?}");
    };
    assert_eq!(loaded, 1, "only compatible pack loads");
    assert_eq!(failed, 1, "outdated API pack must fail in isolation");

    let listed = client
        .request(IpcRequest::ListExtensionPackages)
        .await
        .expect("list");
    let IpcResponse::ExtensionPackages { packages } = listed else {
        panic!("expected ExtensionPackages, got {listed:?}");
    };
    assert!(
        packages
            .iter()
            .any(|p| p.id == "demo-pack" && p.phase == "active"),
        "demo-pack active: {packages:?}"
    );
    assert!(
        packages.iter().all(|p| p.id != "bad-api-pack"),
        "incompat pack must not enter inventory: {packages:?}"
    );

    let session_id = client
        .create_session(workspace_root())
        .await
        .expect("create session");

    let ctx_on = client.get_context(session_id).await.expect("context on");
    assert!(
        context_has_skill(&ctx_on, "demo-extension-skill"),
        "Active instruction_pack must appear in Context: {:?}",
        ctx_on
            .references
            .iter()
            .map(|r| (&r.id, r.kind))
            .collect::<Vec<_>>()
    );

    let disabled = client
        .request(IpcRequest::DisableExtensionPackage {
            id: "demo-pack".into(),
        })
        .await
        .expect("disable");
    let IpcResponse::ExtensionPackage { package } = disabled else {
        panic!("expected ExtensionPackage, got {disabled:?}");
    };
    assert_eq!(package.phase, "disabled");

    let ctx_off = client.get_context(session_id).await.expect("context off");
    assert!(
        !context_has_skill(&ctx_off, "demo-extension-skill"),
        "disable must remove skill from Context: {:?}",
        ctx_off
            .references
            .iter()
            .map(|r| (&r.id, r.kind))
            .collect::<Vec<_>>()
    );

    let enabled = client
        .request(IpcRequest::EnableExtensionPackage {
            id: "demo-pack".into(),
        })
        .await
        .expect("enable");
    let IpcResponse::ExtensionPackage { package } = enabled else {
        panic!("expected ExtensionPackage, got {enabled:?}");
    };
    assert_eq!(package.phase, "active");

    let ctx_back = client.get_context(session_id).await.expect("context back");
    assert!(
        context_has_skill(&ctx_back, "demo-extension-skill"),
        "re-enable must restore skill in Context"
    );

    // Disable again, then restart — durable SoT must keep it Disabled.
    let _ = client
        .request(IpcRequest::DisableExtensionPackage {
            id: "demo-pack".into(),
        })
        .await
        .expect("disable before restart");

    daemon.restart_after_kill();
    let client2 = UnixSocketTransport::connect(&daemon.socket)
        .await
        .expect("reconnect");
    let _ = client2
        .request(IpcRequest::ReloadExtensionPackages)
        .await
        .expect("reload after restart");
    let listed2 = client2
        .request(IpcRequest::ListExtensionPackages)
        .await
        .expect("list after restart");
    let IpcResponse::ExtensionPackages { packages } = listed2 else {
        panic!("expected ExtensionPackages, got {listed2:?}");
    };
    assert!(
        packages
            .iter()
            .any(|p| p.id == "demo-pack" && p.phase == "disabled"),
        "demo-pack must stay disabled across restart: {packages:?}"
    );

    let session2 = client2
        .create_session(workspace_root())
        .await
        .expect("session after restart");
    let ctx_after = client2.get_context(session2).await.expect("context after");
    assert!(
        !context_has_skill(&ctx_after, "demo-extension-skill"),
        "durable disable must keep skill out of Context after restart"
    );
}

fn copy_host_process_echo_fixture(data_dir: &Path) {
    // Integration tests run with cwd = crates/impetusd; fixture lives in sibling crate.
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../impetus-extension-sdk/fixtures/host-process-echo");
    let src = src.canonicalize().unwrap_or_else(|e| {
        panic!(
            "host-process-echo fixture missing at {}: {e}",
            src.display()
        );
    });
    let dst = data_dir
        .join("extensions")
        .join("packages")
        .join("host-process-echo");
    fs::create_dir_all(&dst).expect("mkdir host-process-echo");
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

/// Daemon E2E: deterministic host_process fixture activates via initialize
/// handshake (operate RPC covered in impetus-core unit tests; no IPC operate
/// surface in this slice).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_unix_host_process_fixture_activates() {
    let mut daemon = DaemonFixture::spawn();
    copy_host_process_echo_fixture(daemon.data_dir.path());

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

    let reloaded = client
        .request(IpcRequest::ReloadExtensionPackages)
        .await
        .expect("reload");
    let IpcResponse::ExtensionPackagesReloaded { loaded, failed } = reloaded else {
        panic!("expected ExtensionPackagesReloaded, got {reloaded:?}");
    };
    assert_eq!(failed, 0, "host_process fixture must not fail load");
    assert!(loaded >= 1, "expected at least host-process-echo loaded");

    let listed = client
        .request(IpcRequest::ListExtensionPackages)
        .await
        .expect("list");
    let IpcResponse::ExtensionPackages { packages } = listed else {
        panic!("expected ExtensionPackages, got {listed:?}");
    };
    let pack = packages
        .iter()
        .find(|p| p.id == "host-process-echo")
        .expect("host-process-echo in inventory");
    assert_eq!(
        pack.phase, "active",
        "host_process initialize handshake must leave package Active: {pack:?}"
    );
    assert!(
        pack.permissions.iter().any(|p| p == "process_spawn"),
        "process_spawn required: {:?}",
        pack.permissions
    );
    assert!(pack.last_error.is_none(), "no last_error: {pack:?}");

    let disabled = client
        .request(IpcRequest::DisableExtensionPackage {
            id: "host-process-echo".into(),
        })
        .await
        .expect("disable");
    let IpcResponse::ExtensionPackage { package } = disabled else {
        panic!("expected ExtensionPackage, got {disabled:?}");
    };
    assert_eq!(package.phase, "disabled");

    // Restart must keep durable disable (same SoT as instruction_pack path).
    daemon.restart_after_kill();
    let client2 = UnixSocketTransport::connect(&daemon.socket)
        .await
        .expect("reconnect");
    let _ = client2
        .request(IpcRequest::ReloadExtensionPackages)
        .await
        .expect("reload after restart");
    let listed2 = client2
        .request(IpcRequest::ListExtensionPackages)
        .await
        .expect("list after restart");
    let IpcResponse::ExtensionPackages { packages } = listed2 else {
        panic!("expected ExtensionPackages, got {listed2:?}");
    };
    assert!(
        packages
            .iter()
            .any(|p| p.id == "host-process-echo" && p.phase == "disabled"),
        "host-process-echo must stay disabled across restart: {packages:?}"
    );
}
