use tokio_postgres::types::{ToSql, Type};
use serde_json::Value;

use crate::utils::identifiers::{qualified,quote_identifier};
use super::{catalog::{Catalog,TableCatalog,SpatialColumn},table::TableInput,types::RowIdentity, SpatialError};

#[derive(Default)]
pub struct Sql {
    pub text: String,
    pub values: Vec<Box<dyn ToSql + Send + Sync>>,
    pub types: Vec<Type>,
}
impl Sql {
    pub fn new() -> Self { Self { text: String::new(),values: Vec::new(),types: Vec::new() } }
    pub fn push<T: ToSql + Send + Sync + 'static>(&mut self,value: T,ty: Type) -> String {
        self.values.push(Box::new(value));self.types.push(ty);format!("${}",self.values.len())
    }
    pub fn references(&self) -> Vec<&(dyn ToSql + Sync)> {
        self.values.iter().map(|value| value.as_ref() as &(dyn ToSql+Sync)).collect()
    }
}

pub fn identity_values(sql: &mut Sql, table: &TableCatalog, identity: &RowIdentity) -> Result<Vec<String>, SpatialError> {
    validate_keyset(table)?;
    if identity.columns != table.primary_key || identity.values.len()!=table.primary_key.len()
        || identity.values.iter().any(Value::is_null) || table.primary_key.is_empty() {
        return Err(SpatialError::argument("Identity must contain the ordered complete primary key"));
    }
    identity.columns.iter().zip(&identity.values).map(|(column,value)| {
        let ty = table.column_types.get(column).ok_or_else(|| SpatialError::argument("Primary key column does not exist"))?;
        let text = match value { Value::String(value)=>value.clone(),Value::Number(_) | Value::Bool(_)=>value.to_string(),
            _=>return Err(SpatialError::argument("Unsupported primary key value")) };
        let placeholder = sql.push(text,Type::TEXT);
        Ok(format!("CAST({placeholder} AS {ty})"))
    }).collect()
}

pub fn validate_keyset(table: &TableCatalog) -> Result<(),SpatialError> {
    if !table.keyset_supported {
        return Err(SpatialError::new("UNSUPPORTED_OPERATION","Primary key type does not support a stable scalar keyset binding"));
    }
    Ok(())
}

pub fn viewport_query(catalog: &Catalog, table: &TableCatalog,column: &SpatialColumn,input: &TableInput,
    keyset: Option<&RowIdentity>, count: usize) -> Result<Sql, SpatialError> {
    let viewport=&input.viewport;let source_srid=input.source_srid;let srid_filter=input.srid_filter;
    let schema=input.table.schema();let name=&input.table.table;
    let f = |name| catalog.function(name);
    let mut sql = Sql::new();
    let geography = column.native_type=="geography";
    let source = if let Some(known)=column.srid {
        if source_srid.is_some_and(|source| source!=known) || srid_filter.is_some_and(|filter| filter!=known) {
            return Err(SpatialError::argument("SRID override/filter conflicts with column SRID"));
        }
        known
    } else {
        let source = source_srid.filter(|s| (1..=999_999).contains(s))
            .ok_or_else(|| SpatialError::new("SRID_REQUIRED", "Mixed/unknown table SRIDs require source_srid and srid_filter"))?;
        let filter = srid_filter.filter(|s| (0..=999_999).contains(s))
            .ok_or_else(|| SpatialError::new("SRID_REQUIRED", "Mixed/unknown table SRIDs require source_srid and srid_filter"))?;
        if filter>0 && filter!=source { return Err(SpatialError::argument("source_srid conflicts with selected known SRID")); }
        source
    };
    let pieces = viewport.pieces(geography)?;
    let full_world=viewport.world && viewport.south== -90.0 && viewport.north==90.0;
    if geography && !full_world {
        return Err(SpatialError::new("UNSUPPORTED_OPERATION","Geography viewport coverage is only validated for the full world latitude range"));
    }
    if !full_world && !matches!(source,4326|3857) {
        return Err(SpatialError::new("UNSUPPORTED_OPERATION", "Viewport coverage is only validated for EPSG:4326 and EPSG:3857"));
    }
    if !full_world && source==3857 && (viewport.south< -85.05112878 || viewport.north>85.05112878) {
        return Err(SpatialError::new("UNSUPPORTED_OPERATION", "Viewport exceeds the validated EPSG:3857 latitude range"));
    }
    let col = quote_identifier(&column.column);
    let geometry_type = format!("{}.geometry",quote_identifier(&catalog.schema));
    let native = if geography { format!("{col}::{geometry_type}") } else { col.clone() };
    let source_parameter = sql.push(source,Type::INT4);
    let effective = if column.srid.is_none() {
        format!("CASE WHEN {}({native})=0 THEN {}({native},{source_parameter}) ELSE {native} END",f("st_srid"),f("st_setsrid"))
    } else { native.clone() };
    let mut predicates = vec![format!("{col} IS NOT NULL")];
    let filter = srid_filter.unwrap_or(source);
    let filter_parameter = sql.push(filter,Type::INT4);
    predicates.push(format!("{}({native})={filter_parameter}",f("st_srid")));
    if !full_world {
        let mut intersects = Vec::new();
        for [west,south,east,north] in pieces {
            let w=sql.push(west,Type::FLOAT8);let s=sql.push(south,Type::FLOAT8);
            let e=sql.push(east,Type::FLOAT8);let n=sql.push(north,Type::FLOAT8);
            let boundary = if west==east && south==north { format!("{}({}({w},{s}),4326)",f("st_setsrid"),f("st_makepoint")) }
                else if west==east || south==north { format!("{}({}({}({w},{s}),{}({e},{n})),4326)",f("st_setsrid"),f("st_makeline"),f("st_makepoint"),f("st_makepoint")) }
                else { format!("{}({w},{s},{e},{n},4326)",f("st_makeenvelope")) };
            let boundary = format!("{}({boundary},1.0)",f("st_segmentize"));
            if geography {
                intersects.push(format!("{}({col},({boundary})::{}.geography)",f("st_intersects"),quote_identifier(&catalog.schema)));
            } else {
                let boundary=format!("{}({boundary},{source_parameter})",f("st_transform"));
                intersects.push(format!("({native} && {boundary} AND {}({effective},{boundary}))",f("st_intersects")));
            }
        }
        predicates.push(format!("CASE WHEN {}({native})={filter_parameter} THEN ({}) ELSE false END",f("st_srid"),intersects.join(" OR ")));
    }
    if let Some(keyset)=keyset {
        let values = identity_values(&mut sql,table,keyset)?;
        let columns = table.primary_key.iter().map(|c|quote_identifier(c)).collect::<Vec<_>>();
        predicates.push(format!("({}) > ({})",columns.join(","),values.join(",")));
    }
    let order = if table.primary_key.is_empty() { String::new() } else {
        format!(" ORDER BY {}",table.primary_key.iter().map(|c|quote_identifier(c)).collect::<Vec<_>>().join(","))
    };
    let limit=sql.push(i64::try_from(count).map_err(|_| SpatialError::argument("Invalid row count"))?,Type::INT8);
    sql.text=format!("SELECT * FROM {} WHERE {}{order} LIMIT {limit}",qualified(schema,name),predicates.join(" AND "));
    Ok(sql)
}
