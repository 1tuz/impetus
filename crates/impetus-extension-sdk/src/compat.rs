//! Host ↔ extension API compatibility checks.

use thiserror::Error;

use crate::version::{ExtensionApiVersion, SupportedApiRange};

/// Compatibility rejection when a manifest API version is outside the host range.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CompatError {
    #[error(
        "extension API version {required} is below supported minimum {min} (supported range {min}..={max})"
    )]
    BelowMin { required: u32, min: u32, max: u32 },
    #[error(
        "extension API version {required} is above supported maximum {max} (supported range {min}..={max})"
    )]
    AboveMax { required: u32, min: u32, max: u32 },
}

/// Reject load when `required` is outside the host's `supported` range.
pub fn check_compatibility(
    required: ExtensionApiVersion,
    supported: SupportedApiRange,
) -> Result<(), CompatError> {
    if required < supported.min {
        return Err(CompatError::BelowMin {
            required: required.get(),
            min: supported.min.get(),
            max: supported.max.get(),
        });
    }
    if required > supported.max {
        return Err(CompatError::AboveMax {
            required: required.get(),
            min: supported.min.get(),
            max: supported.max.get(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::version::ExtensionApiVersion;

    #[test]
    fn in_range_ok() {
        let range = SupportedApiRange::new(ExtensionApiVersion(1), ExtensionApiVersion(3));
        assert!(check_compatibility(ExtensionApiVersion(1), range).is_ok());
        assert!(check_compatibility(ExtensionApiVersion(2), range).is_ok());
        assert!(check_compatibility(ExtensionApiVersion(3), range).is_ok());
    }

    #[test]
    fn below_min_err() {
        let range = SupportedApiRange::new(ExtensionApiVersion(2), ExtensionApiVersion(3));
        let err = check_compatibility(ExtensionApiVersion(1), range).unwrap_err();
        assert!(matches!(
            err,
            CompatError::BelowMin {
                required: 1,
                min: 2,
                ..
            }
        ));
        assert!(err.to_string().contains("below supported minimum"));
    }

    #[test]
    fn above_max_err() {
        let range = SupportedApiRange::new(ExtensionApiVersion(1), ExtensionApiVersion(2));
        let err = check_compatibility(ExtensionApiVersion(3), range).unwrap_err();
        assert!(matches!(
            err,
            CompatError::AboveMax {
                required: 3,
                max: 2,
                ..
            }
        ));
        assert!(err.to_string().contains("above supported maximum"));
    }
}
