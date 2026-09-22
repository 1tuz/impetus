//! Shared SDK error envelope.

use thiserror::Error;

use crate::compat::CompatError;
use crate::entrypoint::EntrypointError;
use crate::id::ExtensionIdError;
use crate::lifecycle::ExtensionError;
use crate::manifest::ExtensionPackageManifestError;
use crate::permissions::PermissionsError;

/// Top-level SDK error for host/author tooling.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ExtensionSdkError {
    #[error(transparent)]
    Id(#[from] ExtensionIdError),
    #[error(transparent)]
    Compat(#[from] CompatError),
    #[error(transparent)]
    Permissions(#[from] PermissionsError),
    #[error(transparent)]
    Entrypoint(#[from] EntrypointError),
    #[error(transparent)]
    Manifest(#[from] ExtensionPackageManifestError),
    #[error(transparent)]
    Extension(#[from] ExtensionError),
}
