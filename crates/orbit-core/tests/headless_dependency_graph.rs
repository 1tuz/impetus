//! Headless dependency graph verification for v0.2 gate.
//!
//! Проверяет, что headless runtime не зависит от GPUI, Metal,
//! PTY и ANSI renderer — клиентских concerns.

use std::process::Command;

#[test]
fn headless_no_gpui_dependency() {
    // cargo tree для orbit-core не должен содержать gpui
    let output = Command::new("cargo")
        .args(["tree", "-p", "orbit-core", "--no-dedupe"])
        .output()
        .expect("cargo tree");

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        !stdout.contains("gpui"),
        "headless core must not depend on gpui:\n{}",
        stdout
    );

    println!("✓ No gpui dependency in orbit-core");
}

#[test]
fn headless_no_metal_dependency() {
    let output = Command::new("cargo")
        .args(["tree", "-p", "orbit-core", "--no-dedupe"])
        .output()
        .expect("cargo tree");

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        !stdout.contains("metal"),
        "headless core must not depend on metal:\n{}",
        stdout
    );

    println!("✓ No metal dependency in orbit-core");
}

#[test]
fn headless_no_pty_dependency() {
    let output = Command::new("cargo")
        .args(["tree", "-p", "orbit-core", "--no-dedupe"])
        .output()
        .expect("cargo tree");

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Проверяем отсутствие portable-pty, mio-pty и других PTY crates
    assert!(
        !stdout.contains("portable-pty") && !stdout.contains("mio-pty"),
        "headless core must not depend on PTY crates:\n{}",
        stdout
    );

    println!("✓ No PTY dependency in orbit-core");
}

#[test]
fn headless_no_ansi_renderer() {
    let output = Command::new("cargo")
        .args(["tree", "-p", "orbit-core", "--no-dedupe"])
        .output()
        .expect("cargo tree");

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Проверяем отсутствие vte, alacritty_terminal и других ANSI parsers
    assert!(
        !stdout.contains("vte") && !stdout.contains("alacritty_terminal"),
        "headless core must not depend on ANSI renderer crates:\n{}",
        stdout
    );

    println!("✓ No ANSI renderer dependency in orbit-core");
}

#[test]
fn headless_allowed_dependencies() {
    // Проверка позитивная: headless может зависеть от SQLite, serde, tokio, UUID и т.п.
    let output = Command::new("cargo")
        .args(["tree", "-p", "orbit-core", "--depth", "1"])
        .output()
        .expect("cargo tree");

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Ожидаемые зависимости headless runtime
    let expected = ["rusqlite", "serde", "uuid", "thiserror"];

    for dep in expected {
        assert!(
            stdout.contains(dep),
            "expected dependency {} not found in tree",
            dep
        );
    }

    println!("✓ All expected headless dependencies present");
    println!("Headless dependency tree (depth 1):\n{}", stdout);
}
