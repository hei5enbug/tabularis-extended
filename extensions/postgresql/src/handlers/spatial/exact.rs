use futures_util::{pin_mut, TryStreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_postgres::{types::Type, Client};

use super::{catalog::Catalog, SpatialError};
use crate::{binding::PgParam, utils::identifiers::quote_identifier};

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
fn max_rows() -> usize {
    10_000
}
fn max_bytes() -> usize {
    32 * 1024 * 1024
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Parameter {
    pub name: String,
    pub value: Value,
}

pub fn search_path_statement(schema: Option<&str>) -> Result<Option<String>, SpatialError> {
    let Some(schema) = schema else {
        return Ok(None);
    };
    if schema.is_empty() || schema.contains('\0') {
        return Err(SpatialError::argument(
            "Schema must be a nonempty name without NUL characters",
        ));
    }
    Ok(Some(format!(
        "SET LOCAL search_path TO {}",
        quote_identifier(schema)
    )))
}

pub fn validate_text(text: &str) -> Result<(), SpatialError> {
    fn skip(bytes: &[u8], offset: &mut usize) -> Result<(), SpatialError> {
        loop {
            while bytes.get(*offset).is_some_and(u8::is_ascii_whitespace) {
                *offset += 1;
            }
            if bytes.get(*offset..*offset + 2) == Some(b"--") {
                *offset += 2;
                while bytes.get(*offset).is_some_and(|b| *b != b'\n') {
                    *offset += 1;
                }
            } else if bytes.get(*offset..*offset + 2) == Some(b"/*") {
                *offset += 2;
                let mut depth = 1usize;
                while depth > 0 {
                    if *offset >= bytes.len() {
                        return Err(SpatialError::argument("Unterminated SQL comment"));
                    }
                    if bytes.get(*offset..*offset + 2) == Some(b"/*") {
                        depth += 1;
                        *offset += 2;
                    } else if bytes.get(*offset..*offset + 2) == Some(b"*/") {
                        depth -= 1;
                        *offset += 2;
                    } else {
                        *offset += 1;
                    }
                }
            } else {
                return Ok(());
            }
        }
    }
    fn word(bytes: &[u8], offset: &mut usize) -> Result<String, SpatialError> {
        skip(bytes, offset)?;
        let start = *offset;
        while bytes
            .get(*offset)
            .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
        {
            *offset += 1;
        }
        Ok(String::from_utf8_lossy(&bytes[start..*offset]).to_ascii_uppercase())
    }
    let mut offset = 0;
    let first = word(text.as_bytes(), &mut offset)?;
    let blocked = matches!(
        first.as_str(),
        "BEGIN"
            | "START"
            | "COMMIT"
            | "END"
            | "ROLLBACK"
            | "ABORT"
            | "SAVEPOINT"
            | "RELEASE"
            | "SET"
            | "RESET"
            | "DISCARD"
    ) || (first == "PREPARE" && word(text.as_bytes(), &mut offset)? == "TRANSACTION");
    if blocked {
        return Err(SpatialError::new(
            "UNSUPPORTED_OPERATION",
            "Transaction/session control requires the service session RPCs",
        ));
    }
    if text.trim().is_empty() {
        return Err(SpatialError::argument("SQL text is required"));
    }
    Ok(())
}

fn bind(value: &Value, ty: &Type) -> Result<PgParam, SpatialError> {
    let invalid =
        || SpatialError::argument("Parameter value does not match the PostgreSQL parameter type");
    macro_rules! scalar {
        ($type:ty,$convert:expr) => {{
            if value.is_null() {
                return Ok(Box::new(None::<$type>));
            }
            let typed: $type = $convert.ok_or_else(invalid)?;
            return Ok(Box::new(typed));
        }};
    }
    match *ty {
        Type::BOOL => scalar!(bool, value.as_bool()),
        Type::INT2 => scalar!(
            i16,
            value
                .as_i64()
                .or_else(|| value.as_str().and_then(|v| v.parse().ok()))
                .and_then(|v| i16::try_from(v).ok())
        ),
        Type::INT4 => scalar!(
            i32,
            value
                .as_i64()
                .or_else(|| value.as_str().and_then(|v| v.parse().ok()))
                .and_then(|v| i32::try_from(v).ok())
        ),
        Type::INT8 => scalar!(
            i64,
            value
                .as_i64()
                .or_else(|| value.as_str().and_then(|v| v.parse().ok()))
        ),
        Type::FLOAT4 => scalar!(
            f32,
            value.as_f64().map(|v| v as f32).filter(|v| v.is_finite())
        ),
        Type::FLOAT8 => scalar!(f64, value.as_f64().filter(|v| v.is_finite())),
        Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME => {
            scalar!(String, value.as_str().map(str::to_string))
        }
        Type::UUID => scalar!(uuid::Uuid, value.as_str().and_then(|v| v.parse().ok())),
        Type::NUMERIC => scalar!(
            rust_decimal::Decimal,
            value
                .as_str()
                .map(str::to_string)
                .or_else(|| value.is_number().then(|| value.to_string()))
                .and_then(|v| v.parse().ok())
        ),
        Type::DATE => scalar!(
            chrono::NaiveDate,
            value.as_str().and_then(|v| v.parse().ok())
        ),
        Type::TIME => scalar!(
            chrono::NaiveTime,
            value.as_str().and_then(|v| v.parse().ok())
        ),
        Type::TIMESTAMP => scalar!(
            chrono::NaiveDateTime,
            value.as_str().and_then(|v| v.parse().ok())
        ),
        Type::TIMESTAMPTZ => scalar!(
            chrono::DateTime<chrono::Utc>,
            value.as_str().and_then(|v| v.parse().ok())
        ),
        Type::JSON | Type::JSONB => {
            if value.is_null() {
                Ok(Box::new(None::<Value>))
            } else {
                Ok(Box::new(value.clone()))
            }
        }
        _ => Err(SpatialError::argument(
            "Unsupported PostgreSQL parameter type",
        )),
    }
}

pub async fn execute(
    client: &Client,
    catalog: Option<&Catalog>,
    input: &ExactInput,
    scope: &crate::cancel::ScopedGuard,
    read_only: bool,
    reserved: bool,
) -> Result<Value, SpatialError> {
    validate_text(&input.text)?;
    let search_path = search_path_statement(input.schema.as_deref())?;
    if input.max_rows == 0
        || input.max_rows > max_rows()
        || input.max_bytes == 0
        || input.max_bytes > max_bytes()
    {
        return Err(SpatialError::argument(
            "Exact query limits exceed the supported bounds",
        ));
    }
    for (index, parameter) in input.parameters.iter().enumerate() {
        if parameter.name != format!("${}", index + 1) {
            return Err(SpatialError::argument(
                "Parameters must use ordered unique contiguous $1..$n names",
            ));
        }
    }
    let previous_search_path = match &search_path {
        Some(set_local) if reserved => {
            let row = client
                .query_one("SELECT pg_catalog.current_setting('search_path')", &[])
                .await
                .map_err(SpatialError::database)?;
            let previous: String = row.get(0);
            client
                .batch_execute(set_local)
                .await
                .map_err(SpatialError::database)?;
            Some(previous)
        }
        Some(set_local) => {
            client
                .batch_execute(set_local)
                .await
                .map_err(SpatialError::database)?;
            None
        }
        None => None,
    };
    let statement = client
        .prepare(&input.text)
        .await
        .map_err(SpatialError::database)?;
    if statement.params().len() != input.parameters.len() {
        return Err(SpatialError::argument("Parameter count does not match SQL"));
    }
    if let Some(catalog) = catalog {
        catalog.validate_columns(statement.columns())?;
    }
    let values = input
        .parameters
        .iter()
        .zip(statement.params())
        .map(|(parameter, ty)| bind(&parameter.value, ty))
        .collect::<Result<Vec<_>, _>>()?;
    let references = values
        .iter()
        .map(|v| v.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync))
        .collect::<Vec<_>>();
    let columns = statement
        .columns()
        .iter()
        .map(|c| c.name())
        .collect::<Vec<_>>();
    let mut column_metadata=statement.columns().iter().enumerate().map(|(index,c)|json!({"name":c.name(),"column_index":index,
        "type_oid":c.type_().oid(),"native_type":catalog.and_then(|catalog|catalog.native_type(c.type_())),
        "srid":null,"dimensions":null})).collect::<Vec<_>>();
    if let Some(catalog) = catalog {
        for (index, column) in statement.columns().iter().enumerate() {
            if catalog.native_type(column.type_()).is_some() && column.type_modifier() >= 0 {
                let query = format!(
                    "SELECT {}($1::integer),{}($1::integer)",
                    catalog.function("postgis_typmod_srid"),
                    catalog.function("postgis_typmod_type")
                );
                let meta = client
                    .query_one(&query, &[&column.type_modifier()])
                    .await
                    .map_err(SpatialError::database)?;
                let srid: i32 = meta.get(0);
                let shape: String = meta.get(1);
                column_metadata[index]["srid"] = json!((srid != 0).then_some(srid));
                column_metadata[index]["dimensions"] = json!(if shape.ends_with("ZM") {
                    "XYZM"
                } else if shape.ends_with('Z') {
                    "XYZ"
                } else if shape.ends_with('M') {
                    "XYM"
                } else {
                    "XY"
                });
            }
        }
    }
    let envelope = json!({"columns":columns,"column_metadata":column_metadata,"rows":[],"affected_rows":u64::MAX,
        "truncated":true,"pagination":null,"limits":{"truncated":true,"reasons":["ROW_LIMIT","BYTE_LIMIT"]},
        "page":{"next_token":null,"has_more":false,"resume_mode":"none"}});
    let mut rows = Vec::new();
    let mut bytes = serde_json::to_vec(&envelope)
        .map_err(|_| SpatialError::argument("Cannot encode query columns"))?
        .len();
    if bytes > input.max_bytes {
        return Err(SpatialError::new(
            "RESOURCE_LIMIT",
            "Query column metadata exceeds the byte limit",
        ));
    }
    let mut reasons = Vec::new();
    let affected;
    client
        .batch_execute("SAVEPOINT exact_query_stream")
        .await
        .map_err(SpatialError::database)?;
    {
        let stream = client
            .query_raw(&statement, references)
            .await
            .map_err(SpatialError::database)?;
        pin_mut!(stream);
        while let Some(row) = stream.try_next().await.map_err(SpatialError::database)? {
            if rows.len() >= input.max_rows {
                reasons.push("ROW_LIMIT");
                break;
            }
            let row = row
                .columns()
                .iter()
                .enumerate()
                .map(|(index, _)| crate::extract::extract_value_with_catalog(&row, index, catalog))
                .collect::<Result<Vec<_>, _>>()?;
            let size = serde_json::to_vec(&row)
                .map_err(|_| SpatialError::argument("Cannot encode query row"))?
                .len();
            let size = size + usize::from(!rows.is_empty());
            if bytes + size > input.max_bytes {
                reasons.push("BYTE_LIMIT");
                break;
            }
            bytes += size;
            rows.push(row);
        }
        affected = stream.rows_affected().unwrap_or(0);
    }
    if !reasons.is_empty() {
        scope.stop_backend().await;
        if !read_only || reserved {
            let mut error = SpatialError::new(
                "RESOURCE_LIMIT",
                "Query result exceeded its limit; the full transaction was rolled back",
            );
            if reserved {
                error.details = json!({"session_closed":true});
            }
            return Err(error);
        }
    } else {
        client
            .batch_execute("RELEASE SAVEPOINT exact_query_stream")
            .await
            .map_err(SpatialError::database)?;
    }
    if let Some(previous) = previous_search_path {
        client
            .query_one(
                "SELECT pg_catalog.set_config('search_path',$1,true)",
                &[&previous],
            )
            .await
            .map_err(SpatialError::database)?;
    }
    Ok(
        json!({"columns":columns,"column_metadata":column_metadata,"rows":rows,"affected_rows":affected,
        "truncated":!reasons.is_empty(),"pagination":null,"limits":{"truncated":!reasons.is_empty(),"reasons":reasons},
        "page":{"next_token":null,"has_more":false,"resume_mode":"none"}}),
    )
}

#[cfg(test)]
#[path = "exact_tests.rs"]
mod exact_tests;
