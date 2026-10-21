use super::contracts::{ErrorCode, Operation, SchemaValidator, ServiceRequest, ServiceResponse};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallerTransport {
    Gui,
    Mcp,
    Cli,
    LocalIpc,
}

#[derive(Debug, Clone)]
pub struct CallerContext {
    pub transport: CallerTransport,
    pub caller_id: String,
    pub owner_instance_id: String,
}

#[async_trait]
pub trait ServiceHandler: Send + Sync {
    async fn call(&self, request: &ServiceRequest, caller: &CallerContext) -> ServiceResponse;
    fn rejected(&self, _request: &ServiceRequest, _caller: &CallerContext) {}
}

pub struct ServiceDispatcher {
    pub validator: SchemaValidator,
    handlers: HashMap<Operation, Arc<dyn ServiceHandler>>,
}

impl ServiceDispatcher {
    pub fn new() -> Result<Self, String> {
        Ok(Self {
            validator: SchemaValidator::new()?,
            handlers: HashMap::new(),
        })
    }

    pub fn register(&mut self, operation: Operation, handler: Arc<dyn ServiceHandler>) {
        self.handlers.insert(operation, handler);
    }
    pub fn registered_operations(&self) -> Vec<Operation> {
        let mut operations: Vec<_> = self.handlers.keys().copied().collect();
        operations.sort_by_key(|operation| {
            serde_json::to_value(operation)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default()
        });
        operations
    }

    pub async fn dispatch(
        &self,
        request: &ServiceRequest,
        caller: &CallerContext,
    ) -> ServiceResponse {
        let value = match serde_json::to_value(request) {
            Ok(value) => value,
            Err(_) => {
                return ServiceResponse::failure(
                    request,
                    ErrorCode::InvalidArgument,
                    "The service request is invalid.",
                )
            }
        };
        if self.validator.validate_request(&value).is_err() {
            if let Some(handler) = self.handlers.get(&request.operation) {
                handler.rejected(request, caller);
            }
            return ServiceResponse::failure(
                request,
                if request.protocol_version != 1 {
                    ErrorCode::ProtocolMismatch
                } else {
                    ErrorCode::InvalidArgument
                },
                "The service request does not match protocol version 1.",
            );
        }
        match self.handlers.get(&request.operation) {
            Some(handler) => handler.call(request, caller).await,
            None => ServiceResponse::failure(
                request,
                ErrorCode::CapabilityUnavailable,
                "The requested service handler is unavailable.",
            ),
        }
    }
}
