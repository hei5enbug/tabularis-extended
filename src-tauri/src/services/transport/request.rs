use super::super::{
    contracts::{
        ErrorCode, JobStatus, Operation, Outcome, SchemaValidator, ServiceError, ServiceRequest,
        ServiceResponse,
    },
    dispatch::CallerContext,
    error::{failure, redact_error, ServiceResult},
};
use serde_json::{json, Value};
use std::io::{self, Write};

pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_FOREGROUND_MS: u64 = 122_000;

struct CountBytes(usize);
impl Write for CountBytes {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("limit"))?;
        if self.0 > MAX_FRAME_BYTES {
            return Err(io::Error::other("limit"));
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(crate) fn decode(validator: &SchemaValidator, raw: &Value) -> ServiceResult<ServiceRequest> {
    serde_json::to_writer(CountBytes(0), raw).map_err(|_| failure(ErrorCode::ResourceLimit))?;
    validator.decode_request(raw).map_err(|_| {
        failure(
            if raw
                .get("protocol_version")
                .is_some_and(|value| value != &json!(1) && value.as_f64() != Some(1.0))
            {
                ErrorCode::ProtocolMismatch
            } else {
                ErrorCode::InvalidArgument
            },
        )
    })
}

pub(crate) fn validate_caller(caller: &CallerContext) -> ServiceResult<()> {
    if caller.caller_id.is_empty()
        || caller.caller_id.len() > 512
        || caller.caller_id.chars().any(char::is_control)
    {
        return Err(failure(ErrorCode::PermissionDenied));
    }
    super::super::security::opaque_id(&caller.owner_instance_id)
}

pub(crate) fn terminal(status: JobStatus) -> bool {
    !matches!(status, JobStatus::Queued | JobStatus::Running)
}

pub(crate) fn checked_response(
    validator: &SchemaValidator,
    request: &ServiceRequest,
    response: ServiceResponse,
    require_terminal: bool,
) -> ServiceResult<ServiceResponse> {
    if response.protocol_version != 1
        || response.request_id != request.request_id
        || response.connection_id != request.connection_id
        || (require_terminal && !terminal(response.status))
    {
        return Err(failure(ErrorCode::OutcomeUnknown));
    }
    let raw = serde_json::to_value(&response).map_err(|_| failure(ErrorCode::ResourceLimit))?;
    serde_json::to_writer(CountBytes(0), &raw).map_err(|_| failure(ErrorCode::ResourceLimit))?;
    validator
        .validate_response(&raw)
        .map_err(|_| failure(ErrorCode::OutcomeUnknown))?;
    Ok(response)
}

pub(crate) fn error_response(request: &ServiceRequest, error: ServiceError) -> ServiceResponse {
    let error = redact_error(error);
    let mut response = ServiceResponse::failure(request, error.code, &error.message);
    response.status = if error.outcome == Outcome::Unknown {
        JobStatus::OutcomeUnknown
    } else if error.code == ErrorCode::Cancelled {
        JobStatus::Cancelled
    } else {
        JobStatus::Failed
    };
    response.error = Some(error);
    response
}

pub(crate) fn unknown_response(
    request: &ServiceRequest,
    job_id: Option<String>,
) -> ServiceResponse {
    let mut error = failure(ErrorCode::OutcomeUnknown);
    error.outcome = Outcome::Unknown;
    let mut response = error_response(request, error);
    response.job_id = job_id;
    response
}

pub(crate) fn rejected_raw(raw: &Value, error: ServiceError) -> ServiceResponse {
    let identity = |key: &str| {
        raw.get(key)
            .and_then(Value::as_str)
            .filter(|value| {
                !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
            })
            .map(str::to_owned)
    };
    let request = ServiceRequest {
        protocol_version: 1,
        operation: Operation::ConnectionList,
        request_id: identity("request_id").unwrap_or_else(|| "invalid-request".into()),
        connection_id: identity("connection_id"),
        session_id: None,
        map_id: None,
        expected_version: None,
        deadline_ms: 30_000,
        input: json!({}),
    };
    error_response(&request, error)
}

pub fn error_exit_code(error: &ServiceError) -> i32 {
    match error.code {
        ErrorCode::InvalidArgument | ErrorCode::ProtocolMismatch => 2,
        ErrorCode::PermissionDenied
        | ErrorCode::WriteNotAllowed
        | ErrorCode::AuthRequired
        | ErrorCode::AuthExpired
        | ErrorCode::InteractionRequired => 3,
        ErrorCode::Cancelled => 130,
        _ => 4,
    }
}

pub fn exit_code(response: &ServiceResponse) -> i32 {
    response.error.as_ref().map(error_exit_code).unwrap_or(
        if response.status == JobStatus::Succeeded {
            0
        } else {
            4
        },
    )
}

pub(crate) fn control_operation(operation: Operation) -> bool {
    matches!(
        operation,
        Operation::JobGet
            | Operation::JobResult
            | Operation::JobCancel
            | Operation::AuthStatus
            | Operation::AuthCancel
            | Operation::AuthLogout
    )
}

pub(crate) fn control_request(
    request: &ServiceRequest,
    operation: Operation,
    job_id: &str,
    owner: &str,
    remaining_ms: u64,
) -> ServiceRequest {
    ServiceRequest {
        protocol_version: 1,
        operation,
        request_id: uuid::Uuid::new_v4().to_string(),
        connection_id: request.connection_id.clone(),
        session_id: None,
        map_id: None,
        expected_version: None,
        deadline_ms: remaining_ms.clamp(1, 120_000),
        input: json!({"job_id":job_id,"owner_instance_id":owner}),
    }
}
