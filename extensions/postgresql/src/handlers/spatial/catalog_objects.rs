/// Object kinds a `catalog.objects` row can report, as the service contract names them.
pub const KINDS: [&str; 5] = ["table", "view", "materialized_view", "routine", "trigger"];

const SYSTEM_SCHEMA: &str = "n.nspname NOT IN ('pg_catalog','information_schema') \
AND n.nspname NOT LIKE 'pg\\_toast%' AND n.nspname NOT LIKE 'pg\\_temp\\_%'";

/// Relations, routines and triggers in one parameterized statement. The host
/// filters the returned rows by `kind`, so every kind is enumerated here.
/// Relations sort first so that a row-limit truncation never drops them.
pub fn objects_sql() -> String {
    format!(
        "SELECT o.schema,o.name,o.kind,o.owner_table AS \"table\" FROM (\
SELECT 0 AS rank,n.nspname::text AS schema,c.relname::text AS name,\
CASE c.relkind WHEN 'v' THEN 'view' WHEN 'm' THEN 'materialized_view' ELSE 'table' END::text AS kind,\
NULL::text AS owner_table \
FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace \
WHERE c.relkind IN ('r','p','v','m','f') AND ($1::text IS NULL OR n.nspname=$1) \
AND pg_catalog.has_schema_privilege(n.oid,'USAGE') AND pg_catalog.has_table_privilege(c.oid,'SELECT') \
UNION ALL \
SELECT DISTINCT 1,n.nspname::text,p.proname::text,'routine'::text,NULL::text \
FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace \
WHERE ($1::text IS NULL OR n.nspname=$1) AND ($1::text IS NOT NULL OR ({SYSTEM_SCHEMA})) \
AND pg_catalog.has_schema_privilege(n.oid,'USAGE') AND pg_catalog.has_function_privilege(p.oid,'EXECUTE') \
AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_depend d WHERE d.classid='pg_catalog.pg_proc'::regclass AND d.objid=p.oid AND d.deptype='e') \
UNION ALL \
SELECT 2,n.nspname::text,t.tgname::text,'trigger'::text,c.relname::text \
FROM pg_catalog.pg_trigger t JOIN pg_catalog.pg_class c ON c.oid=t.tgrelid \
JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace \
WHERE NOT t.tgisinternal AND ($1::text IS NULL OR n.nspname=$1) AND ($1::text IS NOT NULL OR ({SYSTEM_SCHEMA})) \
AND pg_catalog.has_schema_privilege(n.oid,'USAGE') AND pg_catalog.has_table_privilege(c.oid,'SELECT')\
) o ORDER BY o.rank,o.schema,o.name,o.owner_table"
    )
}

#[cfg(test)]
#[path = "catalog_objects_tests.rs"]
mod catalog_objects_tests;
