//! Migrations for already-persisted connection data — rewriting values on
//! disk that a previous version of the app saved incorrectly. Each migration
//! is path-based (no `AppHandle` dependency) so it can run from both the GUI
//! process's Tauri commands and the standalone `--mcp` server process, which
//! reads and (as of the SSL-mode migration below) writes the same
//! `connections.json` but has no Tauri context.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::drivers::driver_trait::SqlDialect;
use crate::drivers::registry as driver_registry;
use crate::models::SavedConnection;
use crate::persistence;

/// The SSL mode dropdown used to branch on `driver === "postgres"` literally
/// (issue #614), so a postgres-dialect driver with a different id (e.g. the
/// standalone PostgreSQL plugin, id `"postgresql"`) offered MySQL-style
/// underscored `ssl_mode` values instead of Postgres-style hyphenated ones.
/// The plugin's own TLS check only recognizes the hyphenated spelling, so a
/// connection saved with the wrong-family value connects in cleartext with no
/// error. Fixing the dropdown only stops *new* saves from getting the wrong
/// value — this rewrites values already persisted before the fix shipped.
///
/// Maps a stale MySQL-style `ssl_mode` value to its Postgres-style
/// equivalent. Returns `None` for a value that isn't one of the stale
/// MySQL-style spellings (including values already correct, or driver-
/// specific values like ClickHouse's `"disable"`/`"require"`, which happen to
/// already be spelled correctly in both families and need no rewrite).
pub(crate) fn stale_postgres_ssl_mode_replacement(value: &str) -> Option<&'static str> {
    match value {
        "disabled" => Some("disable"),
        "preferred" => Some("prefer"),
        "required" => Some("require"),
        "verify_ca" => Some("verify-ca"),
        "verify_identity" => Some("verify-full"),
        _ => None,
    }
}

/// Rewrites `conn.params.ssl_mode` in place if `conn`'s driver resolves (via
/// `dialects`) to the postgres SQL dialect and its stored value is a stale
/// MySQL-style spelling. Builtin `"postgres"` connections are excluded —
/// their own dropdown was always correct, so nothing there needs migrating.
/// A driver whose manifest doesn't declare `sql_dialect` (`None`, or absent
/// from the map because it never resolved) is treated as NOT postgres —
/// deliberately not defaulted, unlike the splitter's own historical
/// postgres-default, because this migration must distinguish "explicitly
/// postgres" from "unspecified" to avoid rewriting a driver's SSL value
/// based on a guess.
/// Pure and synchronous so it can be exercised directly in tests without a
/// live driver registry. Returns whether a rewrite happened.
pub(crate) fn migrate_connection_ssl_mode_in_place(
    conn: &mut SavedConnection,
    dialects: &HashMap<String, Option<SqlDialect>>,
) -> bool {
    if conn.params.driver == "postgres" {
        return false;
    }
    let is_postgres_dialect = dialects
        .get(&conn.params.driver)
        .copied()
        .flatten()
        .is_some_and(|d| d == SqlDialect::Postgres);
    if !is_postgres_dialect {
        return false;
    }
    let Some(stale) = conn.params.ssl_mode.as_deref() else {
        return false;
    };
    let Some(replacement) = stale_postgres_ssl_mode_replacement(stale) else {
        return false;
    };
    conn.params.ssl_mode = Some(replacement.to_string());
    true
}

/// Migrates already-persisted `ssl_mode` values on postgres-dialect
/// connections (driver id other than the builtin `"postgres"`) from the
/// stale MySQL-style spelling to the Postgres-style spelling the plugin
/// actually understands. Idempotent — a no-op once every affected
/// connection has been rewritten.
///
/// Path-based, no `AppHandle` — callable from both the GUI process (wrapped
/// by `commands::migrate_postgres_ssl_mode_spelling`, which additionally
/// invalidates the connection cache) and the standalone `--mcp` server
/// process, which has no Tauri context and no cache to invalidate.
///
/// Returns `Ok(true)` only once the rewrite has actually been committed to
/// disk — `Ok(false)` covers both "nothing needed migrating" and "a
/// migration was computed but skipped because the file changed concurrently"
/// (see the concurrency note below).
///
/// # Concurrency
///
/// The GUI and the standalone `--mcp` process are independent writers of the
/// same file. The rewrite runs as a locked read-modify-write on the freshly
/// loaded file, so it never overwrites a concurrent edit; only the driver
/// dialect lookup, which is async, happens before the lock. A connection that
/// appeared in between and whose driver was not resolved is skipped and
/// picked up by the next run.
pub async fn migrate_postgres_ssl_mode_spelling_at_path(conn_path: &Path) -> Result<bool, String> {
    if !conn_path.exists() {
        return Ok(false); // Nothing to migrate
    }

    let content = fs::read_to_string(conn_path).map_err(|e| e.to_string())?;
    let observed = persistence::parse_connections_file(&content)?;

    // Resolve each distinct non-builtin driver id's dialect once, not once
    // per connection — the registry lookup is async and connections commonly
    // share a driver.
    let mut dialects: HashMap<String, Option<SqlDialect>> = HashMap::new();
    for conn in &observed.connections {
        let driver_id = &conn.params.driver;
        if driver_id == "postgres" || dialects.contains_key(driver_id) {
            continue; // builtin driver's own dropdown was always correct
        }
        if let Some(driver) = driver_registry::get_driver(driver_id).await {
            dialects.insert(
                driver_id.clone(),
                driver.manifest().capabilities.sql_dialect,
            );
        }
    }

    persistence::update_connections_file(conn_path, |conn_file| {
        let mut migrated_count = 0usize;
        for conn in conn_file.connections.iter_mut() {
            if migrate_connection_ssl_mode_in_place(conn, &dialects) {
                migrated_count += 1;
            }
        }
        if migrated_count > 0 {
            eprintln!(
                "[Migration] Rewriting stale ssl_mode spelling on {} postgres-dialect connection(s)",
                migrated_count
            );
        }
        Ok(migrated_count > 0)
    })
}
