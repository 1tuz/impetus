//! RTK (Reduce Token Kontinuity) optional adapter.
//!
//! RTK is an external tool for intelligent output reduction. This adapter:
//! - Probes RTK binary availability at runtime
//! - Not a hard dependency — harness works without it
//! - Integrates with Output Optimization pipeline as replaceable reducer
//!
//! See ROADMAP § OUTPUT OPTIMIZATION.

use std::process::Command;

/// RTK adapter for optional external output reduction
#[derive(Debug)]
pub struct RtkAdapter {
    available: bool,
    version: Option<String>,
    capabilities: Vec<RtkCapability>,
}

/// RTK capability
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RtkCapability {
    /// Basic output reduction
    Reduce,
    /// Command-specific reduction (e.g., cargo test, git diff)
    CommandAware,
    /// Structured output parsing
    Structured,
}

/// RTK probe result
#[derive(Debug, Clone)]
pub struct RtkProbe {
    pub available: bool,
    pub version: Option<String>,
    pub capabilities: Vec<RtkCapability>,
    pub binary_path: Option<String>,
}

impl RtkAdapter {
    /// Create new RTK adapter with runtime probing
    pub fn new() -> Self {
        let probe = Self::probe();
        Self {
            available: probe.available,
            version: probe.version,
            capabilities: probe.capabilities,
        }
    }

    /// Probe RTK availability and capabilities
    pub fn probe() -> RtkProbe {
        // Try to find rtk binary in PATH
        let binary_check = Command::new("which").arg("rtk").output();

        let binary_path = match binary_check {
            Ok(output) if output.status.success() => {
                String::from_utf8_lossy(&output.stdout).trim().to_string()
            }
            _ => {
                return RtkProbe {
                    available: false,
                    version: None,
                    capabilities: vec![],
                    binary_path: None,
                };
            }
        };

        // Get RTK version
        let version_output = Command::new("rtk").arg("--version").output();

        let version = version_output.ok().and_then(|out| {
            if out.status.success() {
                Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
            } else {
                None
            }
        });

        // Probe capabilities
        let capabilities = Self::probe_capabilities();

        RtkProbe {
            available: true,
            version,
            capabilities,
            binary_path: Some(binary_path),
        }
    }

    /// Probe RTK capabilities
    fn probe_capabilities() -> Vec<RtkCapability> {
        let mut caps = vec![RtkCapability::Reduce];

        // Check for command-aware reduction
        let help_output = Command::new("rtk").arg("--help").output();

        if let Ok(output) = help_output {
            let help_text = String::from_utf8_lossy(&output.stdout);

            if help_text.contains("cargo") || help_text.contains("git") {
                caps.push(RtkCapability::CommandAware);
            }

            if help_text.contains("json") || help_text.contains("structured") {
                caps.push(RtkCapability::Structured);
            }
        }

        caps
    }

    /// Check if RTK is available
    pub fn is_available(&self) -> bool {
        self.available
    }

    /// Get RTK version
    pub fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }

    /// Check if capability is supported
    pub fn has_capability(&self, cap: &RtkCapability) -> bool {
        self.capabilities.contains(cap)
    }

    /// Reduce output using RTK
    ///
    /// Falls back to None if RTK unavailable or errors.
    pub fn reduce(&self, raw_output: &str, max_tokens: usize) -> Option<String> {
        if !self.available {
            return None;
        }

        // Invoke RTK with token budget
        let mut child = Command::new("rtk")
            .arg("reduce")
            .arg("--max-tokens")
            .arg(max_tokens.to_string())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .ok()?;

        // Write input
        if let Some(stdin) = child.stdin.as_mut() {
            use std::io::Write;
            let _ = stdin.write_all(raw_output.as_bytes());
        }

        // Wait for completion
        let output = child.wait_with_output().ok()?;

        if !output.status.success() {
            return None;
        }

        Some(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Reduce output with command context
    pub fn reduce_with_context(
        &self,
        raw_output: &str,
        command: &str,
        max_tokens: usize,
    ) -> Option<String> {
        if !self.available || !self.has_capability(&RtkCapability::CommandAware) {
            return None;
        }

        let mut child = Command::new("rtk")
            .arg(command)
            .arg("--max-tokens")
            .arg(max_tokens.to_string())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .ok()?;

        if let Some(stdin) = child.stdin.as_mut() {
            use std::io::Write;
            let _ = stdin.write_all(raw_output.as_bytes());
        }

        let output = child.wait_with_output().ok()?;

        if !output.status.success() {
            return None;
        }

        Some(String::from_utf8_lossy(&output.stdout).to_string())
    }
}

impl Default for RtkAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_returns_result() {
        let probe = RtkAdapter::probe();
        // RTK may or may not be present — just verify probe completes
        assert!(probe.version.is_none() || !probe.version.as_ref().unwrap().is_empty());
    }

    #[test]
    fn adapter_creation() {
        let adapter = RtkAdapter::new();
        // Should not panic regardless of RTK presence
        let _ = adapter.is_available();
    }

    #[test]
    fn capability_check() {
        let adapter = RtkAdapter::new();
        if adapter.is_available() {
            // If available, should have at least Reduce
            assert!(adapter.has_capability(&RtkCapability::Reduce));
        }
    }

    #[test]
    fn reduce_graceful_when_unavailable() {
        let mut adapter = RtkAdapter::new();
        adapter.available = false;

        let result = adapter.reduce("test output", 100);
        assert!(result.is_none());
    }
}
