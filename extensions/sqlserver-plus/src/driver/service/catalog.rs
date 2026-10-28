mod describe;
#[cfg(test)]
mod tests;

use super::{
    exact::{self, Parameter},
    ServiceError,
};
use crate::models::ConnectionParams;
use serde::Deserialize;
use serde_json::{json, Value};
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TableRef {
    pub database: Option<String>,
    pub schema: Option<String>,
    pub table: String,
}
impl TableRef {
    pub fn validate(&self, params: &ConnectionParams) -> Result<(), ServiceError> {
        if self
            .database
            .as_ref()
            .is_some_and(|database| database != params.database.primary())
        {
            return Err(ServiceError::argument(
                "The table database does not match the physical connection",
            ));
        }
        for value in [self.schema.as_deref().unwrap_or("dbo"), self.table.as_str()] {
            if value.is_empty() || value.contains('\0') || value.chars().count() > 128 {
                return Err(ServiceError::argument("Invalid SQL Server identifier"));
            }
        }
        Ok(())
    }
    pub fn qualified(&self) -> String {
        crate::driver::helpers::qualify(self.schema.as_deref(), &self.table)
    }
}
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Table,
    View,
    Routine,
    Trigger,
    Sequence,
    Synonym,
    Type,
    Rule,
    SecurityPolicy,
}
#[derive(Deserialize)]
#[serde(tag = "operation", deny_unknown_fields)]
pub enum Input {
    #[serde(rename = "catalog.databases")]
    Databases,
    #[serde(rename = "catalog.schemas")]
    Schemas { database: String },
    #[serde(rename = "catalog.objects")]
    Objects {
        database: String,
        schema: Option<String>,
    },
    #[serde(rename = "catalog.describe")]
    Describe { table: TableRef, kind: Option<Kind> },
}
/// Object types the legacy `get_routines` lists: T-SQL and CLR procedures and functions.
const ROUTINE_TYPES: [&str; 7] = ["P", "PC", "FN", "FS", "FT", "IF", "TF"];
/// DML trigger types: T-SQL and CLR.
const TRIGGER_TYPES: [&str; 2] = ["TR", "TA"];

fn type_list(types: &[&str]) -> String {
    types
        .iter()
        .map(|kind| format!("'{kind}'"))
        .collect::<Vec<_>>()
        .join(",")
}
/// Relations, routines, table-level triggers, sequences, synonyms, rules,
/// security policies and user-defined types in one parameterized statement.
/// The host filters the returned rows by `kind`, so every kind is enumerated
/// here.
pub fn objects_sql() -> String {
    let routines = type_list(&ROUTINE_TYPES);
    let triggers = type_list(&TRIGGER_TYPES);
    format!(
        "SELECT schema_name,name,kind,[table] FROM (\
SELECT s.name AS schema_name,o.name,\
CASE WHEN o.type='V' THEN 'view' WHEN o.type IN ({routines}) THEN 'routine' WHEN o.type IN ({triggers}) THEN 'trigger' \
WHEN o.type='SO' THEN 'sequence' WHEN o.type='SN' THEN 'synonym' WHEN o.type='R' THEN 'rule' WHEN o.type='SP' THEN 'security_policy' ELSE 'table' END AS kind,\
CASE WHEN o.type IN ({triggers}) THEN p.name END AS [table] \
FROM sys.objects o JOIN sys.schemas s ON o.schema_id=s.schema_id \
LEFT JOIN sys.triggers tr ON tr.object_id=o.object_id AND tr.parent_class=1 \
LEFT JOIN sys.objects p ON p.object_id=tr.parent_id \
WHERE o.type IN ('U','V',{routines},{triggers},'SO','SN','R','SP') AND o.is_ms_shipped=0 \
AND (o.type NOT IN ({triggers}) OR tr.object_id IS NOT NULL) \
AND (@schema IS NULL OR s.name=@schema) \
UNION ALL \
SELECT s.name,ty.name,'type',CAST(NULL AS sysname) \
FROM sys.types ty JOIN sys.schemas s ON ty.schema_id=s.schema_id \
WHERE ty.is_user_defined=1 AND (@schema IS NULL OR s.name=@schema)) objects \
ORDER BY CASE WHEN kind IN ('table','view') THEN 0 WHEN kind='routine' THEN 1 WHEN kind='trigger' THEN 2 ELSE 3 END,schema_name,name,[table]"
    )
}
pub struct Query {
    pub text: String,
    pub parameters: Vec<exact::Parameter>,
    pub key: &'static str,
    pub kind: Option<Kind>,
}
pub fn query(input: &Input, params: &ConnectionParams) -> Result<Query, ServiceError> {
    let db = |value: &str| {
        if value != params.database.primary() {
            Err(ServiceError::argument(
                "Catalog database must match the physical connection",
            ))
        } else {
            Ok(())
        }
    };
    let mut describe_kind = None;
    let (text, parameters, key) = match input {
        Input::Databases if crate::connection::is_azure_sql(params) => ("SELECT DB_NAME() AS name".to_owned(), vec![], "databases"),
        Input::Databases => ("SELECT name FROM sys.databases WHERE database_id>4 AND HAS_DBACCESS(name)=1 ORDER BY name".to_owned(), vec![], "databases"),
        Input::Schemas { database } => { db(database)?; ("SELECT name FROM sys.schemas WHERE schema_id<16384 AND name NOT IN ('sys','INFORMATION_SCHEMA','guest') ORDER BY name".to_owned(), vec![], "schemas") }
        Input::Objects { database, schema } => { db(database)?; (objects_sql(), vec![Parameter { name: "@schema".into(), value: json!(schema) }], "objects") }
        Input::Describe { table, kind } => {
            table.validate(params)?;
            let kind = kind.unwrap_or(Kind::Table);
            describe_kind = Some(kind);
            (describe::sql(kind), describe::parameters(table, kind), "describe")
        }
    };
    Ok(Query {
        text,
        parameters,
        key,
        kind: describe_kind,
    })
}
pub fn result(query: &Query, data: &Value) -> Value {
    if let Some(kind) = query.kind {
        return describe::result(kind, data);
    }
    let rows = data["rows"].as_array().unwrap();
    if matches!(query.key, "databases" | "schemas") {
        return json!({query.key: rows.iter().map(|row| row[0].clone()).collect::<Vec<_>>()});
    }
    let columns = data["columns"].as_array().unwrap();
    json!({query.key:rows.iter().map(|row| columns.iter().zip(row.as_array().unwrap()).map(|(name,value)| (name.as_str().unwrap().to_owned(),value.clone())).collect::<serde_json::Map<String,Value>>()).collect::<Vec<_>>()})
}
