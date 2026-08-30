use anyhow::Result;
use std::fmt;

/// Service provider abstraction for replaceable services
///
/// Enables dependency injection while maintaining type safety:
/// - Builtin: compiled-in implementation
/// - Custom: user-provided implementation (same process)
/// - External: separate process via IPC
pub enum ServiceProvider<T> {
    /// Built-in service implementation
    Builtin(T),
    /// Custom user-provided implementation
    Custom(Box<dyn ServiceTrait<Output = T>>),
    /// External module via IPC
    External(ExternalServiceHandle),
}

impl<T> ServiceProvider<T> {
    /// Create a builtin provider
    pub fn builtin(service: T) -> Self {
        Self::Builtin(service)
    }

    /// Create a custom provider
    pub fn custom<S>(service: S) -> Self
    where
        S: ServiceTrait<Output = T> + 'static,
    {
        Self::Custom(Box::new(service))
    }

    /// Create an external provider
    pub fn external(handle: ExternalServiceHandle) -> Self {
        Self::External(handle)
    }

    /// Get the service kind
    pub fn kind(&self) -> ServiceProviderKind {
        match self {
            Self::Builtin(_) => ServiceProviderKind::Builtin,
            Self::Custom(_) => ServiceProviderKind::Custom,
            Self::External(_) => ServiceProviderKind::External,
        }
    }

    /// Get builtin service reference (if available)
    pub fn as_builtin(&self) -> Option<&T> {
        match self {
            Self::Builtin(service) => Some(service),
            _ => None,
        }
    }

    /// Get mutable builtin service reference (if available)
    pub fn as_builtin_mut(&mut self) -> Option<&mut T> {
        match self {
            Self::Builtin(service) => Some(service),
            _ => None,
        }
    }
}

impl<T: fmt::Debug> fmt::Debug for ServiceProvider<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Builtin(service) => f.debug_tuple("Builtin").field(service).finish(),
            Self::Custom(_) => f.debug_tuple("Custom").field(&"<trait object>").finish(),
            Self::External(handle) => f.debug_tuple("External").field(handle).finish(),
        }
    }
}

/// Service provider kind
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceProviderKind {
    Builtin,
    Custom,
    External,
}

impl fmt::Display for ServiceProviderKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Builtin => write!(f, "builtin"),
            Self::Custom => write!(f, "custom"),
            Self::External => write!(f, "external"),
        }
    }
}

/// Trait for custom service implementations
pub trait ServiceTrait: Send + Sync {
    type Output;

    fn name(&self) -> &str;
    fn version(&self) -> &str;
}

/// Handle to an external service via IPC
#[derive(Debug, Clone)]
pub struct ExternalServiceHandle {
    pub module_id: String,
    pub socket_path: String,
}

impl ExternalServiceHandle {
    pub fn new(module_id: String, socket_path: String) -> Self {
        Self {
            module_id,
            socket_path,
        }
    }
}

/// Service resolution result
pub enum ResolvedService<T> {
    Ready(T),
    Degraded { fallback: T, reason: String },
    Unavailable { reason: String },
}

impl<T> ResolvedService<T> {
    /// Check if service is ready
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready(_))
    }

    /// Check if service is degraded
    pub fn is_degraded(&self) -> bool {
        matches!(self, Self::Degraded { .. })
    }

    /// Get service reference (ready or degraded fallback)
    pub fn service(&self) -> Option<&T> {
        match self {
            Self::Ready(service) => Some(service),
            Self::Degraded { fallback, .. } => Some(fallback),
            Self::Unavailable { .. } => None,
        }
    }

    /// Unwrap service or panic
    pub fn unwrap(self) -> T {
        match self {
            Self::Ready(service) => service,
            Self::Degraded { fallback, .. } => fallback,
            Self::Unavailable { reason } => panic!("Service unavailable: {}", reason),
        }
    }

    /// Convert to Result
    pub fn into_result(self) -> Result<T> {
        match self {
            Self::Ready(service) => Ok(service),
            Self::Degraded { fallback, .. } => Ok(fallback),
            Self::Unavailable { reason } => anyhow::bail!("Service unavailable: {}", reason),
        }
    }
}

impl<T: fmt::Debug> fmt::Debug for ResolvedService<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ready(service) => f.debug_tuple("Ready").field(service).finish(),
            Self::Degraded { fallback, reason } => f
                .debug_struct("Degraded")
                .field("fallback", fallback)
                .field("reason", reason)
                .finish(),
            Self::Unavailable { reason } => f
                .debug_struct("Unavailable")
                .field("reason", reason)
                .finish(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone)]
    struct MockService {
        #[allow(dead_code)]
        name: String,
    }

    struct MockCustomService;

    impl ServiceTrait for MockCustomService {
        type Output = MockService;

        fn name(&self) -> &str {
            "mock-custom"
        }

        fn version(&self) -> &str {
            "1.0.0"
        }
    }

    #[test]
    fn test_builtin_provider() {
        let service = MockService {
            name: "test".to_string(),
        };
        let provider = ServiceProvider::builtin(service);

        assert_eq!(provider.kind(), ServiceProviderKind::Builtin);
        assert!(provider.as_builtin().is_some());
    }

    #[test]
    fn test_custom_provider() {
        let provider: ServiceProvider<MockService> = ServiceProvider::custom(MockCustomService);

        assert_eq!(provider.kind(), ServiceProviderKind::Custom);
        assert!(provider.as_builtin().is_none());
    }

    #[test]
    fn test_external_provider() {
        let handle =
            ExternalServiceHandle::new("test-module".to_string(), "/tmp/test.sock".to_string());
        let provider: ServiceProvider<MockService> = ServiceProvider::external(handle);

        assert_eq!(provider.kind(), ServiceProviderKind::External);
    }

    #[test]
    fn test_resolved_service_ready() {
        let service = MockService {
            name: "test".to_string(),
        };
        let resolved = ResolvedService::Ready(service);

        assert!(resolved.is_ready());
        assert!(!resolved.is_degraded());
        assert!(resolved.service().is_some());
    }

    #[test]
    fn test_resolved_service_degraded() {
        let fallback = MockService {
            name: "fallback".to_string(),
        };
        let resolved = ResolvedService::Degraded {
            fallback,
            reason: "Primary failed".to_string(),
        };

        assert!(!resolved.is_ready());
        assert!(resolved.is_degraded());
        assert!(resolved.service().is_some());
    }

    #[test]
    fn test_resolved_service_unavailable() {
        let resolved: ResolvedService<MockService> = ResolvedService::Unavailable {
            reason: "Not configured".to_string(),
        };

        assert!(!resolved.is_ready());
        assert!(!resolved.is_degraded());
        assert!(resolved.service().is_none());
    }
}
