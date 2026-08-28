//! ACP Gateway для v0.3 — связывает harness с external coding-agent CLI.
//!
//! Один дочерний process на profile; stdout для ACP JSON-RPC, stderr для logs.
//! ACP session → внутренняя durable Session; permission проходит Policy.

pub mod gateway;
pub mod mock;
pub mod profile;

pub use gateway::{AcpGateway, AgentStatus, GatewayError};
pub use mock::MockAgent;
pub use profile::{AcpProfile, CredentialStrategy};
