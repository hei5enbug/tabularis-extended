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
    Describe { table: TableRef },
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
/// Relations, routines and table-level triggers in one parameterized
/// statement. The host filters the returned rows by `kind`, so every kind is
/// enumerated here.
pub fn objects_sql() -> String {
    let routines = type_list(&ROUTINE_TYPES);
    let triggers = type_list(&TRIGGER_TYPES);
    format!(
        "SELECT s.name AS schema_name,o.name,\
CASE WHEN o.type='V' THEN 'view' WHEN o.type IN ({routines}) THEN 'routine' WHEN o.type IN ({triggers}) THEN 'trigger' ELSE 'table' END AS kind,\
CASE WHEN o.type IN ({triggers}) THEN p.name END AS [table] \
FROM sys.objects o JOIN sys.schemas s ON o.schema_id=s.schema_id \
LEFT JOIN sys.triggers tr ON tr.object_id=o.object_id AND tr.parent_class=1 \
LEFT JOIN sys.objects p ON p.object_id=tr.parent_id \
WHERE o.type IN ('U','V',{routines},{triggers}) AND o.is_ms_shipped=0 \
AND (o.type NOT IN ({triggers}) OR tr.object_id IS NOT NULL) \
AND (@schema IS NULL OR s.name=@schema) \
ORDER BY CASE WHEN o.type IN ('U','V') THEN 0 WHEN o.type IN ({routines}) THEN 1 ELSE 2 END,s.name,o.name,p.name"
    )
}
pub struct Query {
    pub text: String,
    pub parameters: Vec<exact::Parameter>,
    pub key: &'static str,
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
    let (text, parameters, key) = match input {
        Input::Databases if crate::connection::is_azure_sql(params) => ("SELECT DB_NAME() AS name".to_owned(), vec![], "databases"),
        Input::Databases => ("SELECT name FROM sys.databases WHERE database_id>4 AND HAS_DBACCESS(name)=1 ORDER BY name".to_owned(), vec![], "databases"),
        Input::Schemas { database } => { db(database)?; ("SELECT name FROM sys.schemas WHERE schema_id<16384 AND name NOT IN ('sys','INFORMATION_SCHEMA','guest') ORDER BY name".to_owned(), vec![], "schemas") }
        Input::Objects { database, schema } => { db(database)?; (objects_sql(), vec![Parameter { name: "@schema".into(), value: json!(schema) }], "objects") }
        Input::Describe { table } => { table.validate(params)?; ("SELECT c.name,t.name AS data_type,c.max_length,c.precision,c.scale,c.is_nullable,c.is_identity,CAST(CASE WHEN EXISTS(SELECT 1 FROM sys.index_columns ic JOIN sys.indexes i ON ic.object_id=i.object_id AND ic.index_id=i.index_id WHERE ic.object_id=c.object_id AND ic.column_id=c.column_id AND i.is_primary_key=1) THEN 1 ELSE 0 END AS bit) AS is_pk,(SELECT CAST(ic.key_ordinal AS int) FROM sys.index_columns ic JOIN sys.indexes i ON ic.object_id=i.object_id AND ic.index_id=i.index_id WHERE ic.object_id=c.object_id AND ic.column_id=c.column_id AND i.is_primary_key=1 AND ic.key_ordinal>0) AS primary_key_position FROM sys.columns c JOIN sys.types t ON c.user_type_id=t.user_type_id WHERE c.object_id=OBJECT_ID(@table) ORDER BY c.column_id".to_owned(), vec![Parameter { name: "@table".into(), value: json!(table.qualified()) }], "columns") }
    };
    Ok(Query {
        text,
        parameters,
        key,
    })
}
pub fn result(query: &Query, data: &Value) -> Value {
    let rows = data["rows"].as_array().unwrap();
    if matches!(query.key, "databases" | "schemas") {
        return json!({query.key: rows.iter().map(|row| row[0].clone()).collect::<Vec<_>>()});
    }
    let columns = data["columns"].as_array().unwrap();
    json!({query.key:rows.iter().map(|row| columns.iter().zip(row.as_array().unwrap()).map(|(name,value)| (name.as_str().unwrap().to_owned(),value.clone())).collect::<serde_json::Map<String,Value>>()).collect::<Vec<_>>()})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 객체_목록은_계약이_정의한_종류_이름으로_routine과_trigger를_포함한다() {
        // given
        let sql = objects_sql();

        // when
        let actual = ["'view'", "'routine'", "'trigger'", "'table'"].map(|kind| sql.contains(kind));

        // then
        assert_eq!(actual, [true; 4]);
    }

    #[test]
    fn routine은_legacy_get_routines가_지원하는_형식만_열거한다() {
        // given
        let sql = objects_sql();

        // when
        let actual = sql.contains("o.type IN ('P','PC','FN','FS','FT','IF','TF')");

        // then
        assert!(actual);
        assert!(!sql.contains("'X'"));
    }

    #[test]
    fn trigger는_table_수준만_열거하고_부모_객체를_table로_반환한다() {
        // given
        let sql = objects_sql();

        // when
        let actual = (
            sql.contains("tr.parent_class=1"),
            sql.contains("LEFT JOIN sys.objects p ON p.object_id=tr.parent_id"),
            sql.contains("AS [table]"),
            sql.contains("tr.object_id IS NOT NULL"),
        );

        // then
        assert_eq!(actual, (true, true, true, true));
    }

    #[test]
    fn 객체_목록은_schema를_매개변수로만_받고_시스템_객체를_제외한다() {
        // given
        let sql = objects_sql();

        // when
        let parameters: Vec<&str> = sql
            .match_indices('@')
            .map(|(i, _)| &sql[i..i + 7])
            .collect();

        // then
        assert!(parameters.iter().all(|name| *name == "@schema"));
        assert!(sql.contains("o.is_ms_shipped=0"));
    }

    #[test]
    fn 객체_목록_질의는_schema를_하나의_매개변수로_묶는다() {
        // given
        let input = Input::Objects {
            database: "app".into(),
            schema: Some("sales".into()),
        };
        let params = ConnectionParams {
            database: crate::models::DatabaseSelection::Single("app".into()),
            ..Default::default()
        };

        // when
        let actual = query(&input, &params).unwrap();

        // then
        assert_eq!(actual.key, "objects");
        assert_eq!(actual.parameters.len(), 1);
        assert_eq!(actual.parameters[0].name, "@schema");
        assert_eq!(actual.parameters[0].value, json!("sales"));
    }
}
