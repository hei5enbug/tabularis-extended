//! Turns service responses into MCP tool results that stay under 64 KiB.
//!
//! Row pages carry whole rows: as many as fit, with `next_offset` pointing at
//! the first row left out. Every other payload is sampled.

use super::super::{
    contracts::{ErrorCode, JobStatus, Operation, ServiceError, ServiceResponse},
    error::{failure, redact_error},
};
use serde_json::{json, Map, Value};

pub(crate) const MAX_PRESENTATION: usize = 64 * 1024;
const DATA_BUDGET: usize = 12 * 1024;

struct Presentation {
    budget: usize,
    nodes: usize,
    truncated: bool,
}
impl Presentation {
    fn value(&mut self, value: &Value, depth: usize) -> Value {
        if depth >= 32 || self.nodes >= 2048 || self.budget < 16 {
            self.truncated = true;
            return Value::Null;
        }
        self.nodes += 1;
        match value {
            Value::String(text) => {
                let maximum = self.budget.min(1024);
                let mut end = text.len().min(maximum);
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                self.truncated |= end < text.len();
                self.budget = self.budget.saturating_sub(end + 8);
                json!(&text[..end])
            }
            Value::Array(items) => {
                self.truncated |= items.len() > 5;
                self.budget = self.budget.saturating_sub(16);
                Value::Array(
                    items
                        .iter()
                        .take(5)
                        .map(|value| self.value(value, depth + 1))
                        .collect(),
                )
            }
            Value::Object(object) => {
                self.budget = self.budget.saturating_sub(16);
                let mut result = Map::new();
                for (key, value) in object.iter().take(64) {
                    if self.budget < key.len() + 16 || key.len() > 256 {
                        self.truncated = true;
                        continue;
                    }
                    self.budget -= key.len() + 8;
                    if key == "coordinates" && object.contains_key("type") {
                        self.truncated = true;
                        continue;
                    }
                    if matches!(key.as_str(), "native_state" | "tail") {
                        self.truncated = true;
                        continue;
                    }
                    if key == "geometry" && value.is_object() {
                        result.insert(
                            key.clone(),
                            json!({"type":self.value(&value["type"],depth+1)}),
                        );
                        self.truncated |= value.get("coordinates").is_some();
                        continue;
                    }
                    if matches!(key.as_str(), "rows" | "documents" | "values" | "features")
                        && value.is_array()
                    {
                        let count = value.as_array().map_or(0, Vec::len);
                        result.insert(format!("{key}_count"), json!(count));
                        result.insert(format!("{key}_sample"), self.value(value, depth + 1));
                    } else {
                        result.insert(key.clone(), self.value(value, depth + 1));
                    }
                }
                self.truncated |= object.len() > 64;
                Value::Object(result)
            }
            _ => {
                self.budget = self.budget.saturating_sub(32);
                value.clone()
            }
        }
    }
}

/// The status a caller sees. Job control calls report the job's own status;
/// a job that waits for a human reads `awaiting_approval` instead of
/// `running`.
fn status_text(operation: Operation, response: &ServiceResponse) -> Value {
    let status = if matches!(operation, Operation::JobGet | Operation::JobCancel) {
        serde_json::from_value(response.data["status"].clone()).unwrap_or(response.status)
    } else {
        response.status
    };
    if matches!(status, JobStatus::Queued | JobStatus::Running)
        && response.data["awaiting_approval"] == true
    {
        return json!("awaiting_approval");
    }
    json!(status)
}

fn render(summary: &Value, is_error: bool) -> Value {
    json!({"content":[{"type":"text","text":summary.to_string()}],"structuredContent":summary,"isError":is_error})
}

fn oversized(output: &Value) -> bool {
    serde_json::to_vec(output).map_or(true, |bytes| bytes.len() > MAX_PRESENTATION)
}

fn summary(
    operation: Operation,
    response: &ServiceResponse,
    data: Value,
    presentation_truncated: bool,
    effective: Option<&Value>,
) -> Value {
    json!({
        "protocol_version":response.protocol_version,
        "request_id":response.request_id,
        "connection_id":response.connection_id,
        "status":status_text(operation,response),
        "job_id":response.job_id,
        "owner_instance_id":response.data.get("owner_instance_id").filter(|owner| owner.is_string()),
        "result_id":response.result_id,
        "effective":effective,
        "data":data,
        "page":response.page,
        "limits":response.limits,
        "metrics":response.metrics,
        "warnings":response.warnings.iter().take(16).map(|warning| { let mut end=warning.len().min(256); while !warning.is_char_boundary(end) {end-=1;} warning[..end].to_owned() }).collect::<Vec<_>>(),
        "error":response.error.clone().map(redact_error),
        "presentation_truncated":presentation_truncated,
    })
}

fn too_large(response: &ServiceResponse) -> Value {
    let error = redact_error(failure(ErrorCode::ResourceLimit));
    let safe = json!({"request_id":response.request_id,"connection_id":response.connection_id,"status":response.status,"job_id":response.job_id,"result_id":response.result_id,"presentation_truncated":true,"presentation_error":error});
    json!({"content":[{"type":"text","text":safe.to_string()}],"structuredContent":safe,"isError":true})
}

fn rows_field(data: &Value) -> Option<&'static str> {
    ["rows", "documents", "values"]
        .into_iter()
        .find(|field| data.get(*field).is_some_and(Value::is_array))
}

/// A page whose rows are kept whole. Rows are dropped from the end until the
/// whole result fits, and `next_offset` points at the first row dropped.
fn page_result(
    operation: Operation,
    response: &ServiceResponse,
    effective: Option<&Value>,
) -> Option<Value> {
    if !matches!(
        operation,
        Operation::QueryExecute | Operation::QueryNext | Operation::ResultGet
    ) || response.error.is_some()
        || response.status != JobStatus::Succeeded
    {
        return None;
    }
    let field = rows_field(&response.data)?;
    let rows = response.data[field].as_array()?;
    let offset = response.data["offset"].as_u64().unwrap_or(0);
    let mut data = Map::new();
    for key in [
        "kind",
        "columns",
        "result_set_index",
        "result_set_count",
        "result_revision",
        "affected_rows",
        "origin",
    ] {
        if let Some(value) = response.data.get(key) {
            data.insert(key.into(), value.clone());
        }
    }
    data.insert("offset".into(), json!(offset));
    data.insert(
        "stored_row_count".into(),
        response.data["stored_row_count"].clone(),
    );
    data.insert("truncated".into(), json!(response.limits.truncated));
    data.insert(
        "truncation_reason".into(),
        json!(response.limits.reasons.first()),
    );
    data.insert(
        "readonly_guarantee".into(),
        response
            .data
            .get("readonly_guarantee")
            .cloned()
            .unwrap_or(Value::Null),
    );
    let build = |kept: usize| {
        let mut data = data.clone();
        let next = offset + kept as u64;
        data.insert(field.into(), Value::Array(rows[..kept].to_vec()));
        data.insert("next_offset".into(), json!(next));
        data.insert(
            "has_more".into(),
            json!(kept < rows.len() || response.page.has_more),
        );
        render(
            &summary(
                operation,
                response,
                Value::Object(data),
                kept < rows.len(),
                effective,
            ),
            false,
        )
    };
    let (mut low, mut high) = (0usize, rows.len());
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        if oversized(&build(middle)) {
            high = middle - 1;
        } else {
            low = middle;
        }
    }
    if low == 0 && !rows.is_empty() {
        return Some(too_large(response));
    }
    Some(build(low))
}

pub(crate) fn tool_result(
    operation: Operation,
    response: &ServiceResponse,
    effective: Option<&Value>,
) -> Value {
    if let Some(page) = page_result(operation, response, effective) {
        return page;
    }
    let mut presentation = Presentation {
        budget: DATA_BUDGET,
        nodes: 0,
        truncated: false,
    };
    let data = if matches!(
        operation,
        Operation::AuthBegin | Operation::AuthStatus | Operation::AuthCancel
    ) {
        let mut safe = Map::new();
        for key in ["auth_job_id", "state", "user_action_required", "error_code"] {
            if let Some(value) = response.data.get(key) {
                safe.insert(key.into(), presentation.value(value, 0));
            }
        }
        Value::Object(safe)
    } else {
        presentation.value(&response.data, 0)
    };
    let mut summary = summary(operation, response, data, presentation.truncated, effective);
    let is_error = response.error.is_some();
    let mut output = render(&summary, is_error);
    if oversized(&output) {
        summary["data"] = json!({"omitted":true});
        summary["presentation_truncated"] = json!(true);
        summary["warnings"] = json!([]);
        output = render(&summary, is_error);
        if oversized(&output) {
            output = too_large(response);
        }
    }
    output
}

/// A tool result for a failure that has no request to answer.
pub(crate) fn error_result(error: ServiceError) -> Value {
    let summary = json!({"status":"failed","effective":{"database":null,"schema":null},"data":null,"error":redact_error(error),"presentation_truncated":false});
    render(&summary, true)
}

/// A result that is not a service response, such as the job list. The caller
/// keeps it under the size limit.
pub(crate) fn raw_result(summary: &Value) -> Value {
    render(summary, false)
}
pub(crate) fn fits(output: &Value) -> bool {
    !oversized(output)
}
