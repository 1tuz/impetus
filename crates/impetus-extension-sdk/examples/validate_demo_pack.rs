//! External-author smoke: validate the reference fixture without `impetus-core`.
//!
//! ```text
//! cargo run -p impetus-extension-sdk --example validate_demo_pack
//! ```

use std::path::PathBuf;

use impetus_extension_sdk::ExtensionPackageManifest;

fn main() {
    let manifest_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("demo-pack")
        .join("extension.toml");
    let text = std::fs::read_to_string(&manifest_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", manifest_path.display()));
    let manifest = ExtensionPackageManifest::from_toml_str(&text)
        .unwrap_or_else(|e| panic!("parse fixture: {e}"));
    manifest
        .validate()
        .unwrap_or_else(|e| panic!("validate fixture: {e}"));
    println!(
        "ok: id={} api={} entrypoint={:?}",
        manifest.id.as_str(),
        manifest.extension_api_version,
        manifest.entrypoint
    );
}
