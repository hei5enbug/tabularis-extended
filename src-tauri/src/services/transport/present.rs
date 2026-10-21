//! Turns service responses into MCP tool results that stay under 64 KiB.
//!
//! Row pages carry whole rows: as many as fit, with `next_offset` pointing at
//! the first row left out and the page token resuming there. Catalog lists
//! carry whole entries the same way. Every other payload keeps its arrays
//! whole while the response fits, and is sampled when it does not.

use super::super::{
    contracts::{ErrorCode, JobStatus, Operation, Page, ServiceError, ServiceResponse},
    error::{failure, redact_error},
};
use serde_json::{json, Map, Value};

pub(crate) const MAX_PRESENTATION: usize = 64 * 1024;
const DATA_BUDGET: usize = 12 * 1024;
const COMPLETE_BUDGET: usize = 48 * 1024;
const COMPLETE_NODES: usize = 50_000;
const SAMPLE_ITEMS: usize = 5;
/// Arrays that describe the call rather than hold its answer stay sampled.
const DIAGNOSTIC_KEYS: &[&str] = &["warnings", "errors", "diagnostics", "reasons", "notices"];

struct Presentation {
    budget: usize,
    nodes: usize,
    max_nodes: usize,
    array_cap: usize,
    truncated: bool,
}
impl Presentation {
    fn sampled() -> Self {
        Self::with(DATA_BUDGET, 2048, SAMPLE_ITEMS)
    }
    fn complete() -> Self {
        Self::with(COMPLETE_BUDGET, COMPLETE_NODES, usize::MAX)
    }
    fn with(budget: usize, max_nodes: usize, array_cap: usize) -> Self {
        Self {
            budget,
            nodes: 0,
            max_nodes,
            array_cap,
            truncated: false,
        }
    }
    fn sample(&mut self, value: &Value, depth: usize) -> Value {
        let cap = std::mem::replace(&mut self.array_cap, SAMPLE_ITEMS);
        let sampled = self.value(value, depth);
        self.array_cap = cap;
        sampled
    }
    fn value(&mut self, value: &Value, depth: usize) -> Value {
        if depth >= 32 || self.nodes >= self.max_nodes || self.budget < 16 {
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
                self.truncated |= items.len() > self.array_cap;
                self.budget = self.budget.saturating_sub(16);
                Value::Array(
                    items
                        .iter()
                        .take(self.array_cap)
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
                        result.insert(format!("{key}_sample"), self.sample(value, depth + 1));
                    } else if DIAGNOSTIC_KEYS.contains(&key.as_str()) {
                        result.insert(key.clone(), self.sample(value, depth + 1));
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
    page: &Page,
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
        "page":page,
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

/// Where the page token resumes after rows were left out: given the absolute
/// offset of the first omitted row, the token that returns exactly from it, or
/// `None` when no such token exists.
pub(crate) type Resume<'a> = &'a dyn Fn(usize) -> Option<String>;

/// A page whose rows are kept whole. Rows are dropped from the end until the
/// whole result fits, `next_offset` points at the first row dropped and the
/// page token resumes there.
fn page_result(
    operation: Operation,
    response: &ServiceResponse,
    effective: Option<&Value>,
    resume: Resume,
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
    // Sized like the token that will replace it, so the search below cannot
    // pick a row count the final page no longer fits.
    let probe = Page {
        next_token: Some(format!("t-{}", "0".repeat(36))),
        has_more: true,
        resume_mode: response.page.resume_mode,
    };
    let build = |kept: usize, page: &Page| {
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
                page,
            ),
            false,
        )
    };
    let page_for = |kept: usize| {
        if kept < rows.len() {
            &probe
        } else {
            &response.page
        }
    };
    let (mut low, mut high) = (0usize, rows.len());
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        if oversized(&build(middle, page_for(middle))) {
            high = middle - 1;
        } else {
            low = middle;
        }
    }
    if low == 0 && !rows.is_empty() {
        return Some(too_large(response));
    }
    if low < rows.len() {
        let page = Page {
            next_token: resume(offset as usize + low),
            has_more: true,
            resume_mode: response.page.resume_mode,
        };
        return Some(build(low, &page));
    }
    Some(build(low, &response.page))
}

/// The array a catalog tool lists, which `offset` and `next_offset` page.
fn list_field(operation: Operation) -> Option<&'static str> {
    match operation {
        Operation::CatalogDatabases => Some("databases"),
        Operation::CatalogSchemas => Some("schemas"),
        Operation::CatalogObjects => Some("objects"),
        Operation::CatalogDescribe => Some("columns"),
        _ => None,
    }
}

/// A catalog list whose entries are kept whole: as many as fit from `offset`,
/// with `next_offset` naming the first entry left out.
fn list_result(
    operation: Operation,
    response: &ServiceResponse,
    effective: Option<&Value>,
    offset: u64,
) -> Option<Value> {
    let field = list_field(operation)?;
    if response.error.is_some() || response.status != JobStatus::Succeeded {
        return None;
    }
    let object = response.data.as_object()?;
    let items = object.get(field)?.as_array()?;
    let start = usize::try_from(offset).unwrap_or(usize::MAX).min(items.len());
    let mut other = Map::new();
    let mut other_truncated = false;
    for (key, value) in object.iter().filter(|(key, _)| key.as_str() != field) {
        let mut presentation = Presentation::sampled();
        other.insert(key.clone(), presentation.value(value, 1));
        other_truncated |= presentation.truncated;
    }
    let sanitized: Vec<(Value, bool)> = items[start..]
        .iter()
        .map(|item| {
            let mut presentation = Presentation::with(DATA_BUDGET, 2048, SAMPLE_ITEMS);
            (presentation.value(item, 1), presentation.truncated)
        })
        .collect();
    let build = |kept: usize| {
        let mut data = other.clone();
        let next = start + kept;
        data.insert(
            field.into(),
            Value::Array(sanitized[..kept].iter().map(|(item, _)| item.clone()).collect()),
        );
        data.insert("offset".into(), json!(start));
        data.insert("total_count".into(), json!(items.len()));
        data.insert("next_offset".into(), json!((next < items.len()).then_some(next)));
        data.insert("has_more".into(), json!(next < items.len()));
        let truncated = other_truncated
            || next < items.len()
            || sanitized[..kept].iter().any(|(_, truncated)| *truncated);
        render(
            &summary(
                operation,
                response,
                Value::Object(data),
                truncated,
                effective,
                &response.page,
            ),
            false,
        )
    };
    let (mut low, mut high) = (0usize, sanitized.len());
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        if oversized(&build(middle)) {
            high = middle - 1;
        } else {
            low = middle;
        }
    }
    if low == 0 && !sanitized.is_empty() {
        return Some(too_large(response));
    }
    Some(build(low))
}

pub(crate) fn tool_result(
    operation: Operation,
    response: &ServiceResponse,
    effective: Option<&Value>,
) -> Value {
    tool_result_resuming(operation, response, effective, 0, &|_| None)
}

/// `offset` starts a catalog list at that entry; `resume` re-issues the page
/// token of a row page that had to leave rows out.
pub(crate) fn tool_result_resuming(
    operation: Operation,
    response: &ServiceResponse,
    effective: Option<&Value>,
    offset: u64,
    resume: Resume,
) -> Value {
    if let Some(page) = page_result(operation, response, effective, resume) {
        return page;
    }
    if let Some(list) = list_result(operation, response, effective, offset) {
        return list;
    }
    let is_error = response.error.is_some();
    // Arrays stay whole while the response fits; only then is a payload
    // sampled.
    for mut presentation in [Presentation::complete(), Presentation::sampled()] {
        let complete = presentation.array_cap == usize::MAX;
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
        let mut summary = summary(
            operation,
            response,
            data,
            presentation.truncated,
            effective,
            &response.page,
        );
        let mut output = render(&summary, is_error);
        if !oversized(&output) {
            return output;
        }
        if !complete {
            summary["data"] = json!({"omitted":true});
            summary["presentation_truncated"] = json!(true);
            summary["warnings"] = json!([]);
            output = render(&summary, is_error);
            if oversized(&output) {
                output = too_large(response);
            }
            return output;
        }
    }
    too_large(response)
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
