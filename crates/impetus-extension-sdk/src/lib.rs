//! Stable public SDK for external `impetus-extensions` authors.
//!
//! No UI, rusqlite, harness, or daemon dependencies. Host discovery, policy,
//! and AgentLoop wiring live in `impetus-core`.

#![forbid(unsafe_code)]

pub mod capability;
pub mod compat;
pub mod config;
pub mod entrypoint;
pub mod error;
pub mod host_protocol;
pub mod id;
pub mod lifecycle;
pub mod manifest;
pub mod permissions;
pub mod version;

pub use capability::ExtensionCapabilityKind;
pub use compat::{CompatError, check_compatibility};
pub use config::{ConfigScope, ExtensionConfigSchema};
pub use entrypoint::{EntrypointError, ExtensionEntrypoint, is_safe_relative_root};
pub use error::ExtensionSdkError;
pub use host_protocol::{
    CancelParams, DEFAULT_OPERATE_TIMEOUT_MS, FORBIDDEN_SECRET_KEYS, HOST_PROTOCOL_VERSION,
    InitializeParams, InitializeResult, JsonRpcError, JsonRpcRequest, JsonRpcResponse,
    MAX_HOST_RPC_LINE_BYTES, METHOD_BROWSER_HEALTH, METHOD_BROWSER_NEGOTIATE, METHOD_CANCEL,
    METHOD_CODING_CANCEL, METHOD_CODING_DEFINITION, METHOD_CODING_DIAGNOSTICS, METHOD_CODING_HOVER,
    METHOD_CODING_SYMBOLS, METHOD_INITIALIZE, METHOD_OPERATE, METHOD_PING, METHOD_SHUTDOWN,
    OperateParams, OperateResult, check_rpc_line_size, error_codes, gate_operate_permission, ops,
    reject_secret_keys,
};
pub use id::{ExtensionId, ExtensionIdError, is_valid_extension_id};
pub use lifecycle::{Extension, ExtensionError, ExtensionHealth};
pub use manifest::{
    EXTENSION_PACKAGE_SCHEMA_ID, EXTENSION_PACKAGE_SCHEMA_VERSION, ExtensionPackageManifest,
    ExtensionPackageManifestError,
};
pub use permissions::{ExtensionPermission, PermissionsError, validate_permissions};
pub use version::{
    CURRENT_SUPPORTED_RANGE, EXTENSION_API_VERSION, ExtensionApiVersion, SupportedApiRange,
};
