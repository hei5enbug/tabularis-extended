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
        Input::Objects { database, schema } => { db(database)?; ("SELECT s.name AS schema_name,o.name,CASE WHEN o.type='V' THEN 'view' ELSE 'table' END AS kind FROM sys.objects o JOIN sys.schemas s ON o.schema_id=s.schema_id WHERE o.type IN ('U','V') AND o.is_ms_shipped=0 AND (@schema IS NULL OR s.name=@schema) ORDER BY s.name,o.name".to_owned(), vec![Parameter { name: "@schema".into(), value: json!(schema) }], "objects") }
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
