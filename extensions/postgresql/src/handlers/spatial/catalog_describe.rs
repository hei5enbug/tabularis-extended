use serde::Deserialize;
use serde_json::{json, Value};
use tokio_postgres::Client;

use super::{
    catalog::Catalog,
    exact,
    service::{exact_input, row_objects},
    types::TableRef,
    SpatialError,
};
use crate::cancel::ScopedGuard;

/// Object kinds `catalog.describe` accepts, as the service contract names them.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ObjectKind {
    Table,
    View,
    MaterializedView,
    ForeignTable,
    Routine,
    Aggregate,
    Trigger,
    Sequence,
    Type,
    Collation,
    Operator,
    OperatorClass,
    OperatorFamily,
}

#[derive(Debug, PartialEq)]
pub struct Statement {
    pub text: &'static str,
    pub values: Vec<Value>,
}

/// The statements one describe runs: an optional per-row list for `columns`
/// and one single-row statement for every other key of the response.
#[derive(Debug, PartialEq)]
pub struct Plan {
    pub columns: Option<Statement>,
    pub details: Statement,
}

const DETAIL_KEYS: [&str; 6] = [
    "foreign_keys",
    "indexes",
    "keys",
    "check_constraints",
    "definition",
    "properties",
];

const COLUMNS_SQL: &str = "SELECT a.attname::text AS name,(row_number() OVER (ORDER BY a.attnum)-1)::integer AS column_index,\
a.atttypid::bigint AS type_oid,pg_catalog.format_type(a.atttypid,a.atttypmod)::text AS data_type,NOT a.attnotnull AS nullable,\
COALESCE(a.attnum=ANY(pk.conkey),false) AS is_primary_key,pg_catalog.array_position(pk.conkey,a.attnum)::integer AS primary_key_position,\
COALESCE((SELECT json_agg(pa.attname ORDER BY k.ordinality) FROM unnest(pk.conkey) WITH ORDINALITY k(attnum,ordinality) \
JOIN pg_catalog.pg_attribute pa ON pa.attrelid=c.oid AND pa.attnum=k.attnum),'[]'::json) AS primary_key,\
pg_catalog.pg_get_expr(d.adbin,d.adrelid)::text AS default_value,\
(a.attidentity<>'' OR a.attgenerated<>'' OR pg_catalog.pg_get_expr(d.adbin,d.adrelid) LIKE 'nextval(%') AS generated \
FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace \
JOIN pg_catalog.pg_attribute a ON a.attrelid=c.oid \
LEFT JOIN pg_catalog.pg_constraint pk ON pk.conrelid=c.oid AND pk.contype='p' \
LEFT JOIN pg_catalog.pg_attrdef d ON d.adrelid=c.oid AND d.adnum=a.attnum \
WHERE n.nspname=$1::text AND c.relname=$2::text AND ($3::text IS NULL OR c.relkind::text=ANY(string_to_array($3::text,','))) \
AND a.attnum>0 AND NOT a.attisdropped AND pg_catalog.has_schema_privilege(n.oid,'USAGE') \
AND (c.relkind='c' OR pg_catalog.has_table_privilege(c.oid,'SELECT')) ORDER BY a.attnum";

const RELATION_SQL: &str = "WITH rel AS (\
SELECT c.oid,c.relkind FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace \
WHERE n.nspname=$1::text AND c.relname=$2::text \
AND ($3::text IS NULL OR c.relkind::text=ANY(string_to_array($3::text,','))) \
AND pg_catalog.has_schema_privilege(n.oid,'USAGE') \
AND (c.relkind='c' OR pg_catalog.has_table_privilege(c.oid,'SELECT'))) \
SELECT \
(SELECT COALESCE(json_agg(json_build_object(\
'name',co.conname::text,\
'columns',(SELECT COALESCE(json_agg(a.attname::text ORDER BY k.ord),'[]'::json) \
FROM unnest(co.conkey) WITH ORDINALITY k(attnum,ord) \
JOIN pg_catalog.pg_attribute a ON a.attrelid=co.conrelid AND a.attnum=k.attnum),\
'ref_schema',rn.nspname::text,\
'ref_table',rc.relname::text,\
'ref_columns',(SELECT COALESCE(json_agg(a.attname::text ORDER BY k.ord),'[]'::json) \
FROM unnest(co.confkey) WITH ORDINALITY k(attnum,ord) \
JOIN pg_catalog.pg_attribute a ON a.attrelid=co.confrelid AND a.attnum=k.attnum),\
'on_update',CASE co.confupdtype WHEN 'a' THEN 'NO ACTION' WHEN 'r' THEN 'RESTRICT' WHEN 'c' THEN 'CASCADE' WHEN 'n' THEN 'SET NULL' WHEN 'd' THEN 'SET DEFAULT' END,\
'on_delete',CASE co.confdeltype WHEN 'a' THEN 'NO ACTION' WHEN 'r' THEN 'RESTRICT' WHEN 'c' THEN 'CASCADE' WHEN 'n' THEN 'SET NULL' WHEN 'd' THEN 'SET DEFAULT' END\
) ORDER BY co.conname),'[]'::json) \
FROM pg_catalog.pg_constraint co JOIN pg_catalog.pg_class rc ON rc.oid=co.confrelid \
JOIN pg_catalog.pg_namespace rn ON rn.oid=rc.relnamespace \
WHERE co.conrelid=rel.oid AND co.contype='f') AS foreign_keys,\
(SELECT COALESCE(json_agg(json_build_object(\
'name',ic.relname::text,\
'columns',(SELECT COALESCE(json_agg(COALESCE(a.attname::text,pg_catalog.pg_get_indexdef(ix.indexrelid,k.n::integer,true)) ORDER BY k.n),'[]'::json) \
FROM generate_series(1,ix.indnkeyatts::integer) k(n) \
LEFT JOIN pg_catalog.pg_attribute a ON a.attrelid=ix.indrelid AND a.attnum=ix.indkey[k.n-1] AND ix.indkey[k.n-1]<>0),\
'included_columns',(SELECT COALESCE(json_agg(a.attname::text ORDER BY k.n),'[]'::json) \
FROM generate_series(ix.indnkeyatts::integer+1,ix.indnatts::integer) k(n) \
JOIN pg_catalog.pg_attribute a ON a.attrelid=ix.indrelid AND a.attnum=ix.indkey[k.n-1]),\
'is_unique',ix.indisunique,\
'is_primary',ix.indisprimary,\
'type',am.amname::text,\
'filter',pg_catalog.pg_get_expr(ix.indpred,ix.indrelid,true),\
'definition',pg_catalog.pg_get_indexdef(ix.indexrelid)\
) ORDER BY ic.relname),'[]'::json) \
FROM pg_catalog.pg_index ix JOIN pg_catalog.pg_class ic ON ic.oid=ix.indexrelid \
JOIN pg_catalog.pg_am am ON am.oid=ic.relam WHERE ix.indrelid=rel.oid) AS indexes,\
(SELECT COALESCE(json_agg(json_build_object(\
'name',co.conname::text,\
'kind',CASE co.contype WHEN 'p' THEN 'primary_key' ELSE 'unique' END,\
'columns',(SELECT COALESCE(json_agg(a.attname::text ORDER BY k.ord),'[]'::json) \
FROM unnest(co.conkey) WITH ORDINALITY k(attnum,ord) \
JOIN pg_catalog.pg_attribute a ON a.attrelid=co.conrelid AND a.attnum=k.attnum \
WHERE k.ord<=COALESCE(xi.indnkeyatts,cardinality(co.conkey))),\
'index',ic.relname::text\
) ORDER BY (co.contype<>'p'),co.conname),'[]'::json) \
FROM pg_catalog.pg_constraint co LEFT JOIN pg_catalog.pg_index xi ON xi.indexrelid=co.conindid \
LEFT JOIN pg_catalog.pg_class ic ON ic.oid=co.conindid \
WHERE co.conrelid=rel.oid AND co.contype IN ('p','u')) AS keys,\
(SELECT COALESCE(json_agg(json_build_object('name',co.conname::text,'definition',pg_catalog.pg_get_constraintdef(co.oid,true)) ORDER BY co.conname),'[]'::json) \
FROM pg_catalog.pg_constraint co WHERE co.conrelid=rel.oid AND co.contype='c') AS check_constraints,\
CASE WHEN rel.relkind IN ('v','m') THEN pg_catalog.pg_get_viewdef(rel.oid,true) END AS definition,\
CASE WHEN rel.relkind='f' THEN (SELECT json_build_object('server',s.srvname::text,\
'options',COALESCE((SELECT json_object_agg(split_part(o,'=',1),substr(o,strpos(o,'=')+1)) FROM unnest(ft.ftoptions) o),'{}'::json)) \
FROM pg_catalog.pg_foreign_table ft JOIN pg_catalog.pg_foreign_server s ON s.oid=ft.ftserver WHERE ft.ftrelid=rel.oid) END AS properties \
FROM rel";

const ROUTINE_PARAMETERS_SQL: &str = "WITH f AS (\
SELECT p.oid,p.prokind,p.prorettype,p.proargnames,p.proargmodes,\
COALESCE(p.proallargtypes,string_to_array(p.proargtypes::text,' ')::oid[]) AS types \
FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace \
WHERE n.nspname=$1::text AND p.proname=$2::text AND p.prokind::text=ANY(string_to_array($3::text,',')) \
AND pg_catalog.has_schema_privilege(n.oid,'USAGE') AND pg_catalog.has_function_privilege(p.oid,'EXECUTE') \
ORDER BY p.oid LIMIT 1) \
SELECT x.name,x.data_type,x.mode,x.ordinal_position FROM (\
SELECT ''::text AS name,pg_catalog.format_type(f.prorettype,NULL)::text AS data_type,'OUT'::text AS mode,0 AS ordinal_position \
FROM f WHERE f.prokind IN ('f','w') \
AND f.prorettype NOT IN ('void'::regtype,'trigger'::regtype,'event_trigger'::regtype) \
AND (f.proargmodes IS NULL OR NOT f.proargmodes::text[] && ARRAY['o','b','t']) \
UNION ALL \
SELECT COALESCE(f.proargnames[g.i],'')::text,pg_catalog.format_type(f.types[g.i],NULL)::text,\
CASE COALESCE(f.proargmodes[g.i]::text,'i') WHEN 'i' THEN 'IN' WHEN 'o' THEN 'OUT' WHEN 'b' THEN 'INOUT' WHEN 'v' THEN 'VARIADIC' WHEN 't' THEN 'TABLE' END::text,\
g.i::integer \
FROM f CROSS JOIN LATERAL generate_series(1,COALESCE(cardinality(f.types),0)) g(i)\
) x ORDER BY x.ordinal_position";

const ROUTINE_SQL: &str = "WITH f AS (\
SELECT p.oid,p.prokind,l.lanname,count(*) OVER () AS overloads,row_number() OVER (ORDER BY p.oid) AS position \
FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace \
JOIN pg_catalog.pg_language l ON l.oid=p.prolang \
WHERE n.nspname=$1::text AND p.proname=$2::text AND p.prokind::text=ANY(string_to_array($3::text,',')) \
AND pg_catalog.has_schema_privilege(n.oid,'USAGE') AND pg_catalog.has_function_privilege(p.oid,'EXECUTE')) \
SELECT \
(SELECT string_agg(pg_catalog.pg_get_functiondef(d.oid),E'\\n\\n' ORDER BY d.oid) FROM f d WHERE d.prokind<>'a') AS definition,\
CASE WHEN f.prokind='a' THEN json_build_object(\
'return_type',pg_catalog.pg_get_function_result(f.oid)::text,\
'arguments',pg_catalog.pg_get_function_identity_arguments(f.oid)::text,\
'overloads',f.overloads) \
ELSE json_build_object(\
'routine_type',CASE f.prokind WHEN 'p' THEN 'PROCEDURE' ELSE 'FUNCTION' END,\
'return_type',pg_catalog.pg_get_function_result(f.oid)::text,\
'language',f.lanname::text,\
'overloads',f.overloads) END AS properties \
FROM f WHERE f.position=1";

const TRIGGER_SQL: &str = "SELECT pg_catalog.pg_get_triggerdef(t.oid,true) AS definition,count(*) OVER () AS candidates,json_build_object(\
'table',c.relname::text,\
'timing',CASE WHEN t.tgtype::integer & 2 <> 0 THEN 'BEFORE' WHEN t.tgtype::integer & 64 <> 0 THEN 'INSTEAD OF' ELSE 'AFTER' END,\
'events',(SELECT COALESCE(json_agg(e.name ORDER BY e.bit),'[]'::json) \
FROM (VALUES ('INSERT',4),('DELETE',8),('UPDATE',16),('TRUNCATE',32)) e(name,bit) WHERE t.tgtype::integer & e.bit <> 0),\
'enabled',t.tgenabled<>'D') AS properties \
FROM pg_catalog.pg_trigger t JOIN pg_catalog.pg_class c ON c.oid=t.tgrelid \
JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace \
WHERE NOT t.tgisinternal AND n.nspname=$1::text AND t.tgname=$2::text AND ($3::text IS NULL OR c.relname=$3::text) \
AND pg_catalog.has_schema_privilege(n.oid,'USAGE') AND pg_catalog.has_table_privilege(c.oid,'SELECT') \
ORDER BY c.relname LIMIT 1";

const SEQUENCE_SQL: &str = "SELECT json_build_object(\
'data_type',s.data_type::text,\
'start_value',CASE WHEN s.start_value BETWEEN -9007199254740991 AND 9007199254740991 THEN to_json(s.start_value) ELSE to_json(s.start_value::text) END,\
'increment',CASE WHEN s.increment_by BETWEEN -9007199254740991 AND 9007199254740991 THEN to_json(s.increment_by) ELSE to_json(s.increment_by::text) END,\
'minimum_value',CASE WHEN s.min_value BETWEEN -9007199254740991 AND 9007199254740991 THEN to_json(s.min_value) ELSE to_json(s.min_value::text) END,\
'maximum_value',CASE WHEN s.max_value BETWEEN -9007199254740991 AND 9007199254740991 THEN to_json(s.max_value) ELSE to_json(s.max_value::text) END,\
'cycle',s.cycle,\
'cache_size',CASE WHEN s.cache_size BETWEEN -9007199254740991 AND 9007199254740991 THEN to_json(s.cache_size) ELSE to_json(s.cache_size::text) END,\
'last_value',CASE WHEN s.last_value IS NULL THEN NULL WHEN s.last_value BETWEEN -9007199254740991 AND 9007199254740991 THEN to_json(s.last_value) ELSE to_json(s.last_value::text) END) AS properties \
FROM pg_catalog.pg_sequences s \
WHERE s.schemaname=$1::text AND s.sequencename=$2::text AND pg_catalog.has_schema_privilege($1::text,'USAGE')";

const TYPE_SQL: &str = "SELECT \
(SELECT COALESCE(json_agg(json_build_object('name',co.conname::text,'definition',pg_catalog.pg_get_constraintdef(co.oid,true)) ORDER BY co.conname),'[]'::json) \
FROM pg_catalog.pg_constraint co WHERE co.contypid=t.oid AND co.contype='c') AS check_constraints,\
json_strip_nulls(json_build_object(\
'category',CASE t.typtype WHEN 'd' THEN 'domain' WHEN 'e' THEN 'enum' WHEN 'c' THEN 'composite' WHEN 'r' THEN 'range' ELSE 'base' END,\
'base_type',CASE t.typtype WHEN 'd' THEN pg_catalog.format_type(t.typbasetype,t.typtypmod) \
WHEN 'r' THEN (SELECT pg_catalog.format_type(r.rngsubtype,NULL) FROM pg_catalog.pg_range r WHERE r.rngtypid=t.oid) END,\
'max_length',CASE WHEN t.typtype='b' AND t.typlen>0 THEN t.typlen END,\
'nullable',CASE WHEN t.typtype='d' THEN NOT t.typnotnull END,\
'default',CASE WHEN t.typtype='d' THEN t.typdefault END,\
'labels',CASE WHEN t.typtype='e' THEN (SELECT json_agg(e.enumlabel::text ORDER BY e.enumsortorder) FROM pg_catalog.pg_enum e WHERE e.enumtypid=t.oid) END)) AS properties \
FROM pg_catalog.pg_type t JOIN pg_catalog.pg_namespace n ON n.oid=t.typnamespace \
WHERE n.nspname=$1::text AND t.typname=$2::text AND t.typtype IN ('d','e','c','r','b') AND t.typcategory<>'A' \
AND (t.typtype<>'c' OR EXISTS (SELECT 1 FROM pg_catalog.pg_class c WHERE c.oid=t.typrelid AND c.relkind='c')) \
AND pg_catalog.has_schema_privilege(n.oid,'USAGE')";

const COLLATION_SQL: &str = "SELECT json_build_object(\
'provider',CASE c.collprovider WHEN 'c' THEN 'libc' WHEN 'i' THEN 'icu' WHEN 'd' THEN 'default' ELSE c.collprovider::text END,\
'collate',COALESCE(c.collcollate,to_jsonb(c)->>'colllocale',to_jsonb(c)->>'colliculocale'),\
'ctype',COALESCE(c.collctype,to_jsonb(c)->>'colllocale',to_jsonb(c)->>'colliculocale'),\
'deterministic',c.collisdeterministic) AS properties \
FROM pg_catalog.pg_collation c JOIN pg_catalog.pg_namespace n ON n.oid=c.collnamespace \
WHERE n.nspname=$1::text AND c.collname=$2::text \
AND c.collencoding IN (-1,pg_catalog.pg_char_to_encoding(pg_catalog.getdatabaseencoding())) \
AND pg_catalog.has_schema_privilege(n.oid,'USAGE') ORDER BY c.oid LIMIT 1";

const OPERATOR_SQL: &str = "SELECT json_build_object(\
'left_type',CASE WHEN o.oprleft<>0 THEN pg_catalog.format_type(o.oprleft,NULL) END,\
'right_type',CASE WHEN o.oprright<>0 THEN pg_catalog.format_type(o.oprright,NULL) END,\
'result_type',CASE WHEN o.oprresult<>0 THEN pg_catalog.format_type(o.oprresult,NULL) END,\
'function',o.oprcode::text,\
'overloads',count(*) OVER ()) AS properties \
FROM pg_catalog.pg_operator o JOIN pg_catalog.pg_namespace n ON n.oid=o.oprnamespace \
WHERE n.nspname=$1::text AND o.oprname=$2::text AND pg_catalog.has_schema_privilege(n.oid,'USAGE') \
ORDER BY o.oid LIMIT 1";

const OPERATOR_CLASS_SQL: &str = "SELECT json_build_object('access_method',am.amname::text,\
'input_type',pg_catalog.format_type(oc.opcintype,NULL),'is_default',oc.opcdefault,'family',f.opfname::text) AS properties \
FROM pg_catalog.pg_opclass oc JOIN pg_catalog.pg_namespace n ON n.oid=oc.opcnamespace \
JOIN pg_catalog.pg_am am ON am.oid=oc.opcmethod JOIN pg_catalog.pg_opfamily f ON f.oid=oc.opcfamily \
WHERE n.nspname=$1::text AND oc.opcname=$2::text AND pg_catalog.has_schema_privilege(n.oid,'USAGE') \
ORDER BY oc.oid LIMIT 1";

const OPERATOR_FAMILY_SQL: &str =
    "SELECT json_build_object('access_method',am.amname::text) AS properties \
FROM pg_catalog.pg_opfamily f JOIN pg_catalog.pg_namespace n ON n.oid=f.opfnamespace \
JOIN pg_catalog.pg_am am ON am.oid=f.opfmethod \
WHERE n.nspname=$1::text AND f.opfname=$2::text AND pg_catalog.has_schema_privilege(n.oid,'USAGE') \
ORDER BY f.oid LIMIT 1";

fn statement(text: &'static str, values: Vec<Value>) -> Statement {
    Statement { text, values }
}

fn relation_plan(schema: Value, name: Value, relkinds: Option<&str>) -> Plan {
    let kinds = relkinds.map_or(Value::Null, |kinds| json!(kinds));
    let values = vec![schema, name, kinds];
    Plan {
        columns: Some(statement(COLUMNS_SQL, values.clone())),
        details: statement(RELATION_SQL, values),
    }
}

fn routine_plan(schema: Value, name: Value, prokinds: &str) -> Plan {
    let values = vec![schema, name, json!(prokinds)];
    Plan {
        columns: Some(statement(ROUTINE_PARAMETERS_SQL, values.clone())),
        details: statement(ROUTINE_SQL, values),
    }
}

fn details_only(text: &'static str, schema: Value, name: Value) -> Plan {
    Plan {
        columns: None,
        details: statement(text, vec![schema, name]),
    }
}

pub fn plan(table: &TableRef, kind: Option<ObjectKind>, owner_table: Option<&str>) -> Plan {
    let schema = json!(table.schema());
    let name = json!(table.table);
    match kind {
        None => relation_plan(schema, name, None),
        Some(ObjectKind::Table) => relation_plan(schema, name, Some("r,p")),
        Some(ObjectKind::View) => relation_plan(schema, name, Some("v")),
        Some(ObjectKind::MaterializedView) => relation_plan(schema, name, Some("m")),
        Some(ObjectKind::ForeignTable) => relation_plan(schema, name, Some("f")),
        Some(ObjectKind::Routine) => routine_plan(schema, name, "f,p,w"),
        Some(ObjectKind::Aggregate) => routine_plan(schema, name, "a"),
        Some(ObjectKind::Type) => {
            let values = vec![schema.clone(), name.clone(), json!("c")];
            Plan {
                columns: Some(statement(COLUMNS_SQL, values)),
                details: statement(TYPE_SQL, vec![schema, name]),
            }
        }
        Some(ObjectKind::Trigger) => Plan {
            columns: None,
            details: statement(TRIGGER_SQL, vec![schema, name, json!(owner_table)]),
        },
        Some(ObjectKind::Sequence) => details_only(SEQUENCE_SQL, schema, name),
        Some(ObjectKind::Collation) => details_only(COLLATION_SQL, schema, name),
        Some(ObjectKind::Operator) => details_only(OPERATOR_SQL, schema, name),
        Some(ObjectKind::OperatorClass) => details_only(OPERATOR_CLASS_SQL, schema, name),
        Some(ObjectKind::OperatorFamily) => details_only(OPERATOR_FAMILY_SQL, schema, name),
    }
}

fn default_limits() -> Value {
    json!({"truncated":false,"reasons":[]})
}

fn default_page() -> Value {
    json!({"next_token":null,"has_more":false,"resume_mode":"none"})
}

fn is_truncated(limits: &Value) -> bool {
    limits["truncated"].as_bool().unwrap_or(false)
}

pub fn merge_limits(first: Option<Value>, second: &Value) -> Value {
    let Some(mut merged) = first else {
        return second.clone();
    };
    let mut reasons = merged["reasons"].as_array().cloned().unwrap_or_default();
    for reason in second["reasons"].as_array().into_iter().flatten() {
        if !reasons.contains(reason) {
            reasons.push(reason.clone());
        }
    }
    merged["truncated"] = json!(is_truncated(&merged) || is_truncated(second));
    merged["reasons"] = json!(reasons);
    merged
}

fn empty_detail(key: &str) -> Value {
    match key {
        "definition" => Value::Null,
        "properties" => json!({}),
        _ => json!([]),
    }
}

fn fits(key: &str, value: &Value) -> bool {
    match key {
        "definition" => value.is_string(),
        "properties" => value.is_object(),
        _ => value.is_array(),
    }
}

pub fn assemble(columns: Vec<Value>, detail: Option<Value>, limits: Value, page: Value) -> Value {
    let mut data = json!({"columns":columns,"limits":limits,"page":page});
    for key in DETAIL_KEYS {
        data[key] = detail
            .as_ref()
            .map(|detail| &detail[key])
            .filter(|value| fits(key, value))
            .cloned()
            .unwrap_or_else(|| empty_detail(key));
    }
    data
}

async fn run(
    client: &Client,
    catalog: Option<&Catalog>,
    statement: &Statement,
    scope: &ScopedGuard,
    reserved: bool,
) -> Result<Value, SpatialError> {
    exact::execute(
        client,
        catalog,
        &exact_input(statement.text.to_string(), statement.values.clone()),
        scope,
        true,
        reserved,
    )
    .await
}

pub async fn describe(
    client: &Client,
    catalog: Option<&Catalog>,
    table: &TableRef,
    kind: Option<ObjectKind>,
    owner_table: Option<&str>,
    scope: &ScopedGuard,
    reserved: bool,
) -> Result<Value, SpatialError> {
    if owner_table.is_some() && kind != Some(ObjectKind::Trigger) {
        return Err(SpatialError::argument(
            "owner_table applies only to kind trigger",
        ));
    }
    let plan = plan(table, kind, owner_table);
    let mut columns = Vec::new();
    let mut limits = None;
    let mut page = None;
    if let Some(statement) = &plan.columns {
        let result = run(client, catalog, statement, scope, reserved).await?;
        columns = row_objects(&result)?;
        limits = Some(result["limits"].clone());
        page = Some(result["page"].clone());
    }
    let mut detail = None;
    if !limits.as_ref().is_some_and(is_truncated) {
        let result = run(client, catalog, &plan.details, scope, reserved).await?;
        detail = row_objects(&result)?.into_iter().next();
        if owner_table.is_none()
            && detail
                .as_ref()
                .and_then(|detail| detail["candidates"].as_i64())
                .is_some_and(|candidates| candidates > 1)
        {
            return Err(SpatialError::argument(
                "Several tables have a trigger with this name; set owner_table to choose one",
            ));
        }
        limits = Some(merge_limits(limits, &result["limits"]));
        page = page.or_else(|| Some(result["page"].clone()));
    }
    Ok(assemble(
        columns,
        detail,
        limits.unwrap_or_else(default_limits),
        page.unwrap_or_else(default_page),
    ))
}

#[cfg(test)]
#[path = "catalog_describe_tests.rs"]
mod catalog_describe_tests;
