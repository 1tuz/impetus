//! HarnessClient-only boundary for `impetus-tui` (#142).
//!
//! Direct `impetus-core` is forbidden. The crate may depend on `impetus-client`
//! (which may pull core transitively for shared wire types). Source must talk
//! to the harness only through `impetus_client` / `UiBackend`.

/// Names that must never appear as a *direct* Cargo dependency of this crate.
pub const FORBIDDEN_DIRECT_DEPS: &[&str] = &["impetus-core"];

/// True when `manifest` lists `package` under `[dependencies]` (not a comment).
pub fn cargo_toml_has_direct_dep(manifest: &str, package: &str) -> bool {
    let mut in_deps = false;
    for raw in manifest.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            in_deps = line == "[dependencies]";
            continue;
        }
        if !in_deps {
            continue;
        }
        // `impetus-core = …` or `impetus-core.workspace = …`
        let key = line.split_whitespace().next().unwrap_or("");
        let name = key.split('=').next().unwrap_or("").trim();
        let name = name.split('.').next().unwrap_or("").trim();
        if name == package {
            return true;
        }
    }
    false
}

/// True when Rust source uses the `impetus_core` crate (import / path).
pub fn rust_source_imports_impetus_core(source: &str) -> bool {
    for raw in source.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        // Skip line comments; block comments are rare in import lines.
        if line.starts_with("//") {
            continue;
        }
        if line.contains("extern crate impetus_core") {
            return true;
        }
        if line.starts_with("use impetus_core") || line.starts_with("use ::impetus_core") {
            return true;
        }
        // Path form: `impetus_core::…` (not inside a string / comment we skipped).
        if let Some(idx) = line.find("impetus_core::") {
            let before = &line[..idx];
            // Ignore identifiers that merely contain the substring.
            if before.is_empty()
                || before
                    .chars()
                    .next_back()
                    .is_some_and(|c| !c.is_ascii_alphanumeric() && c != '_')
            {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn crate_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
        let entries =
            fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()));
        for entry in entries {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if path.is_dir() {
                collect_rs_files(&path, out);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }

    #[test]
    fn cargo_toml_has_no_direct_impetus_core() {
        let manifest = fs::read_to_string(crate_root().join("Cargo.toml"))
            .expect("read impetus-tui Cargo.toml");
        for pkg in FORBIDDEN_DIRECT_DEPS {
            assert!(
                !cargo_toml_has_direct_dep(&manifest, pkg),
                "impetus-tui must not list `{pkg}` under [dependencies]; use impetus-client / HarnessClient only"
            );
        }
        assert!(
            cargo_toml_has_direct_dep(&manifest, "impetus-client"),
            "impetus-tui must keep impetus-client as the harness API surface"
        );
    }

    #[test]
    fn sources_do_not_import_impetus_core() {
        let mut files = Vec::new();
        collect_rs_files(&crate_root().join("src"), &mut files);
        // This module names the forbidden crate in detectors/fixtures; skip it.
        files.retain(|p| p.file_name().and_then(|n| n.to_str()) != Some("boundary.rs"));
        assert!(!files.is_empty(), "expected Rust sources under src/");
        for path in files {
            let source = fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            assert!(
                !rust_source_imports_impetus_core(&source),
                "{} must not import impetus_core; use impetus_client / UiBackend",
                path.display()
            );
        }
    }

    #[test]
    fn parser_detects_direct_dep_and_import() {
        assert!(cargo_toml_has_direct_dep(
            "[dependencies]\nimpetus-core = { path = \"../impetus-core\" }\n",
            "impetus-core"
        ));
        assert!(!cargo_toml_has_direct_dep(
            "[dependencies]\nimpetus-client = { path = \"../impetus-client\" }\n# impetus-core = bad\n",
            "impetus-core"
        ));
        assert!(rust_source_imports_impetus_core(
            "use impetus_core::Event;\n"
        ));
        assert!(rust_source_imports_impetus_core(
            "let x = impetus_core::foo();\n"
        ));
        assert!(!rust_source_imports_impetus_core(
            "// never import impetus_core\nuse impetus_client::HarnessClient;\n"
        ));
    }
}
