//! The estimated plan `query.explain` returns: the compact summary the host
//! builds from the plan a driver hands back.

use crate::services::{
    contracts::ErrorCode,
    error::{failure, ServiceResult},
};
use roxmltree::{Document, Node};
use serde_json::{json, Value};

pub const POSTGRES_FORMAT: &str = "postgres-json";
pub const SQLSERVER_FORMAT: &str = "sqlserver-showplan-xml";
const MAX_NODES: usize = 40;
const MAX_SCANS: usize = 40;
const MAX_WARNINGS: usize = 16;
const MAX_MISSING_INDEXES: usize = 10;
const MAX_COLUMNS: usize = 8;
const MAX_TEXT: usize = 96;
const MAX_VISITED: usize = 200_000;
const MAX_XML_BYTES: usize = 16 * 1024 * 1024;
/// SQL Server's own limit on XML nesting is 128 levels, so no plan nests deeper.
const MAX_XML_DEPTH: i64 = 150;
const MAX_PLAN_BYTES: usize = 32 * 1024;
/// What the reply data may take of the 64 KiB tool reply.
const MAX_REPLY_BYTES: usize = 56 * 1024;

#[derive(Default)]
struct Summary {
    total_cost: Option<f64>,
    estimated_rows: Option<f64>,
    node_count: usize,
    nodes: Vec<Value>,
    full_scans: Vec<Value>,
    warnings: Vec<String>,
    missing_indexes: Vec<Value>,
}

struct PlanNode<'a> {
    depth: usize,
    operation: Option<&'a str>,
    object: Option<String>,
    index: Option<String>,
    rows: Option<f64>,
    cost: Option<f64>,
    full_scan: bool,
}

impl Summary {
    fn add(&mut self, node: PlanNode) {
        if self.node_count == 0 {
            self.total_cost = node.cost;
            self.estimated_rows = node.rows;
        }
        self.node_count += 1;
        let operation = node.operation.map(|text| short(text, MAX_TEXT));
        let object = node.object.map(|text| short(&text, MAX_TEXT));
        if node.full_scan && self.full_scans.len() < MAX_SCANS {
            self.full_scans.push(json!({
                "operation": operation,
                "object": object,
                "estimated_rows": number(node.rows),
            }));
        }
        if self.nodes.len() < MAX_NODES {
            self.nodes.push(json!({
                "depth": node.depth,
                "operation": operation,
                "object": object,
                "index": node.index.map(|text| short(&text, MAX_TEXT)),
                "estimated_rows": number(node.rows),
                "total_cost": number(node.cost),
            }));
        }
    }

    fn warn(&mut self, warning: String) {
        let warning = short(&warning, 2 * MAX_TEXT);
        if self.warnings.len() < MAX_WARNINGS && !self.warnings.contains(&warning) {
            self.warnings.push(warning);
        }
    }

    fn value(&self, cost_unit: &str) -> Value {
        json!({
            "total_cost": number(self.total_cost),
            "estimated_rows": number(self.estimated_rows),
            "cost_unit": cost_unit,
            "node_count": self.node_count,
            "nodes": self.nodes,
            "full_scans": self.full_scans,
            "warnings": self.warnings,
            "missing_indexes": self.missing_indexes,
        })
    }

    /// Drops list entries, the least certain first, until the summary fits.
    fn fit(&mut self, cost_unit: &str, budget: usize) {
        while serialized_len(&self.value(cost_unit)) > budget {
            if !self.missing_indexes.is_empty() {
                self.missing_indexes
                    .truncate(self.missing_indexes.len() / 2);
            } else if !self.full_scans.is_empty() {
                self.full_scans.truncate(self.full_scans.len() / 2);
            } else if !self.warnings.is_empty() {
                self.warnings.truncate(self.warnings.len() / 2);
            } else if !self.nodes.is_empty() {
                self.nodes.truncate(self.nodes.len() / 2);
            } else {
                break;
            }
        }
    }
}

fn serialized_len(value: &Value) -> usize {
    serde_json::to_vec(value).map_or(usize::MAX, |bytes| bytes.len())
}

fn number(value: Option<f64>) -> Value {
    match value {
        Some(value) if value.is_finite() && value.fract() == 0.0 && value.abs() < 9.0e15 => {
            json!(value as i64)
        }
        Some(value) if value.is_finite() => json!(value),
        _ => Value::Null,
    }
}

fn short(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

/// The reply of `query.explain`, built from the `{engine, format, payload}` a
/// driver returns. A payload that cannot be read leaves the summary fields it
/// would have filled empty.
pub fn plan_reply(raw: &Value) -> ServiceResult<Value> {
    let text = |field: &str| {
        raw.get(field)
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty() && text.len() <= 64)
    };
    let (Some(engine), Some(format)) = (text("engine"), text("format")) else {
        return Err(failure(ErrorCode::InvalidArgument));
    };
    let payload = raw.get("payload").unwrap_or(&Value::Null);
    let mut summary = Summary::default();
    let (cost_unit, plan) = match format {
        POSTGRES_FORMAT => {
            let plan = match payload {
                Value::String(text) => {
                    serde_json::from_str(text).unwrap_or_else(|_| payload.clone())
                }
                _ => payload.clone(),
            };
            postgres(&plan, &mut summary);
            ("postgres_planner_cost", plan)
        }
        SQLSERVER_FORMAT => {
            if let Value::String(xml) = payload {
                // T-SQL needs no `;` between statements, so a batch that planned
                // more than one statement was not the single statement asked for.
                if showplan(xml, &mut summary) > 1 {
                    return Err(failure(ErrorCode::InvalidArgument));
                }
            }
            ("sqlserver_estimated_subtree_cost", payload.clone())
        }
        _ => return Err(failure(ErrorCode::InvalidArgument)),
    };
    summary.fit(cost_unit, MAX_REPLY_BYTES / 2);
    let mut reply = json!({
        "engine": engine,
        "format": format,
        "executed": false,
        "summary": summary.value(cost_unit),
    });
    if !plan.is_null() {
        let plan_bytes = serialized_len(&plan);
        if plan_bytes <= MAX_PLAN_BYTES
            && serialized_len(&reply).saturating_add(plan_bytes) <= MAX_REPLY_BYTES
        {
            reply["plan"] = plan;
        } else {
            reply["plan_omitted"] = json!(true);
        }
    }
    Ok(reply)
}

fn postgres(plan: &Value, summary: &mut Summary) {
    let root = match plan {
        Value::Array(items) => items.first().and_then(|item| item.get("Plan")),
        _ => plan.get("Plan"),
    };
    let Some(root) = root.filter(|root| root.is_object()) else {
        return;
    };
    let mut pending = vec![(root, 0usize)];
    while let Some((node, depth)) = pending.pop() {
        if summary.node_count >= MAX_VISITED {
            break;
        }
        let operation = node.get("Node Type").and_then(Value::as_str);
        let name = ["Relation Name", "CTE Name", "Function Name"]
            .into_iter()
            .find_map(|key| node.get(key).and_then(Value::as_str));
        let object = name.map(|name| match node.get("Schema").and_then(Value::as_str) {
            Some(schema) => format!("{schema}.{name}"),
            None => name.to_owned(),
        });
        summary.add(PlanNode {
            depth,
            operation,
            object,
            index: node
                .get("Index Name")
                .and_then(Value::as_str)
                .map(str::to_owned),
            rows: node.get("Plan Rows").and_then(Value::as_f64),
            cost: node.get("Total Cost").and_then(Value::as_f64),
            full_scan: operation == Some("Seq Scan"),
        });
        if let Some(children) = node.get("Plans").and_then(Value::as_array) {
            pending.extend(
                children
                    .iter()
                    .rev()
                    .filter(|child| child.is_object())
                    .map(|child| (child, depth + 1)),
            );
        }
    }
}

fn attribute_number(node: Node, name: &str) -> Option<f64> {
    node.attribute(name)?.trim().parse().ok()
}

/// The object a plan operator works on, not the ones of the operators below it.
fn own_object<'a, 'input>(operator: Node<'a, 'input>) -> Option<Node<'a, 'input>> {
    let mut pending: Vec<_> = operator.children().filter(Node::is_element).collect();
    pending.reverse();
    while let Some(node) = pending.pop() {
        if node.has_tag_name("RelOp") {
            continue;
        }
        if node.has_tag_name("Object") {
            return Some(node);
        }
        let start = pending.len();
        pending.extend(node.children().filter(Node::is_element));
        pending[start..].reverse();
    }
    None
}

fn qualified(node: Node, parts: &[&str]) -> Option<String> {
    let found: Vec<&str> = parts
        .iter()
        .filter_map(|part| node.attribute(*part))
        .filter(|value| !value.is_empty())
        .collect();
    (!found.is_empty()).then(|| found.join("."))
}

/// True when the element nesting of `xml` goes deeper than `MAX_XML_DEPTH`.
/// The XML parser recurses with the nesting, so the text is checked first.
fn nested_too_deep(xml: &str) -> bool {
    let bytes = xml.as_bytes();
    let (mut depth, mut index) = (0i64, 0usize);
    while index < bytes.len() {
        match (bytes[index], bytes.get(index + 1)) {
            (b'<', Some(b'/')) => depth -= 1,
            (b'<', Some(b'?' | b'!')) => {}
            (b'<', Some(_)) => {
                depth += 1;
                if depth > MAX_XML_DEPTH {
                    return true;
                }
            }
            (b'/', Some(b'>')) => depth -= 1,
            _ => {}
        }
        index += 1;
    }
    false
}

/// Fills `summary` from a SHOWPLAN XML document and returns how many
/// statements its batch holds.
fn showplan(xml: &str, summary: &mut Summary) -> usize {
    if xml.len() > MAX_XML_BYTES || nested_too_deep(xml) {
        return 0;
    }
    let Ok(document) = Document::parse(xml) else {
        return 0;
    };
    let statements = document
        .descendants()
        .filter(|node| {
            node.has_tag_name("Statements")
                && node
                    .parent()
                    .is_some_and(|parent| parent.has_tag_name("Batch"))
        })
        .flat_map(|node| node.children().filter(Node::is_element))
        .count();
    for operator in document
        .descendants()
        .filter(|node| node.has_tag_name("RelOp"))
        .take(MAX_VISITED)
    {
        let physical = operator.attribute("PhysicalOp");
        let object = own_object(operator);
        summary.add(PlanNode {
            depth: operator
                .ancestors()
                .skip(1)
                .filter(|node| node.has_tag_name("RelOp"))
                .count(),
            operation: physical,
            object: object.and_then(|node| qualified(node, &["Schema", "Table"])),
            index: object
                .and_then(|node| node.attribute("Index"))
                .map(str::to_owned),
            rows: attribute_number(operator, "EstimateRows"),
            cost: attribute_number(operator, "EstimatedTotalSubtreeCost"),
            full_scan: matches!(physical, Some("Table Scan" | "Clustered Index Scan")),
        });
    }
    if let Some(statement) = document
        .descendants()
        .find(|node| node.has_tag_name("StmtSimple"))
    {
        summary.total_cost = summary
            .total_cost
            .or_else(|| attribute_number(statement, "StatementSubTreeCost"));
        summary.estimated_rows = summary
            .estimated_rows
            .or_else(|| attribute_number(statement, "StatementEstRows"));
    }
    for node in document.descendants().filter(Node::is_element) {
        match node.tag_name().name() {
            "StmtSimple" => {
                if let Some(reason) = node.attribute("StatementOptmEarlyAbortReason") {
                    summary.warn(format!("StatementOptmEarlyAbortReason={reason}"));
                }
            }
            "Warnings" => {
                for attribute in node.attributes() {
                    if attribute.value().eq_ignore_ascii_case("true") {
                        summary.warn(attribute.name().to_owned());
                    }
                }
                for warning in node.children().filter(Node::is_element) {
                    summary.warn(describe_warning(warning));
                }
            }
            "MissingIndexGroup" => {
                let impact = attribute_number(node, "Impact");
                for index in node
                    .children()
                    .filter(|child| child.has_tag_name("MissingIndex"))
                {
                    if summary.missing_indexes.len() >= MAX_MISSING_INDEXES {
                        break;
                    }
                    summary.missing_indexes.push(missing_index(index, impact));
                }
            }
            _ => {}
        }
    }
    statements
}

fn describe_warning(warning: Node) -> String {
    let name = warning.tag_name().name();
    let details: Vec<String> = warning
        .attributes()
        .filter(|attribute| attribute.name() != "Expression")
        .take(2)
        .map(|attribute| format!("{}={}", attribute.name(), short(attribute.value(), 48)))
        .collect();
    if details.is_empty() {
        name.to_owned()
    } else {
        format!("{name} ({})", details.join(", "))
    }
}

fn missing_index(index: Node, impact: Option<f64>) -> Value {
    let columns = |usage: &str| -> Vec<String> {
        index
            .children()
            .filter(|group| {
                group.has_tag_name("ColumnGroup") && group.attribute("Usage") == Some(usage)
            })
            .flat_map(|group| group.children().filter(|node| node.has_tag_name("Column")))
            .filter_map(|column| column.attribute("Name"))
            .take(MAX_COLUMNS)
            .map(|name| short(name, MAX_TEXT))
            .collect()
    };
    json!({
        "impact": number(impact),
        "table": qualified(index, &["Database", "Schema", "Table"]).map(|table| short(&table, MAX_TEXT)),
        "equality_columns": columns("EQUALITY"),
        "inequality_columns": columns("INEQUALITY"),
        "include_columns": columns("INCLUDE"),
    })
}

#[cfg(test)]
#[path = "tests/explain_summary.rs"]
mod tests;
