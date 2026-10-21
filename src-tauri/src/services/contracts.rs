use jsonschema::{Draft, JSONSchema};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u8 = 1;
pub const DEFAULT_DEADLINE_MS: u64 = 30_000;
pub const MAX_DEADLINE_MS: u64 = 120_000;
pub const REQUEST_SCHEMA: &str =
    include_str!("../../../packages/service-contracts/schema/v1/request.json");
pub const RESPONSE_SCHEMA: &str =
    include_str!("../../../packages/service-contracts/schema/v1/response.json");
pub const VALIDATION_FIXTURES: &str =
    include_str!("../../../packages/service-contracts/fixtures/v1/validation.json");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Operation {
    #[serde(rename = "connection.list")]
    ConnectionList,
    #[serde(rename = "connection.create")]
    ConnectionCreate,
    #[serde(rename = "connection.update")]
    ConnectionUpdate,
    #[serde(rename = "connection.delete")]
    ConnectionDelete,
    #[serde(rename = "connection.test")]
    ConnectionTest,
    #[serde(rename = "catalog.databases")]
    CatalogDatabases,
    #[serde(rename = "catalog.schemas")]
    CatalogSchemas,
    #[serde(rename = "catalog.objects")]
    CatalogObjects,
    #[serde(rename = "catalog.describe")]
    CatalogDescribe,
    #[serde(rename = "query.execute")]
    QueryExecute,
    #[serde(rename = "query.next")]
    QueryNext,
    #[serde(rename = "result.get")]
    ResultGet,
    #[serde(rename = "result.release")]
    ResultRelease,
    #[serde(rename = "row.insert")]
    RowInsert,
    #[serde(rename = "row.update")]
    RowUpdate,
    #[serde(rename = "row.delete")]
    RowDelete,
    #[serde(rename = "document.read")]
    DocumentRead,
    #[serde(rename = "document.create")]
    DocumentCreate,
    #[serde(rename = "document.replace")]
    DocumentReplace,
    #[serde(rename = "document.delete")]
    DocumentDelete,
    #[serde(rename = "session.open")]
    SessionOpen,
    #[serde(rename = "session.commit")]
    SessionCommit,
    #[serde(rename = "session.rollback")]
    SessionRollback,
    #[serde(rename = "session.close")]
    SessionClose,
    #[serde(rename = "spatial.columns")]
    SpatialColumns,
    #[serde(rename = "spatial.query_result")]
    SpatialQueryResult,
    #[serde(rename = "spatial.table_query")]
    SpatialTableQuery,
    #[serde(rename = "spatial.feature")]
    SpatialFeature,
    #[serde(rename = "spatial.export")]
    SpatialExport,
    #[serde(rename = "map.create")]
    MapCreate,
    #[serde(rename = "map.update")]
    MapUpdate,
    #[serde(rename = "map.get")]
    MapGet,
    #[serde(rename = "map.list")]
    MapList,
    #[serde(rename = "map.open")]
    MapOpen,
    #[serde(rename = "map.close")]
    MapClose,
    #[serde(rename = "map.layer.add")]
    MapLayerAdd,
    #[serde(rename = "map.layer.remove")]
    MapLayerRemove,
    #[serde(rename = "map.layer.update")]
    MapLayerUpdate,
    #[serde(rename = "map.viewport.set")]
    MapViewportSet,
    #[serde(rename = "map.selection.set")]
    MapSelectionSet,
    #[serde(rename = "map.save")]
    MapSave,
    #[serde(rename = "job.get")]
    JobGet,
    #[serde(rename = "job.result")]
    JobResult,
    #[serde(rename = "job.cancel")]
    JobCancel,
    #[serde(rename = "auth.begin")]
    AuthBegin,
    #[serde(rename = "auth.status")]
    AuthStatus,
    #[serde(rename = "auth.cancel")]
    AuthCancel,
    #[serde(rename = "auth.logout")]
    AuthLogout,
    #[serde(rename = "credential.import")]
    CredentialImport,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceRequest {
    #[serde(deserialize_with = "deserialize_u8_integer")]
    pub protocol_version: u8,
    pub operation: Operation,
    pub request_id: String,
    #[serde(default)]
    pub connection_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub map_id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_integer")]
    pub expected_version: Option<u64>,
    #[serde(
        default = "default_deadline_ms",
        deserialize_with = "deserialize_u64_integer"
    )]
    pub deadline_ms: u64,
    pub input: Value,
}

fn default_deadline_ms() -> u64 {
    DEFAULT_DEADLINE_MS
}

fn json_integer(value: Value) -> Result<u64, String> {
    if let Some(number) = value.as_u64() {
        if number <= 9_007_199_254_740_991 {
            return Ok(number);
        }
    }
    if let Some(number) = value.as_f64() {
        if number.is_finite()
            && number >= 0.0
            && number <= 9_007_199_254_740_991.0
            && number.fract() == 0.0
        {
            return Ok(number as u64);
        }
    }
    Err("Expected a nonnegative JSON safe integer.".to_owned())
}

fn deserialize_u64_integer<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<u64, D::Error> {
    json_integer(Value::deserialize(deserializer)?).map_err(serde::de::Error::custom)
}

fn deserialize_u8_integer<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<u8, D::Error> {
    let number = deserialize_u64_integer(deserializer)?;
    u8::try_from(number).map_err(serde::de::Error::custom)
}

fn deserialize_optional_integer<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<u64>, D::Error> {
    let value = Value::deserialize(deserializer)?;
    if value.is_null() {
        Ok(None)
    } else {
        json_integer(value)
            .map(Some)
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Interrupted,
    OutcomeUnknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    NotStarted,
    NotApplied,
    Applied,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumeMode {
    None,
    Native,
    Materialized,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorCode {
    #[serde(rename = "DOCUMENT_NOT_FOUND")]
    DocumentNotFound,
    #[serde(rename = "INVALID_ARGUMENT")]
    InvalidArgument,
    #[serde(rename = "PROTOCOL_MISMATCH")]
    ProtocolMismatch,
    #[serde(rename = "CONNECTION_NOT_FOUND")]
    ConnectionNotFound,
    #[serde(rename = "PERMISSION_DENIED")]
    PermissionDenied,
    #[serde(rename = "WRITE_NOT_ALLOWED")]
    WriteNotAllowed,
    #[serde(rename = "CAPABILITY_UNAVAILABLE")]
    CapabilityUnavailable,
    #[serde(rename = "UNSUPPORTED_TYPE")]
    UnsupportedType,
    #[serde(rename = "UNSUPPORTED_OPERATION")]
    UnsupportedOperation,
    #[serde(rename = "AUTH_REQUIRED")]
    AuthRequired,
    #[serde(rename = "AUTH_EXPIRED")]
    AuthExpired,
    #[serde(rename = "INTERACTION_REQUIRED")]
    InteractionRequired,
    #[serde(rename = "TLS_VALIDATION_FAILED")]
    TlsValidationFailed,
    #[serde(rename = "SRID_REQUIRED")]
    SridRequired,
    #[serde(rename = "INVALID_GEOMETRY")]
    InvalidGeometry,
    #[serde(rename = "FEATURE_TOO_LARGE")]
    FeatureTooLarge,
    #[serde(rename = "VERSION_CONFLICT")]
    VersionConflict,
    #[serde(rename = "CONNECTION_CONFLICT")]
    ConnectionConflict,
    #[serde(rename = "ETAG_CONFLICT")]
    EtagConflict,
    #[serde(rename = "DOCUMENT_ALREADY_EXISTS")]
    DocumentAlreadyExists,
    #[serde(rename = "PARTITION_KEY_IMMUTABLE")]
    PartitionKeyImmutable,
    #[serde(rename = "DOCUMENT_TOO_LARGE")]
    DocumentTooLarge,
    #[serde(rename = "RESOURCE_LIMIT")]
    ResourceLimit,
    #[serde(rename = "INVALID_PAGE_TOKEN")]
    InvalidPageToken,
    #[serde(rename = "CURSOR_EXPIRED")]
    CursorExpired,
    #[serde(rename = "RESULT_EXPIRED")]
    ResultExpired,
    #[serde(rename = "RATE_LIMITED")]
    RateLimited,
    #[serde(rename = "DEADLINE_EXCEEDED")]
    DeadlineExceeded,
    #[serde(rename = "CANCELLED")]
    Cancelled,
    #[serde(rename = "GUI_UNAVAILABLE")]
    GuiUnavailable,
    #[serde(rename = "APPLY_TIMEOUT")]
    ApplyTimeout,
    #[serde(rename = "DRIVER_EXITED")]
    DriverExited,
    #[serde(rename = "OUTCOME_UNKNOWN")]
    OutcomeUnknown,
    #[serde(rename = "WRITE_INTENT_REQUIRED")]
    WriteIntentRequired,
    #[serde(rename = "SECRET_NOT_ACCEPTED")]
    SecretNotAccepted,
    #[serde(rename = "OPERATION_NOT_ALLOWED")]
    OperationNotAllowed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceError {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
    pub outcome: Outcome,
    pub details: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Page {
    pub next_token: Option<String>,
    pub has_more: bool,
    pub resume_mode: ResumeMode,
}

impl Default for Page {
    fn default() -> Self {
        Self {
            next_token: None,
            has_more: false,
            resume_mode: ResumeMode::None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub truncated: bool,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metrics {
    #[serde(deserialize_with = "deserialize_u64_integer")]
    pub elapsed_ms: u64,
    pub request_charge: Option<f64>,
    #[serde(deserialize_with = "deserialize_u64_integer")]
    pub retry_count: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceResponse {
    #[serde(deserialize_with = "deserialize_u8_integer")]
    pub protocol_version: u8,
    pub request_id: String,
    pub connection_id: Option<String>,
    pub status: JobStatus,
    pub job_id: Option<String>,
    pub result_id: Option<String>,
    pub data: Value,
    pub page: Page,
    pub limits: Limits,
    pub metrics: Metrics,
    pub warnings: Vec<String>,
    pub error: Option<ServiceError>,
}

impl ServiceResponse {
    pub fn failure(request: &ServiceRequest, code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            request_id: request.request_id.clone(),
            connection_id: request.connection_id.clone(),
            status: JobStatus::Failed,
            job_id: None,
            result_id: None,
            data: Value::Null,
            page: Page::default(),
            limits: Limits::default(),
            metrics: Metrics::default(),
            warnings: Vec::new(),
            error: Some(ServiceError {
                code,
                message: message.into(),
                retryable: false,
                outcome: Outcome::NotStarted,
                details: Value::Null,
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractViolation {
    pub instance_path: String,
    pub message: String,
}

pub struct SchemaValidator {
    request: JSONSchema,
    response: JSONSchema,
}

impl SchemaValidator {
    pub fn new() -> Result<Self, String> {
        let request: Value =
            serde_json::from_str(REQUEST_SCHEMA).map_err(|error| error.to_string())?;
        let response: Value =
            serde_json::from_str(RESPONSE_SCHEMA).map_err(|error| error.to_string())?;
        Ok(Self {
            request: JSONSchema::options()
                .with_draft(Draft::Draft7)
                .compile(&request)
                .map_err(|error| error.to_string())?,
            response: JSONSchema::options()
                .with_draft(Draft::Draft7)
                .compile(&response)
                .map_err(|error| error.to_string())?,
        })
    }

    pub fn validate_request(&self, value: &Value) -> Result<(), Vec<ContractViolation>> {
        Self::validate(&self.request, value)
    }
    pub fn validate_response(&self, value: &Value) -> Result<(), Vec<ContractViolation>> {
        Self::validate(&self.response, value)
    }

    pub fn decode_request(&self, value: &Value) -> Result<ServiceRequest, Vec<ContractViolation>> {
        self.validate_request(value)?;
        serde_json::from_value(value.clone()).map_err(|error| {
            vec![ContractViolation {
                instance_path: String::new(),
                message: error.to_string(),
            }]
        })
    }

    pub fn operation_names(&self) -> Vec<String> {
        let schema: Value = serde_json::from_str(REQUEST_SCHEMA).expect("compiled request schema");
        schema["properties"]["operation"]["enum"]
            .as_array()
            .expect("operation registry")
            .iter()
            .map(|name| name.as_str().expect("operation name").to_owned())
            .collect()
    }

    pub fn input_schema(&self, operation: Operation) -> Value {
        let schema: Value = serde_json::from_str(REQUEST_SCHEMA).expect("compiled request schema");
        let name = serde_json::to_value(operation).expect("operation serialization");
        let mut input = schema["definitions"]
            [format!("input.{}", name.as_str().expect("operation name"))]
        .clone();
        input["$schema"] = schema["$schema"].clone();
        input["definitions"] = schema["definitions"].clone();
        input
    }

    fn validate(schema: &JSONSchema, value: &Value) -> Result<(), Vec<ContractViolation>> {
        schema.validate(value).map_err(|errors| {
            errors
                .map(|error| ContractViolation {
                    instance_path: error.instance_path.to_string(),
                    message: error.to_string(),
                })
                .collect()
        })
    }
}
