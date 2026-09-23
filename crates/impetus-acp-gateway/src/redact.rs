//! ACP payload redaction for logs, stream events, and export audit.
//!
//! Labels only — never emit raw tokens / private keys / bearer headers.

use serde_json::{Map, Value};

const REDACTED: &str = "[REDACTED]";

/// Keys whose values are always redacted (case-insensitive, `_`/`-` normalized).
const SECRET_KEYS: &[&str] = &[
    "token",
    "secret",
    "password",
    "passphrase",
    "pass_phrase",
    "api_key",
    "apikey",
    "access_key",
    "private_key",
    "authorization",
    "credential",
    "credentials",
    "auth",
    "bearer",
];

/// Redact a free-form string (headers, env dumps, tool output previews).
pub fn redact_text(source: &str) -> String {
    let mut output = String::with_capacity(source.len());
    let mut private_key_block = false;
    for segment in source.split_inclusive('\n') {
        let (line, newline) = segment
            .strip_suffix('\n')
            .map_or((segment, ""), |line| (line, "\n"));
        let upper = line.to_ascii_uppercase();
        if upper.contains("-----BEGIN") && upper.contains("PRIVATE KEY-----") {
            private_key_block = true;
            output.push_str("[REDACTED PRIVATE KEY]");
        } else if private_key_block {
            output.push_str(REDACTED);
            if upper.contains("-----END") && upper.contains("PRIVATE KEY-----") {
                private_key_block = false;
            }
        } else {
            output.push_str(&redact_line(line));
        }
        output.push_str(newline);
    }
    output
}

fn redact_line(line: &str) -> String {
    let trimmed = line.trim_start();
    let upper = trimmed.to_ascii_uppercase();
    if upper.starts_with("AUTHORIZATION:") || upper.starts_with("BEARER ") {
        return format!("{}{REDACTED}", &line[..line.len() - trimmed.len()]);
    }
    for separator in ['=', ':'] {
        let Some((key, _value)) = line.split_once(separator) else {
            continue;
        };
        if is_secret_key(key.trim()) {
            let indent = &line[..line.len() - trimmed.len()];
            return format!("{indent}{}={REDACTED}", key.trim());
        }
    }
    line.to_owned()
}

fn normalize_key(key: &str) -> String {
    key.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

fn is_secret_key(key: &str) -> bool {
    let norm = normalize_key(key);
    SECRET_KEYS
        .iter()
        .any(|secret| norm == normalize_key(secret) || norm.contains(&normalize_key(secret)))
}

/// Deep-redact JSON for durable events / export (object keys + string leaves).
pub fn redact_json(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = Map::new();
            for (k, v) in map {
                if is_secret_key(k) {
                    out.insert(k.clone(), Value::String(REDACTED.to_owned()));
                } else {
                    out.insert(k.clone(), redact_json(v));
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(redact_json).collect()),
        Value::String(s) => Value::String(redact_text(s)),
        other => other.clone(),
    }
}

/// Export-audit envelope for a stream update (no secrets; stable shape for tests).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamExportAudit {
    Text {
        chars: usize,
        redacted: bool,
    },
    ToolUse {
        tool_call_id: String,
        tool_name: String,
        status: String,
        kind: String,
        arguments: Value,
    },
    Status {
        label: String,
    },
    Completed {
        stop_reason: String,
    },
    Interrupted {
        reason: String,
    },
    Error {
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn redacts_secret_object_keys() {
        let raw = json!({
            "command": "ls",
            "api_key": "sk-live-SECRET",
            "nested": { "token": "abc", "path": "/tmp" }
        });
        let scrubbed = redact_json(&raw);
        assert_eq!(scrubbed["api_key"], REDACTED);
        assert_eq!(scrubbed["nested"]["token"], REDACTED);
        assert_eq!(scrubbed["nested"]["path"], "/tmp");
        assert_eq!(scrubbed["command"], "ls");
    }

    #[test]
    fn redacts_bearer_and_private_key_blocks() {
        let text = "Authorization: Bearer super-secret\n-----BEGIN PRIVATE KEY-----\nabc\n-----END PRIVATE KEY-----\n";
        let scrubbed = redact_text(text);
        assert!(!scrubbed.contains("super-secret"));
        assert!(!scrubbed.contains("abc"));
        assert!(scrubbed.contains(REDACTED) || scrubbed.contains("REDACTED"));
    }
}
