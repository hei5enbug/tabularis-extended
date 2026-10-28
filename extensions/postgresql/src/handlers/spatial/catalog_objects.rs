/// Object kinds a `catalog.objects` row can report, as the service contract names them.
pub const KINDS: [&str; 13] = [
    "table",
    "view",
    "materialized_view",
    "foreign_table",
    "routine",
    "aggregate",
    "trigger",
    "sequence",
    "type",
    "collation",
    "operator",
    "operator_class",
    "operator_family",
];

const SYSTEM_SCHEMA: &str = "n.nspname NOT IN ('pg_catalog','information_schema') \
AND n.nspname NOT LIKE 'pg\\_toast%' AND n.nspname NOT LIKE 'pg\\_temp\\_%'";

const IN_SCHEMA: &str = "($1::text IS NULL OR n.nspname=$1) \
AND ($1::text IS NOT NULL OR (SYSTEM_SCHEMA)) \
AND pg_catalog.has_schema_privilege(n.oid,'USAGE')";

const SEQUENCE_MEMBER: &str = "NOT EXISTS (SELECT 1 FROM pg_catalog.pg_depend d \
WHERE d.classid='pg_catalog.pg_class'::regclass AND d.objid=c.oid AND d.deptype IN ('e','i'))";

fn not_extension_member(class: &str, oid: &str) -> String {
    format!(
        "NOT EXISTS (SELECT 1 FROM pg_catalog.pg_depend d WHERE d.classid='pg_catalog.{class}'::regclass AND d.objid={oid} AND d.deptype='e')"
    )
}

/// Every supported kind in one parameterized statement. The host filters the
/// returned rows by `kind`, so every kind is enumerated here. Relations sort
/// first so that a row-limit truncation never drops them.
pub fn objects_sql() -> String {
    let scope = IN_SCHEMA.replace("SYSTEM_SCHEMA", SYSTEM_SCHEMA);
    let routine_member = not_extension_member("pg_proc", "p.oid");
    let type_member = not_extension_member("pg_type", "t.oid");
    let collation_member = not_extension_member("pg_collation", "co.oid");
    let operator_member = not_extension_member("pg_operator", "o.oid");
    let class_member = not_extension_member("pg_opclass", "oc.oid");
    let family_member = not_extension_member("pg_opfamily", "f.oid");
    format!(
        "SELECT o.schema,o.name,o.kind,o.owner_table AS \"table\" FROM (\
SELECT 0 AS rank,n.nspname::text AS schema,c.relname::text AS name,\
CASE c.relkind WHEN 'v' THEN 'view' WHEN 'm' THEN 'materialized_view' WHEN 'f' THEN 'foreign_table' ELSE 'table' END::text AS kind,\
NULL::text AS owner_table \
FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace \
WHERE c.relkind IN ('r','p','v','m','f') AND ($1::text IS NULL OR n.nspname=$1) \
AND pg_catalog.has_schema_privilege(n.oid,'USAGE') AND pg_catalog.has_table_privilege(c.oid,'SELECT') \
UNION ALL \
SELECT DISTINCT 1,n.nspname::text,p.proname::text,CASE WHEN p.prokind='a' THEN 'aggregate' ELSE 'routine' END::text,NULL::text \
FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace \
WHERE {scope} AND pg_catalog.has_function_privilege(p.oid,'EXECUTE') AND {routine_member} \
UNION ALL \
SELECT 2,n.nspname::text,t.tgname::text,'trigger'::text,c.relname::text \
FROM pg_catalog.pg_trigger t JOIN pg_catalog.pg_class c ON c.oid=t.tgrelid \
JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace \
WHERE NOT t.tgisinternal AND {scope} AND pg_catalog.has_table_privilege(c.oid,'SELECT') \
UNION ALL \
SELECT 3,n.nspname::text,c.relname::text,'sequence'::text,NULL::text \
FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace \
WHERE c.relkind='S' AND {scope} AND CASE WHEN c.relkind='S' THEN pg_catalog.has_sequence_privilege(c.oid,'SELECT,USAGE,UPDATE') ELSE false END AND {SEQUENCE_MEMBER} \
UNION ALL \
SELECT 4,n.nspname::text,t.typname::text,'type'::text,NULL::text \
FROM pg_catalog.pg_type t JOIN pg_catalog.pg_namespace n ON n.oid=t.typnamespace \
WHERE t.typtype IN ('d','e','c','r','b') AND t.typcategory<>'A' \
AND (t.typtype<>'c' OR EXISTS (SELECT 1 FROM pg_catalog.pg_class c WHERE c.oid=t.typrelid AND c.relkind='c')) \
AND {scope} AND pg_catalog.has_type_privilege(t.oid,'USAGE') AND {type_member} \
UNION ALL \
SELECT DISTINCT 5,n.nspname::text,co.collname::text,'collation'::text,NULL::text \
FROM pg_catalog.pg_collation co JOIN pg_catalog.pg_namespace n ON n.oid=co.collnamespace \
WHERE co.collencoding IN (-1,pg_catalog.pg_char_to_encoding(pg_catalog.getdatabaseencoding())) \
AND {scope} AND {collation_member} \
UNION ALL \
SELECT DISTINCT 6,n.nspname::text,o.oprname::text,'operator'::text,NULL::text \
FROM pg_catalog.pg_operator o JOIN pg_catalog.pg_namespace n ON n.oid=o.oprnamespace \
WHERE {scope} AND {operator_member} \
UNION ALL \
SELECT DISTINCT 7,n.nspname::text,oc.opcname::text,'operator_class'::text,NULL::text \
FROM pg_catalog.pg_opclass oc JOIN pg_catalog.pg_namespace n ON n.oid=oc.opcnamespace \
WHERE {scope} AND {class_member} \
UNION ALL \
SELECT DISTINCT 8,n.nspname::text,f.opfname::text,'operator_family'::text,NULL::text \
FROM pg_catalog.pg_opfamily f JOIN pg_catalog.pg_namespace n ON n.oid=f.opfnamespace \
WHERE {scope} AND {family_member}\
) o ORDER BY o.rank,o.schema,o.name,o.owner_table"
    )
}

#[cfg(test)]
#[path = "catalog_objects_tests.rs"]
mod catalog_objects_tests;
