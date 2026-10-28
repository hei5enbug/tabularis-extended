use super::{Kind, TableRef};
use crate::driver::service::exact::Parameter;
use serde_json::{json, Map, Value};

type Row = Map<String, Value>;

const ROUTINE_TYPES: &str = "'P','PC','FN','FS','FT','IF','TF'";
const NO_DEFINITION: &str = "CAST(NULL AS nvarchar(max)) AS definition";

fn type_text(ty: &str, c: &str) -> String {
    format!(
        "CASE WHEN {ty}.name IN ('varchar','char','varbinary','binary') THEN {ty}.name+CASE WHEN {c}.max_length=-1 THEN '(max)' ELSE '('+CAST({c}.max_length AS varchar(10))+')' END \
WHEN {ty}.name IN ('nvarchar','nchar') THEN {ty}.name+CASE WHEN {c}.max_length=-1 THEN '(max)' ELSE '('+CAST({c}.max_length/2 AS varchar(10))+')' END \
WHEN {ty}.name IN ('decimal','numeric') THEN {ty}.name+'('+CAST({c}.precision AS varchar(10))+','+CAST({c}.scale AS varchar(10))+')' \
WHEN {ty}.name IN ('datetime2','datetimeoffset','time') THEN {ty}.name+'('+CAST({c}.scale AS varchar(10))+')' \
ELSE {ty}.name END"
    )
}

fn columns_sql(id: &str) -> String {
    format!(
        "SELECT c.name,t.name AS data_type,c.max_length,c.precision,c.scale,c.is_nullable,c.is_identity,\
CAST(CASE WHEN EXISTS(SELECT 1 FROM sys.index_columns ic JOIN sys.indexes i ON ic.object_id=i.object_id AND ic.index_id=i.index_id WHERE ic.object_id=c.object_id AND ic.column_id=c.column_id AND i.is_primary_key=1) THEN 1 ELSE 0 END AS bit) AS is_pk,\
(SELECT CAST(ic.key_ordinal AS int) FROM sys.index_columns ic JOIN sys.indexes i ON ic.object_id=i.object_id AND ic.index_id=i.index_id WHERE ic.object_id=c.object_id AND ic.column_id=c.column_id AND i.is_primary_key=1 AND ic.key_ordinal>0) AS primary_key_position,\
dc.definition AS default_value,c.is_computed,cc.definition AS computed_definition \
FROM sys.columns c JOIN sys.types t ON c.user_type_id=t.user_type_id \
LEFT JOIN sys.default_constraints dc ON dc.parent_object_id=c.object_id AND dc.parent_column_id=c.column_id \
LEFT JOIN sys.computed_columns cc ON cc.object_id=c.object_id AND cc.column_id=c.column_id \
WHERE c.object_id={id} ORDER BY c.column_id;"
    )
}

const FOREIGN_KEYS_SQL: &str = "SELECT fk.name,pc.name AS column_name,rs.name AS ref_schema,ro.name AS ref_table,rc.name AS ref_column,\
CASE fk.update_referential_action WHEN 0 THEN 'NO ACTION' WHEN 1 THEN 'CASCADE' WHEN 2 THEN 'SET NULL' ELSE 'SET DEFAULT' END AS on_update,\
CASE fk.delete_referential_action WHEN 0 THEN 'NO ACTION' WHEN 1 THEN 'CASCADE' WHEN 2 THEN 'SET NULL' ELSE 'SET DEFAULT' END AS on_delete \
FROM sys.foreign_keys fk JOIN sys.foreign_key_columns fkc ON fkc.constraint_object_id=fk.object_id \
JOIN sys.columns pc ON pc.object_id=fkc.parent_object_id AND pc.column_id=fkc.parent_column_id \
JOIN sys.columns rc ON rc.object_id=fkc.referenced_object_id AND rc.column_id=fkc.referenced_column_id \
JOIN sys.objects ro ON ro.object_id=fk.referenced_object_id JOIN sys.schemas rs ON rs.schema_id=ro.schema_id \
WHERE fk.parent_object_id=@id ORDER BY fk.name,fkc.constraint_column_id;";

const INDEXES_SQL: &str = "SELECT i.name,i.type_desc,i.is_unique,i.is_primary_key AS is_primary,i.filter_definition AS filter,\
kc.name AS constraint_name,kc.type AS constraint_type,c.name AS column_name,CAST(ic.key_ordinal AS int) AS key_ordinal,ic.is_included_column AS is_included \
FROM sys.indexes i JOIN sys.index_columns ic ON ic.object_id=i.object_id AND ic.index_id=i.index_id \
JOIN sys.columns c ON c.object_id=ic.object_id AND c.column_id=ic.column_id \
LEFT JOIN sys.key_constraints kc ON kc.parent_object_id=i.object_id AND kc.unique_index_id=i.index_id AND kc.type IN ('PK','UQ') \
WHERE i.object_id=@id AND i.index_id>0 AND i.is_hypothetical=0 AND i.name IS NOT NULL AND (ic.key_ordinal>0 OR ic.is_included_column=1) \
ORDER BY i.index_id,ic.is_included_column,ic.key_ordinal,ic.index_column_id;";

const CHECKS_SQL: &str =
    "SELECT name,definition FROM sys.check_constraints WHERE parent_object_id=@id ORDER BY name;";

const DEFINITION_SQL: &str = "SELECT m.definition FROM sys.sql_modules m WHERE m.object_id=@id;";

fn named(catalog_view: &str, alias: &str, filter: &str) -> String {
    format!(
        "(SELECT {alias}.object_id FROM {catalog_view} {alias} JOIN sys.schemas s ON s.schema_id={alias}.schema_id WHERE s.name=@schema AND {alias}.name=@name{filter})"
    )
}

fn routine_sql() -> String {
    let parameter = type_text("ty", "p");
    let result = type_text("ty", "c");
    let id = named(
        "sys.objects",
        "o",
        &format!(" AND o.type IN ({ROUTINE_TYPES})"),
    );
    format!(
        "DECLARE @id int={id};\
SELECT x.name,x.data_type,x.mode,x.ordinal_position FROM (\
SELECT p.name,{parameter} AS data_type,CASE WHEN p.parameter_id=0 THEN 'OUT' WHEN p.is_output=1 THEN 'INOUT' ELSE 'IN' END AS mode,CAST(p.parameter_id AS int) AS ordinal_position \
FROM sys.parameters p JOIN sys.types ty ON ty.user_type_id=p.user_type_id WHERE p.object_id=@id \
UNION ALL \
SELECT c.name,{result},'TABLE',CAST(c.column_id+(SELECT ISNULL(MAX(q.parameter_id),0) FROM sys.parameters q WHERE q.object_id=c.object_id) AS int) \
FROM sys.columns c JOIN sys.types ty ON ty.user_type_id=c.user_type_id WHERE c.object_id=@id) x ORDER BY x.ordinal_position;\
SELECT m.definition,CASE WHEN o.type IN ('P','PC') THEN 'PROCEDURE' ELSE 'FUNCTION' END AS p_routine_type,\
CASE WHEN o.type IN ('IF','TF','FT') THEN 'TABLE' ELSE (SELECT {parameter} FROM sys.parameters p JOIN sys.types ty ON ty.user_type_id=p.user_type_id WHERE p.object_id=o.object_id AND p.parameter_id=0) END AS p_return_type \
FROM sys.objects o LEFT JOIN sys.sql_modules m ON m.object_id=o.object_id WHERE o.object_id=@id;"
    )
}

const TRIGGER_SQL: &str = "SELECT m.definition,p.name AS p_table,CASE WHEN tr.is_instead_of_trigger=1 THEN 'INSTEAD OF' ELSE 'AFTER' END AS p_timing,\
STUFF((SELECT ','+te.type_desc FROM sys.trigger_events te WHERE te.object_id=tr.object_id ORDER BY te.type FOR XML PATH('')),1,1,'') AS p_events,\
CAST(1-tr.is_disabled AS bit) AS p_enabled \
FROM sys.triggers tr JOIN sys.objects o ON o.object_id=tr.object_id JOIN sys.schemas s ON s.schema_id=o.schema_id \
JOIN sys.objects p ON p.object_id=tr.parent_id LEFT JOIN sys.sql_modules m ON m.object_id=tr.object_id \
WHERE s.name=@schema AND o.name=@name AND tr.parent_class=1;";

fn sequence_sql() -> String {
    format!(
        "SELECT {NO_DEFINITION},ty.name AS p_data_type,CAST(sq.start_value AS nvarchar(64)) AS p_start_value,CAST(sq.increment AS nvarchar(64)) AS p_increment,\
CAST(sq.minimum_value AS nvarchar(64)) AS p_minimum_value,CAST(sq.maximum_value AS nvarchar(64)) AS p_maximum_value,sq.is_cycling AS p_cycle,\
sq.cache_size AS p_cache_size,CAST(sq.current_value AS nvarchar(64)) AS p_current_value \
FROM sys.sequences sq JOIN sys.schemas s ON s.schema_id=sq.schema_id JOIN sys.types ty ON ty.user_type_id=sq.user_type_id \
WHERE s.name=@schema AND sq.name=@name;"
    )
}

const SYNONYM_SQL: &str = "SELECT CAST(NULL AS nvarchar(max)) AS definition,sy.base_object_name AS p_base_object FROM sys.synonyms sy JOIN sys.schemas s ON s.schema_id=sy.schema_id WHERE s.name=@schema AND sy.name=@name;";

fn type_sql() -> String {
    format!(
        "DECLARE @id int=(SELECT tt.type_table_object_id FROM sys.table_types tt JOIN sys.schemas s ON s.schema_id=tt.schema_id WHERE s.name=@schema AND tt.name=@name);{}\
SELECT {NO_DEFINITION},CASE WHEN ty.is_table_type=1 THEN 'table' WHEN ty.is_assembly_type=1 THEN 'base' ELSE 'alias' END AS p_category,\
CASE WHEN ty.is_table_type=1 THEN NULL ELSE bt.name END AS p_base_type,\
CASE WHEN ty.is_table_type=1 THEN NULL ELSE ty.max_length END AS p_max_length,\
CASE WHEN ty.is_table_type=1 THEN NULL ELSE ty.precision END AS p_precision,\
CASE WHEN ty.is_table_type=1 THEN NULL ELSE ty.scale END AS p_scale,\
CASE WHEN ty.is_table_type=1 THEN NULL ELSE ty.is_nullable END AS p_nullable \
FROM sys.types ty JOIN sys.schemas s ON s.schema_id=ty.schema_id LEFT JOIN sys.types bt ON bt.user_type_id=ty.system_type_id AND ty.is_assembly_type=0 \
WHERE s.name=@schema AND ty.name=@name AND ty.is_user_defined=1;",
        columns_sql("@id")
    )
}

const RULE_SQL: &str = "SELECT m.definition FROM sys.objects o JOIN sys.schemas s ON s.schema_id=o.schema_id LEFT JOIN sys.sql_modules m ON m.object_id=o.object_id \
WHERE s.name=@schema AND o.name=@name AND o.type='R';";

fn security_policy_sql() -> String {
    format!(
        "DECLARE @id int={};\
SELECT pr.predicate_type_desc AS type,pr.operation_desc AS operation,ts.name+'.'+tobj.name AS target_table,pr.predicate_definition AS definition \
FROM sys.security_predicates pr JOIN sys.objects tobj ON tobj.object_id=pr.target_object_id JOIN sys.schemas ts ON ts.schema_id=tobj.schema_id \
WHERE pr.object_id=@id ORDER BY pr.security_predicate_id;\
SELECT {NO_DEFINITION},sp.is_enabled AS p_is_enabled,sp.is_schema_bound AS p_is_schema_bound FROM sys.security_policies sp WHERE sp.object_id=@id;",
        named("sys.security_policies", "sp", "")
    )
}

fn relation_sql() -> String {
    format!(
        "DECLARE @id int=OBJECT_ID(@table);{}{FOREIGN_KEYS_SQL}{INDEXES_SQL}{CHECKS_SQL}{DEFINITION_SQL}",
        columns_sql("@id")
    )
}

pub fn sql(kind: Kind) -> String {
    match kind {
        Kind::Table | Kind::View => relation_sql(),
        Kind::Routine => routine_sql(),
        Kind::Trigger => TRIGGER_SQL.to_owned(),
        Kind::Sequence => sequence_sql(),
        Kind::Synonym => SYNONYM_SQL.to_owned(),
        Kind::Type => type_sql(),
        Kind::Rule => RULE_SQL.to_owned(),
        Kind::SecurityPolicy => security_policy_sql(),
    }
}

pub fn parameters(table: &TableRef, kind: Kind) -> Vec<Parameter> {
    let parameter = |name: &str, value: String| Parameter {
        name: name.into(),
        value: json!(value),
    };
    match kind {
        Kind::Table | Kind::View => vec![parameter("@table", table.qualified())],
        _ => {
            let schema = table
                .schema
                .as_deref()
                .filter(|schema| !schema.trim().is_empty())
                .unwrap_or("dbo");
            vec![
                parameter("@schema", schema.to_owned()),
                parameter("@name", table.table.clone()),
            ]
        }
    }
}

fn rows(set: &Value) -> Vec<Row> {
    let (Some(columns), Some(rows)) = (set["columns"].as_array(), set["rows"].as_array()) else {
        return Vec::new();
    };
    rows.iter()
        .filter_map(Value::as_array)
        .map(|row| {
            columns
                .iter()
                .zip(row)
                .filter_map(|(name, value)| Some((name.as_str()?.to_owned(), value.clone())))
                .collect()
        })
        .collect()
}

fn sets(data: &Value) -> Vec<Vec<Row>> {
    std::iter::once(data)
        .chain(data["additional_results"].as_array().into_iter().flatten())
        .map(rows)
        .collect()
}

fn field(row: &Row, key: &str) -> Value {
    row.get(key).cloned().unwrap_or(Value::Null)
}

fn text(row: &Row, key: &str) -> Option<String> {
    row.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn grouped<'a>(rows: &'a [Row], key: &str) -> Vec<(String, Vec<&'a Row>)> {
    let mut groups: Vec<(String, Vec<&Row>)> = Vec::new();
    for row in rows {
        let name = text(row, key).unwrap_or_default();
        match groups.iter_mut().find(|(group, _)| *group == name) {
            Some((_, members)) => members.push(row),
            None => groups.push((name, vec![row])),
        }
    }
    groups
}

fn column_names(rows: &[&Row], included: bool) -> Vec<Value> {
    rows.iter()
        .filter(|row| (row.get("is_included") == Some(&json!(true))) == included)
        .map(|row| field(row, "column_name"))
        .collect()
}

fn foreign_keys(rows: &[Row]) -> Vec<Value> {
    grouped(rows, "name")
        .into_iter()
        .map(|(name, members)| {
            let first = members[0];
            json!({
                "name": name,
                "columns": members.iter().map(|row| field(row, "column_name")).collect::<Vec<_>>(),
                "ref_schema": field(first, "ref_schema"),
                "ref_table": field(first, "ref_table"),
                "ref_columns": members.iter().map(|row| field(row, "ref_column")).collect::<Vec<_>>(),
                "on_update": field(first, "on_update"),
                "on_delete": field(first, "on_delete"),
            })
        })
        .collect()
}

fn indexes_and_keys(rows: &[Row]) -> (Vec<Value>, Vec<Value>) {
    let mut indexes = Vec::new();
    let mut keys = Vec::new();
    for (name, members) in grouped(rows, "name") {
        let first = members[0];
        let columns = column_names(&members, false);
        indexes.push(json!({
            "name": name,
            "columns": columns,
            "included_columns": column_names(&members, true),
            "is_unique": field(first, "is_unique"),
            "is_primary": field(first, "is_primary"),
            "type": field(first, "type_desc"),
            "filter": field(first, "filter"),
            "definition": null,
        }));
        if let Some(constraint) = text(first, "constraint_name") {
            let primary = text(first, "constraint_type").as_deref() == Some("PK");
            keys.push(json!({
                "name": constraint,
                "kind": if primary { "primary_key" } else { "unique" },
                "columns": columns,
                "index": name,
            }));
        }
    }
    keys.sort_by_key(|key| key["kind"] != "primary_key");
    (indexes, keys)
}

fn number_or_text(value: Value) -> Value {
    match value.as_str().map(str::parse::<i64>) {
        Some(Ok(number)) => json!(number),
        _ => value,
    }
}

fn properties(kind: Kind, meta: Option<&Row>, predicates: &[Row]) -> Value {
    let mut properties: Map<String, Value> = meta
        .into_iter()
        .flatten()
        .filter_map(|(key, value)| Some((key.strip_prefix("p_")?.to_owned(), value.clone())))
        .collect();
    match kind {
        Kind::Trigger if meta.is_some() => {
            let events = properties
                .get("events")
                .and_then(Value::as_str)
                .map(|events| events.split(',').map(|event| json!(event)).collect());
            properties.insert("events".into(), Value::Array(events.unwrap_or_default()));
        }
        Kind::Sequence => {
            for key in [
                "start_value",
                "increment",
                "minimum_value",
                "maximum_value",
                "current_value",
            ] {
                if let Some(value) = properties.remove(key) {
                    properties.insert(key.into(), number_or_text(value));
                }
            }
        }
        Kind::SecurityPolicy if meta.is_some() => {
            let predicates = predicates
                .iter()
                .map(|row| {
                    json!({
                        "type": field(row, "type"),
                        "operation": field(row, "operation"),
                        "target_table": field(row, "target_table"),
                        "definition": field(row, "definition"),
                    })
                })
                .collect();
            properties.insert("predicates".into(), Value::Array(predicates));
        }
        _ => {}
    }
    Value::Object(properties)
}

pub fn result(kind: Kind, data: &Value) -> Value {
    let sets = sets(data);
    let set = |index: usize| sets.get(index).map(Vec::as_slice).unwrap_or_default();
    let objects = |rows: &[Row]| rows.iter().cloned().map(Value::Object).collect::<Vec<_>>();
    let (columns, foreign_keys, indexes, keys, check_constraints, predicates, meta) = match kind {
        Kind::Table | Kind::View => {
            let (indexes, keys) = indexes_and_keys(set(2));
            (
                objects(set(0)),
                foreign_keys(set(1)),
                indexes,
                keys,
                objects(set(3)),
                &[][..],
                set(4),
            )
        }
        Kind::Routine | Kind::Type => (
            objects(set(0)),
            vec![],
            vec![],
            vec![],
            vec![],
            &[][..],
            set(1),
        ),
        Kind::SecurityPolicy => (vec![], vec![], vec![], vec![], vec![], set(0), set(1)),
        Kind::Trigger | Kind::Sequence | Kind::Synonym | Kind::Rule => {
            (vec![], vec![], vec![], vec![], vec![], &[][..], set(0))
        }
    };
    let meta = meta.first();
    json!({
        "columns": columns,
        "foreign_keys": foreign_keys,
        "indexes": indexes,
        "keys": keys,
        "check_constraints": check_constraints,
        "definition": meta.map_or(Value::Null, |row| field(row, "definition")),
        "properties": properties(kind, meta, predicates),
    })
}
