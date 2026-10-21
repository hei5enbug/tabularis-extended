pub mod artifacts;
pub mod auth;
mod azure_cli;
pub mod backend;
pub mod contracts;
pub mod credentials;
pub mod dispatch;
pub mod error;
pub mod extensions;
pub mod jobs;
pub mod local_ipc;
pub mod oauth;
pub mod policy;
pub mod results;
pub mod runtime;
pub mod security;
pub mod sessions;
pub mod storage;
pub mod transport;
pub use runtime::ServiceRuntime;

pub use contracts::{SchemaValidator, ServiceRequest, ServiceResponse};
pub use dispatch::{CallerContext, CallerTransport, ServiceDispatcher, ServiceHandler};

#[cfg(test)]
mod tests;
