use super::{types::FRAME_LIMIT, ServiceError};
use mssql_tds::{
    connection::tds_client::{ResultSet, ResultSetClient, TdsClient},
    core::CancelHandle,
    datatypes::{sql_string::SqlString, sqltypes::SqlType},
    message::parameters::rpc_parameters::{RpcParameter, StatusFlags},
};
use mssql_tiberius_bridge::{row::RowSchema, Row};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashSet;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExactInput {
    pub text: String,
    #[serde(default)]
    pub parameters: Vec<Parameter>,
    #[serde(default = "max_rows")]
    pub max_rows: usize,
    #[serde(default = "max_bytes")]
    pub max_bytes: usize,
    #[serde(default)]
    pub schema: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Parameter {
    pub name: String,
    pub value: Value,
}
fn max_rows() -> usize {
    10_000
}
fn max_bytes() -> usize {
    32 * 1024 * 1024
}
pub fn validate_text(text: &str) -> Result<(), ServiceError> {
    validate_leading_word(text, true)
}
pub fn validate_text_for(text: &str, _read_only: bool) -> Result<(), ServiceError> {
    validate_leading_word(text, false)
}
/// Rejects text that opens with transaction or session control, which the
/// service runs through its session RPCs. `strict` also rejects a leading
/// SET, for text that must not change the connection at all.
fn validate_leading_word(text: &str, strict: bool) -> Result<(), ServiceError> {
    let bytes = text.as_bytes();
    let mut index = 0;
    loop {
        while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
            index += 1;
        }
        if bytes.get(index..index + 2) == Some(b"--") {
            while bytes.get(index).is_some_and(|b| *b != b'\n') {
                index += 1;
            }
        } else if bytes.get(index..index + 2) == Some(b"/*") {
            index += 2;
            let mut depth = 1;
            while depth > 0 {
                if index >= bytes.len() {
                    return Err(ServiceError::argument("Unterminated SQL comment"));
                }
                if bytes.get(index..index + 2) == Some(b"/*") {
                    depth += 1;
                    index += 2;
                } else if bytes.get(index..index + 2) == Some(b"*/") {
                    depth -= 1;
                    index += 2;
                } else {
                    index += 1;
                }
            }
        } else {
            break;
        }
    }
    let start = index;
    while bytes
        .get(index)
        .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
    {
        index += 1;
    }
    let word = String::from_utf8_lossy(&bytes[start..index]).to_ascii_uppercase();
    let rest = &text[index..];
    let next = rest.trim_start();
    let next: String = next
        .chars()
        .take_while(|c| c.is_ascii_alphabetic() || *c == '_')
        .collect::<String>()
        .to_ascii_uppercase();
    let opens_transaction = word == "BEGIN"
        && matches!(
            next.as_str(),
            "TRAN" | "TRANSACTION" | "DISTRIBUTED" | "DIALOG"
        );
    let session_set = word == "SET" && (strict || next == "IMPLICIT_TRANSACTIONS");
    if opens_transaction
        || session_set
        || matches!(
            word.as_str(),
            "START"
                | "COMMIT"
                | "END"
                | "ROLLBACK"
                | "ABORT"
                | "SAVE"
                | "SAVEPOINT"
                | "RELEASE"
                | "RESET"
                | "DISCARD"
                | "USE"
        )
    {
        return Err(ServiceError::new(
            "UNSUPPORTED_OPERATION",
            "Transaction and session control requires service session RPCs",
        ));
    }
    if text.trim().is_empty() {
        return Err(ServiceError::argument("SQL text is required"));
    }
    Ok(())
}
pub fn bind(parameters: &[Parameter]) -> Result<Vec<RpcParameter>, ServiceError> {
    let mut names = HashSet::new();
    parameters
        .iter()
        .map(|parameter| {
            let name = parameter.name.as_bytes();
            if name.len() < 2
                || name.len() > 128
                || name[0] != b'@'
                || !(name[1].is_ascii_alphabetic() || name[1] == b'_')
                || !name[2..]
                    .iter()
                    .all(|v| v.is_ascii_alphanumeric() || *v == b'_')
                || !names.insert(parameter.name.to_ascii_lowercase())
            {
                return Err(ServiceError::argument(
                    "Parameters require unique ASCII @identifier names",
                ));
            }
            Ok(RpcParameter::new(
                Some(parameter.name.clone()),
                StatusFlags::NONE,
                value_type(&parameter.value)?,
            ))
        })
        .collect()
}
pub fn value_type(value: &Value) -> Result<SqlType, ServiceError> {
    let unsupported = || {
        ServiceError::new(
            "UNSUPPORTED_TYPE",
            "This SQL parameter value is unsupported",
        )
    };
    Ok(match value {
        Value::Null => SqlType::NVarcharMax(None),
        Value::Bool(value) => SqlType::Bit(Some(*value)),
        Value::String(value) => {
            SqlType::NVarcharMax(Some(SqlString::from_utf8_string(value.clone())))
        }
        Value::Number(value) => {
            if let Some(integer) = value.as_i64() {
                if integer.unsigned_abs() > 9_007_199_254_740_991 {
                    return Err(unsupported());
                }
                SqlType::BigInt(Some(integer))
            } else {
                let number = value
                    .as_f64()
                    .filter(|v| v.is_finite())
                    .ok_or_else(unsupported)?;
                if number.fract() == 0.0 && number.abs() > 9_007_199_254_740_991.0 {
                    return Err(unsupported());
                }
                SqlType::Float(Some(number))
            }
        }
        _ => return Err(unsupported()),
    })
}
pub fn validate(input: &ExactInput, read_only: bool) -> Result<Vec<RpcParameter>, ServiceError> {
    if input
        .schema
        .as_deref()
        .is_some_and(|schema| !schema.is_empty())
    {
        return Err(ServiceError::new(
            "UNSUPPORTED_OPERATION",
            "SQL Server has no per-session default schema; names must be schema-qualified",
        ));
    }
    validate_text_for(&input.text, read_only)?;
    if input.max_rows == 0
        || input.max_rows > max_rows()
        || input.max_bytes == 0
        || input.max_bytes > max_bytes()
    {
        return Err(ServiceError::argument(
            "Exact query limits exceed supported bounds",
        ));
    }
    bind(&input.parameters)
}
pub(super) fn decode(
    row: &Row,
    index: usize,
    column_type: mssql_tiberius_bridge::ColumnType,
) -> Result<Value, ServiceError> {
    let unavailable = || {
        ServiceError::new(
            "UNSUPPORTED_TYPE",
            "The result value cannot be represented without loss",
        )
    };
    let value = crate::driver::extract::extract_value_as(row, index, column_type)
        .map_err(|_| unavailable())?;
    if value.is_null()
        && !matches!(
            row.raw_value(index),
            None | Some(mssql_tds::datatypes::column_values::ColumnValues::Null)
        )
    {
        return Err(unavailable());
    }
    Ok(value)
}
fn metadata_fields(
    metadata: &[mssql_tds::query::metadata::ColumnMetadata],
) -> (Vec<String>, Vec<Value>, bool) {
    let precision = metadata.iter().any(|column| {
        matches!(
            column.data_type,
            mssql_tds::datatypes::sqldatatypes::TdsDataType::Decimal
                | mssql_tds::datatypes::sqldatatypes::TdsDataType::Numeric
                | mssql_tds::datatypes::sqldatatypes::TdsDataType::DecimalN
                | mssql_tds::datatypes::sqldatatypes::TdsDataType::NumericN
        )
    });
    let columns = metadata
        .iter()
        .map(|column| column.column_name.clone())
        .collect();
    let fields = metadata.iter().enumerate().map(|(index, column)| json!({"name":column.column_name,"column_index":index,"native_type":format!("{:?}",column.data_type),"nullable":column.is_nullable(),"length":column.type_info.length,"precision":null,"scale":column.get_scale(),"srid":null,"dimensions":null})).collect();
    (columns, fields, precision)
}
fn finish(
    mut sets: Vec<Value>,
    affected: Value,
    count_sentinel: bool,
    precision: bool,
    reason: Option<&str>,
) -> Value {
    let mut first = if sets.is_empty() {
        json!({"columns":[],"column_metadata":[],"rows":[]})
    } else {
        sets.remove(0)
    };
    first["additional_results"] = if sets.is_empty() {
        Value::Null
    } else {
        json!(sets)
    };
    first["affected_rows"] = affected;
    first["truncated"] = json!(reason.is_some());
    first["limits"] =
        json!({"truncated":reason.is_some(),"reasons":reason.into_iter().collect::<Vec<_>>()});
    let mut warnings = Vec::new();
    if !count_sentinel {
        warnings.push("affected_rows_unavailable");
    }
    if precision {
        warnings.push("column_precision_unavailable");
    }
    first["warnings"] = json!(warnings);
    first
}
pub async fn collect_read_prefix(
    client: &mut TdsClient,
    text: &str,
    parameters: Vec<RpcParameter>,
    cancel: CancelHandle,
    max_rows: usize,
    max_bytes: usize,
    response_overhead: usize,
) -> Result<Value, ServiceError> {
    let limit = max_bytes
        .min(FRAME_LIMIT)
        .checked_sub(response_overhead)
        .ok_or_else(|| {
            ServiceError::new(
                "RESOURCE_LIMIT",
                "The response metadata exceeds its byte limit",
            )
        })?;
    client.close_query().await.map_err(ServiceError::database)?;
    client
        .execute_sp_executesql(text.to_owned(), parameters, None, Some(&cancel))
        .await
        .map_err(ServiceError::database)?;
    let mut sets: Vec<Value> = Vec::new();
    let mut rows = 0;
    let mut bytes = 0;
    let mut precision = false;
    let mut reason = None;
    'results: while let Some(result) = client.get_current_resultset() {
        let metadata = result.get_metadata().clone();
        let schema = RowSchema::from_metadata(&metadata);
        let (columns, fields, current_precision) = metadata_fields(&metadata);
        if current_precision && !precision && !sets.is_empty() {
            bytes += serde_json::to_vec("column_precision_unavailable")
                .unwrap()
                .len()
                + 1;
        }
        precision |= current_precision;
        let empty = json!({"columns":columns,"column_metadata":fields,"rows":[]});
        let standalone = serde_json::to_vec(&finish(
            vec![empty.clone()],
            Value::Null,
            false,
            precision,
            None,
        ))
        .unwrap()
        .len();
        bytes += match sets.len() {
            0 => standalone,
            1 => serde_json::to_vec(&empty).unwrap().len() - 2,
            _ => serde_json::to_vec(&empty).unwrap().len() + 1,
        };
        if bytes + 5 > limit {
            return Err(ServiceError::new(
                "RESOURCE_LIMIT",
                "The response metadata exceeds its byte limit",
            ));
        }
        sets.push(empty);
        while let Some(values) = result.next_row().await.map_err(ServiceError::database)? {
            if rows == max_rows {
                reason = Some("rows");
                break 'results;
            }
            let row = Row::from_schema(schema.clone(), values);
            let values = metadata
                .iter()
                .enumerate()
                .map(|(index, column)| {
                    decode(
                        &row,
                        index,
                        crate::driver::extract::normalized_column_type(
                            column.data_type,
                            column.type_info.length,
                        ),
                    )
                })
                .collect::<Result<Vec<_>, _>>()?;
            let row_bytes = serde_json::to_vec(&values).unwrap().len();
            if standalone + row_bytes + 5 > limit {
                return Err(ServiceError::new(
                    "RESOURCE_LIMIT",
                    "A single SQL row exceeds its byte limit",
                ));
            }
            let stored = sets.last_mut().unwrap()["rows"].as_array_mut().unwrap();
            let increase = row_bytes + usize::from(!stored.is_empty());
            if bytes + increase + 5 > limit {
                reason = Some("bytes");
                break 'results;
            }
            stored.push(json!(values));
            bytes += increase;
            rows += 1;
        }
        if !client
            .move_to_next()
            .await
            .map_err(ServiceError::database)?
        {
            break;
        }
    }
    let data = finish(sets, Value::Null, false, precision, reason);
    if serde_json::to_vec(&data).unwrap().len() > limit {
        return Err(ServiceError::new(
            "RESOURCE_LIMIT",
            "The actual SQL prefix exceeds its byte limit",
        ));
    }
    if reason.is_some() {
        cancel.cancel();
    }
    Ok(data)
}
pub async fn collect(
    client: &mut TdsClient,
    text: &str,
    parameters: Vec<RpcParameter>,
    cancel: Option<&CancelHandle>,
    max_rows: usize,
    max_bytes: usize,
    count_sentinel: bool,
) -> Result<Value, ServiceError> {
    collect_with(
        client,
        text,
        parameters,
        cancel,
        max_rows,
        max_bytes,
        count_sentinel,
        false,
    )
    .await
}
/// Like `collect`; with `batch` and no parameters the text runs as a plain
/// SQL batch instead of through sp_executesql, so temporary tables and SET
/// options it creates stay on the connection for the statements after it.
#[allow(clippy::too_many_arguments)]
pub async fn collect_with(
    client: &mut TdsClient,
    text: &str,
    parameters: Vec<RpcParameter>,
    cancel: Option<&CancelHandle>,
    max_rows: usize,
    max_bytes: usize,
    count_sentinel: bool,
    batch: bool,
) -> Result<Value, ServiceError> {
    client.close_query().await.map_err(ServiceError::database)?;
    if batch && parameters.is_empty() {
        client.execute(text.to_owned(), None, cancel).await
    } else {
        client
            .execute_sp_executesql(text.to_owned(), parameters, None, cancel)
            .await
    }
    .map_err(ServiceError::database)?;
    let mut sets = Vec::new();
    let mut row_count = 0;
    let mut bytes = 0;
    let mut affected = Value::Null;
    let mut precision_unavailable = false;
    while let Some(result) = client.get_current_resultset() {
        let metadata = result.get_metadata().clone();
        let (columns, column_metadata, precision) = metadata_fields(&metadata);
        precision_unavailable |= precision;
        let schema = RowSchema::from_metadata(&metadata);
        let sentinel = count_sentinel
            && columns.len() == 1
            && columns[0] == crate::driver::helpers::AFFECTED_ROWS_COLUMN;
        let mut rows = Vec::new();
        bytes += serde_json::to_vec(&json!({"columns":columns,"column_metadata":column_metadata}))
            .unwrap()
            .len();
        while let Some(values) = result.next_row().await.map_err(ServiceError::database)? {
            let row = Row::from_schema(schema.clone(), values);
            let values = metadata
                .iter()
                .enumerate()
                .map(|(index, c)| {
                    decode(
                        &row,
                        index,
                        crate::driver::extract::normalized_column_type(
                            c.data_type,
                            c.type_info.length,
                        ),
                    )
                })
                .collect::<Result<Vec<_>, _>>()?;
            if sentinel {
                affected = values.first().cloned().unwrap_or(Value::Null);
            } else {
                row_count += 1;
                bytes += serde_json::to_vec(&values).unwrap().len() + 1;
                if row_count > max_rows || bytes > max_bytes.min(FRAME_LIMIT - 4096) {
                    return Err(ServiceError::new(
                        "RESOURCE_LIMIT",
                        "The complete SQL result exceeds its row or byte limit",
                    ));
                }
                rows.push(values);
            }
        }
        if !sentinel {
            sets.push(json!({"columns":columns,"column_metadata":column_metadata,"rows":rows}));
        }
        if bytes > max_bytes.min(FRAME_LIMIT - 4096) {
            return Err(ServiceError::new(
                "RESOURCE_LIMIT",
                "SQL column metadata exceeds the response byte limit",
            ));
        }
        if !client
            .move_to_next()
            .await
            .map_err(ServiceError::database)?
        {
            break;
        }
    }
    let first = finish(sets, affected, count_sentinel, precision_unavailable, None);
    if serde_json::to_vec(&first).unwrap().len() > max_bytes.min(FRAME_LIMIT - 4096) {
        return Err(ServiceError::new(
            "RESOURCE_LIMIT",
            "The complete SQL response exceeds its byte limit",
        ));
    }
    Ok(first)
}
