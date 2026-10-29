# Catalog describe limitations

This page lists known limits of `catalog.describe` in postgresql-plus, the service method behind the MCP
`catalog_describe` tool. The queries live in `src/handlers/spatial/catalog_describe.rs`.

## Objects that share a name

PostgreSQL lets several objects in one schema share a name. `catalog.describe` identifies an object by schema and
name only, so it resolves such names as follows.

| Kind | Several objects with the name | Result |
|---|---|---|
| `routine`, `aggregate` | Overloads with different arguments | The lowest oid is described. `properties.overloads` counts all of them, and a routine's `definition` joins every overload's source. |
| `operator` | Operators with different argument types | The lowest oid is described. `properties.overloads` counts all of them. |
| `operator_class`, `operator_family` | Classes or families for different access methods | The lowest oid is described. No count is reported. |
| `trigger` | Triggers on different tables | Set `owner_table` to the table's name. Without it, the request fails with `INVALID_ARGUMENT`. |

The lowest oid is usually the oldest object, but PostgreSQL does not guarantee that order.

## Aggregates have no definition

An aggregate is described with `properties.return_type`, `properties.arguments`, and `properties.overloads`.
Its `definition` is `null`, because `pg_get_functiondef` does not accept aggregates. The transition and final
functions are not listed.

## Other drivers

The object kinds beyond `table` and `view` are implemented by postgresql-plus and sqlserver-plus only. Every other
driver keeps the kinds its manifest declares, and its `catalog.describe` supports `table` and `view` only.
`catalog_object_kinds` reports what a connection's driver supports.
