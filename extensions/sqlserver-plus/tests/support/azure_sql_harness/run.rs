use super::{
    config::{self, Config, Flags},
    credentials::{refresh_matches, Auth, Credentials, SecretReader, StdinSecrets},
    fixture,
    report::{self, Report},
    rpc::{LiveRpc, Response, Rpc},
    Error, Result, REQUEST_MS,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, path::Path, time::Instant};

#[derive(Clone, Copy)]
enum Role {
    Writer,
    Reader,
    Refresh,
}
struct Harness<'a> {
    config: &'a Config,
    credentials: &'a Credentials,
    rpc: &'a mut dyn Rpc,
    report: &'a mut Report,
    run_id: String,
    writer_connection: String,
    reader_connection: String,
    owned: BTreeSet<i32>,
    session: Option<String>,
}
impl Harness<'_> {
    fn request(
        &self,
        method: &str,
        input: Value,
        role: Role,
        read_only: bool,
        session: Option<&str>,
    ) -> Value {
        let (principal, auth, connection) = match role {
            Role::Writer => (
                &self.config.writer,
                &self.credentials.writer,
                &self.writer_connection,
            ),
            Role::Reader => (
                &self.config.reader,
                &self.credentials.reader,
                &self.reader_connection,
            ),
            Role::Refresh => (
                &self.config.writer,
                self.credentials
                    .refresh
                    .as_ref()
                    .unwrap_or(&self.credentials.writer),
                &self.writer_connection,
            ),
        };
        let request = uuid::Uuid::new_v4().to_string();
        let extra = [
            ("tenant_id", &principal.tenant_id),
            ("client_id", &principal.client_id),
        ]
        .into_iter()
        .filter_map(|(key, value)| value.as_ref().map(|value| (key, value)))
        .collect::<std::collections::BTreeMap<_, _>>();
        json!({"jsonrpc":"2.0","id":request,"method":method,"params":{"params":{"driver":"sqlserver","host":self.config.host,"port":1433,"database":self.config.database,"username":principal.username,"auth_mode":principal.auth_mode,"ssl_mode":"verify-full","connection_id":connection,"extra":extra},"driver_context":{"protocol_version":1,"connection_id":connection,"request_id":request,"deadline_ms":REQUEST_MS,"read_only":read_only,"auth":auth.wire(),"session_handle":session},"input":input}})
    }
    fn call(
        &mut self,
        method: &str,
        input: Value,
        role: Role,
        read_only: bool,
        session: Option<&str>,
    ) -> Result<Response> {
        let request = self.request(method, input, role, read_only, session);
        self.report.calls += 1;
        if !read_only {
            self.report.writes_dispatched += 1;
        }
        let response = self.rpc.call(request);
        if response.as_ref().is_ok_and(|r| r.unknown) || (!read_only && response.is_err()) {
            self.report.unknown_write_outcomes += 1;
        }
        response
    }
    fn data(
        &mut self,
        method: &str,
        input: Value,
        role: Role,
        read_only: bool,
        session: Option<&str>,
    ) -> Result<Value> {
        let response = self.call(method, input, role, read_only, session)?;
        if let Some(code) = response.error {
            Err(Error(code))
        } else {
            Ok(response.result)
        }
    }
    fn require(
        &mut self,
        name: &'static str,
        condition: bool,
        evidence: &'static str,
    ) -> Result<()> {
        self.report
            .check(name, if condition { "pass" } else { "fail" }, evidence);
        if condition {
            Ok(())
        } else {
            Err(Error("REQUIRED_CHECK_UNOBSERVED"))
        }
    }
    fn read_row(&mut self, id: i32) -> Result<Value> {
        self.data(
            "execute_query_exact",
            fixture::owned_row(self.config, &self.run_id, id),
            Role::Writer,
            false,
            None,
        )
    }
    fn value_is(&mut self, id: i32, expected: &str) -> Result<bool> {
        let value = self.read_row(id)?;
        Ok(value["rows"] == json!([[self.run_id, id, expected]]))
    }
    fn row_input(&self, operation: &str, id: i32) -> Value {
        if operation == "row.insert" {
            json!({"operation":operation,"table":fixture::table(self.config),"values":fixture::values(&self.run_id,id)})
        } else {
            json!({"operation":operation,"table":fixture::table(self.config),"identity":fixture::identity(&self.run_id,id)})
        }
    }
    fn sequence(&mut self) -> Result<()> {
        let id = uuid::Uuid::new_v4().to_string();
        self.report.calls += 1;
        let capabilities = self.rpc.call(json!({"jsonrpc":"2.0","id":id,"method":"initialize","params":{"service_protocol":1,"settings":{"trust_server_certificate":false}}}))?;
        let caps = &capabilities.result["service_capabilities"];
        self.require(
            "initialize_protocol",
            capabilities.error.is_none()
                && caps["service_protocol"] == 1
                && [
                    "exact_query",
                    "sessions",
                    "cancel",
                    "catalog",
                    "rows",
                    "entra_token",
                ]
                .iter()
                .all(|key| caps[*key] == true)
                && caps["readonly_guarantee"] == "restricted_database_principal",
            "native_capability_response",
        )?;
        let affected_available = caps["exact_affected_rows"] == true;
        let tested = self.data("service_test", json!({}), Role::Reader, true, None)?;
        self.require(
            "reader_auth_verified_tls",
            tested["success"] == true,
            "verify_full_without_overrides",
        )?;
        let engine = self.data("execute_query_exact", json!({"text":"SELECT CAST(SERVERPROPERTY('EngineEdition') AS int),DB_NAME()","parameters":[]}), Role::Reader, true, None)?;
        self.require(
            "azure_sql_database",
            engine["rows"] == json!([[5, self.config.database]]),
            "native_engine_edition_and_database",
        )?;
        let databases = self.data(
            "service_catalog",
            json!({"operation":"catalog.databases"}),
            Role::Reader,
            true,
            None,
        )?;
        self.require(
            "database_catalog_scope",
            databases["databases"] == json!([self.config.database]),
            "configured_database_only",
        )?;
        let schemas = self.data(
            "service_catalog",
            json!({"operation":"catalog.schemas","database":self.config.database}),
            Role::Reader,
            true,
            None,
        )?;
        self.require(
            "schema_catalog",
            schemas["schemas"]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|v| v == &self.config.schema)),
            "configured_schema_present",
        )?;
        let objects = self.data("service_catalog", json!({"operation":"catalog.objects","database":self.config.database,"schema":self.config.schema}), Role::Reader, true, None)?;
        self.require(
            "object_catalog",
            objects["objects"].as_array().is_some_and(|rows| {
                rows.iter().any(|row| {
                    row["name"] == self.config.table
                        && row["schema_name"] == self.config.schema
                        && row["kind"] == "table"
                })
            }),
            "configured_table_present",
        )?;
        let metadata = self.data(
            "service_catalog",
            json!({"operation":"catalog.describe","table":fixture::table(self.config)}),
            Role::Reader,
            true,
            None,
        )?;
        fixture::validate_metadata(&metadata)?;
        self.report.check(
            "existing_fixture_metadata",
            "pass",
            "exact_columns_and_ordered_composite_primary_key",
        );
        for id in 1..=3 {
            self.owned.insert(id);
            let inserted = self.data(
                "service_row",
                self.row_input("row.insert", id),
                Role::Writer,
                false,
                None,
            )?;
            self.require(
                "insert_affected_rows",
                fixture::affected(&inserted),
                "native_count_sentinel",
            )?;
        }
        let rows = self.data(
            "execute_query_exact",
            fixture::all_rows(self.config, &self.run_id),
            Role::Reader,
            true,
            None,
        )?;
        self.require(
            "owned_rows_exact_values",
            fixture::original_rows(&rows),
            "independent_null_decimal38_datetimeoffset_goldens",
        )?;
        if affected_available {
            self.require(
                "exact_affected_rows",
                rows["affected_rows"].as_u64().is_some(),
                "advertised_native_count",
            )?;
        } else {
            self.require(
                "exact_affected_rows_unavailable",
                rows["affected_rows"].is_null()
                    && rows["warnings"].as_array().is_some_and(|warnings| {
                        warnings
                            .iter()
                            .any(|warning| warning == "affected_rows_unavailable")
                    }),
                "reported_null_and_warning",
            )?;
        }
        let multisets = self.data("execute_query_exact", json!({"text":"SELECT @run AS duplicate,@run AS duplicate; SELECT CAST(@decimal AS decimal(38,18)) AS measure,CAST(@when AS datetimeoffset(7)) AS observed_at,CAST(NULL AS nvarchar(100)) AS nullable","parameters":[{"name":"@run","value":self.run_id},{"name":"@decimal","value":fixture::DECIMAL},{"name":"@when","value":fixture::OBSERVED_AT}]}), Role::Reader, true, None)?;
        self.require(
            "duplicate_columns_and_multisets",
            multisets["columns"] == json!(["duplicate", "duplicate"])
                && multisets["rows"] == json!([[self.run_id, self.run_id]])
                && multisets["additional_results"][0]["rows"]
                    == json!([[fixture::DECIMAL, fixture::OBSERVED_AT, null]])
                && multisets["column_metadata"][0]["column_index"] == 0
                && multisets["column_metadata"][1]["column_index"] == 1,
            "separate_result_sets_and_ordinals",
        )?;
        let mut update = self.row_input("row.update", 1);
        update["changes"] = json!({"value":"updated"});
        let updated = self.data("service_row", update.clone(), Role::Writer, false, None)?;
        self.require(
            "update_affected_rows",
            fixture::affected(&updated),
            "native_count_sentinel",
        )?;
        update["identity"] = json!({"columns":["id"],"values":[1]});
        let wrong_key = self.call("service_row", update, Role::Writer, false, None)?;
        self.require(
            "partial_primary_key_rejected",
            wrong_key.error == Some("INVALID_ARGUMENT"),
            "native_catalog_primary_key_guard",
        )?;
        let duplicate = self.call(
            "service_row",
            self.row_input("row.insert", 1),
            Role::Writer,
            false,
            None,
        )?;
        let unchanged = self.value_is(1, "updated")?;
        self.require(
            "duplicate_insert_no_resend",
            duplicate.error.is_some() && unchanged,
            "error_with_observed_unchanged_existing_row",
        )?;
        let deleted = self.data(
            "service_row",
            self.row_input("row.delete", 3),
            Role::Writer,
            false,
            None,
        )?;
        self.require(
            "delete_affected_rows",
            fixture::affected(&deleted),
            "native_count_sentinel",
        )?;
        self.owned.insert(4);
        let denied = self.call(
            "service_row",
            self.row_input("row.insert", 4),
            Role::Reader,
            false,
            None,
        )?;
        self.require(
            "reader_database_permission_denial",
            denied.permission_denial && denied.error.is_some(),
            "native_permission_denial_observed",
        )?;
        let probe = self.read_row(4)?;
        self.require(
            "reader_probe_absent",
            probe["rows"] == json!([]),
            "separate_owned_probe_reread",
        )?;
        self.owned.insert(5);
        let guarded = self.call(
            "service_row",
            self.row_input("row.insert", 5),
            Role::Writer,
            true,
            None,
        )?;
        self.require(
            "writer_readonly_context_guard",
            guarded.error == Some("WRITE_NOT_ALLOWED"),
            "local_policy_guard",
        )?;
        let target = fixture::qualified(self.config);
        let attempts = [
            fixture::exact(format!("WITH target AS (SELECT value FROM {target} WHERE run_id=@run) UPDATE target SET value=N'blocked'; SELECT 1"), &self.run_id),
            fixture::exact(format!("SELECT 1; UPDATE {target} SET value=N'blocked' WHERE run_id=@run"), &self.run_id),
            json!({"text":"EXEC sys.sp_executesql @stmt,N'@run nvarchar(36)',@run=@run","parameters":[{"name":"@stmt","value":format!("UPDATE {target} SET value=N'blocked' WHERE run_id=@run")},{"name":"@run","value":self.run_id}]}),
        ];
        for input in attempts {
            let denied = self.call("execute_query_exact", input, Role::Reader, true, None)?;
            let unchanged = self.value_is(1, "updated")?;
            self.require(
                "reader_readonly_write_rejected",
                denied.error == Some("WRITE_NOT_ALLOWED") && unchanged,
                "restricted_principal_and_unchanged_owned_row",
            )?;
        }
        self.rotation()?;
        self.cancellation()?;
        Ok(())
    }
    fn rotation(&mut self) -> Result<()> {
        let Some(refresh) = self.credentials.refresh.clone() else {
            self.report.check(
                "supplied_token_rotation",
                "not_applicable",
                "sql_password_mode",
            );
            return Ok(());
        };
        let opened = self.data(
            "service_session_open",
            json!({"database":self.config.database,"mode":"write"}),
            Role::Writer,
            false,
            None,
        )?;
        let handle = opened["session_handle"]
            .as_str()
            .filter(|v| !v.is_empty())
            .ok_or(Error("INVALID_SESSION_RESPONSE"))?
            .to_owned();
        self.session = Some(handle.clone());
        let first = self.data(
            "execute_query_exact",
            json!({"text":"SELECT @@SPID","parameters":[]}),
            Role::Writer,
            false,
            Some(&handle),
        )?;
        let second = self.data(
            "execute_query_exact",
            json!({"text":"SELECT @@SPID","parameters":[]}),
            Role::Refresh,
            false,
            Some(&handle),
        )?;
        self.require(
            "reserved_session_token_identity",
            first["rows"][0][0].as_u64().is_some() && first["rows"] == second["rows"],
            "same_reserved_physical_spid",
        )?;
        let rollback = self.data(
            "service_session_finish",
            json!({"action":"rollback"}),
            Role::Refresh,
            false,
            Some(&handle),
        )?;
        self.session = None;
        self.require(
            "session_rollback",
            rollback["rollback_confirmed"] == true && rollback["in_transaction"] == false,
            "native_rollback_confirmation",
        )?;
        let invalidated = self.data(
            "service_invalidate_auth",
            json!({}),
            Role::Refresh,
            false,
            None,
        )?;
        self.require(
            "auth_invalidation",
            invalidated["invalidated"] == true,
            "discard_acceptance_without_rollback_claim",
        )?;
        let fresh = self.data("service_test", json!({}), Role::Refresh, false, None)?;
        self.require(
            "supplied_token_rotation",
            fresh["success"] == true,
            "supplied_token_rotation",
        )?;
        let mut expired = refresh.clone();
        if let Auth::EntraToken { expires_at_ms, .. } = &mut expired {
            *expires_at_ms = 0;
        }
        let mut scope = refresh.clone();
        if let Auth::EntraToken { scope, .. } = &mut scope {
            *scope = "invalid-scope".into();
        }
        let mut tenant = refresh.clone();
        if let Auth::EntraToken { tenant_id, .. } = &mut tenant {
            *tenant_id = uuid::Uuid::nil().to_string();
        }
        let mut principal = refresh.clone();
        if let Auth::EntraToken { principal_id, .. } = &mut principal {
            *principal_id = uuid::Uuid::nil().to_string();
        }
        self.require(
            "token_local_contract_rejections",
            expired
                .validate(&self.config.writer, super::now_ms())
                .is_err()
                && scope
                    .validate(&self.config.writer, super::now_ms())
                    .is_err()
                && tenant
                    .validate(&self.config.writer, super::now_ms())
                    .is_err()
                && !refresh_matches(&self.credentials.writer, &principal),
            "local_contract_only",
        )
    }
    fn cancellation(&mut self) -> Result<()> {
        let request = self.request(
            "execute_query_exact",
            json!({"text":"WAITFOR DELAY '00:00:10'; SELECT 1","parameters":[]}),
            Role::Writer,
            false,
            None,
        );
        let id = request["id"]
            .as_str()
            .ok_or(Error("INVALID_REQUEST"))?
            .to_owned();
        self.report.calls += 1;
        self.report.writes_dispatched += 1;
        self.rpc.send(request)?;
        let other = self.data("execute_query_exact", json!({"text":"SELECT CAST(@decimal AS decimal(38,18))","parameters":[{"name":"@decimal","value":fixture::DECIMAL}]}), Role::Reader, true, None)?;
        self.require(
            "parallel_reader_connection",
            other["rows"] == json!([[fixture::DECIMAL]]),
            "separate_connection_result",
        )?;
        let cancel_id = uuid::Uuid::new_v4().to_string();
        self.report.calls += 1;
        let ack = self.rpc.call(json!({"jsonrpc":"2.0","id":cancel_id,"method":"cancel_request","params":{"connection_id":self.writer_connection,"request_id":id}}))?;
        self.require(
            "scoped_cancel_ack",
            ack.error.is_none() && ack.result["cancellation_requested"] == true,
            "native_request_scope_ack",
        )?;
        let stopped = self.rpc.receive(&id)?;
        if stopped.unknown {
            self.report.unknown_write_outcomes += 1;
        }
        self.report.cancellation_phase = "native_request";
        self.report.cancellation_write_outcome = if stopped.unknown {
            "unknown"
        } else {
            "not_applied"
        };
        self.require(
            "native_request_cancelled",
            matches!(
                stopped.error,
                Some("OUTCOME_UNKNOWN" | "CANCELLED" | "DEADLINE_EXCEEDED")
            ),
            "request_cancel_result_without_server_phase_claim",
        )?;
        let final_read = self.data("service_test", json!({}), Role::Reader, true, None)?;
        self.require(
            "reader_after_cancel",
            final_read["success"] == true,
            "other_connection_remains_usable",
        )
    }
    fn cleanup(&mut self) {
        if let Some(handle) = self.session.take() {
            let closed = self.call(
                "service_session_finish",
                json!({"action":"rollback"}),
                Role::Writer,
                false,
                Some(&handle),
            );
            if !closed
                .as_ref()
                .is_ok_and(|r| r.error.is_none() && r.result["rollback_confirmed"] == true)
            {
                self.report.error.get_or_insert("SESSION_CLEANUP_FAILED");
            }
        }
        for id in self.owned.clone() {
            let attempt = (|| {
                let value = self.read_row(id)?;
                let rows = value["rows"]
                    .as_array()
                    .ok_or(Error("CLEANUP_READ_FAILED"))?;
                if rows.is_empty() {
                    return Ok(());
                }
                if rows.len() != 1 || rows[0][0] != self.run_id || rows[0][1] != id {
                    return Err(Error("CLEANUP_FOREIGN_RUN"));
                }
                let deleted = self.data(
                    "service_row",
                    self.row_input("row.delete", id),
                    Role::Writer,
                    false,
                    None,
                )?;
                if !fixture::affected(&deleted) {
                    return Err(Error("CLEANUP_DELETE_FAILED"));
                }
                let remaining = self.read_row(id)?;
                if remaining["rows"] != json!([]) {
                    return Err(Error("CLEANUP_ROW_REMAINS"));
                }
                Ok(())
            })();
            match attempt {
                Ok(()) => {
                    self.owned.remove(&id);
                }
                Err(error) => {
                    self.report.error.get_or_insert(error.0);
                }
            }
        }
        self.report.cleanup_remaining = self.owned.len();
        if !self.owned.is_empty() {
            self.report.remaining_id_sha256 = Some(format!(
                "{:x}",
                Sha256::digest(serde_json::to_vec(&self.owned).unwrap_or_default())
            ));
        }
        self.report.check(
            "owned_fixture_cleanup",
            if self.owned.is_empty() {
                "pass"
            } else {
                "fail"
            },
            "reread_complete_primary_key_one_delete_attempt",
        );
    }
}

pub fn run(config: &Config, credentials: &Credentials, rpc: &mut dyn Rpc) -> Report {
    let started = Instant::now();
    let mut report = Report::new();
    report.evidence_kind = rpc.evidence_kind();
    report.auth_modes = vec![
        config.writer.auth_mode.clone(),
        config.reader.auth_mode.clone(),
    ];
    let mut harness = Harness {
        config,
        credentials,
        rpc,
        report: &mut report,
        run_id: uuid::Uuid::new_v4().to_string(),
        writer_connection: uuid::Uuid::new_v4().to_string(),
        reader_connection: uuid::Uuid::new_v4().to_string(),
        owned: BTreeSet::new(),
        session: None,
    };
    if let Err(error) = harness.sequence() {
        harness.report.error = Some(error.0);
    }
    harness.cleanup();
    if let Err(error) = harness.rpc.shutdown() {
        harness
            .report
            .error
            .get_or_insert(if error.0 == "DRIVER_SHUTDOWN_TIMEOUT" {
                error.0
            } else {
                "DRIVER_SHUTDOWN_FAILED"
            });
    }
    report.elapsed_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
    report.deferred();
    report.success = report.error.is_none() && report.cleanup_remaining == 0;
    report
}

pub fn authorized(
    config: &Config,
    flags: Flags,
    reader: &mut dyn SecretReader,
    factory: impl FnOnce(&Credentials) -> Result<Box<dyn Rpc>>,
) -> Report {
    let preflight = (|| {
        flags.validate()?;
        config.validate()?;
        let credentials = Credentials::read(reader, config, super::now_ms())?;
        let mut rpc = factory(&credentials)?;
        Ok(run(config, &credentials, rpc.as_mut()))
    })();
    let mut report = match preflight {
        Ok(report) => report,
        Err(Error(code)) => {
            let mut report = Report::new();
            report.error = Some(code);
            report.deferred();
            report
        }
    };
    if let Some(path) = &config.report {
        if let Err(error) = report::publish(&report, Path::new(path)) {
            report.success = false;
            report.error = Some(error.0);
        }
    }
    report
}
pub fn run_live_from_stdin() -> Report {
    let preflight = (|| {
        let flags = Flags {
            live: std::env::var("TABULARIS_AZURE_SQL_ALLOW_LIVE")
                .ok()
                .as_deref()
                == Some("1"),
            fixture_writes: std::env::var("TABULARIS_AZURE_SQL_ALLOW_FIXTURE_WRITES")
                .ok()
                .as_deref()
                == Some("1"),
        };
        flags.validate()?;
        let path =
            std::env::var_os("TABULARIS_AZURE_SQL_TEST_CONFIG").ok_or(Error("CONFIG_REQUIRED"))?;
        let config = config::load(Path::new(&path))?;
        Ok(authorized(
            &config,
            flags,
            &mut StdinSecrets,
            |credentials| LiveRpc::spawn(credentials).map(|rpc| Box::new(rpc) as Box<dyn Rpc>),
        ))
    })();
    let report = match preflight {
        Ok(report) => report,
        Err(Error(code)) => {
            let mut report = Report::new();
            report.error = Some(code);
            report.deferred();
            report
        }
    };
    if let Ok(bytes) = report.bytes() {
        println!("{}", String::from_utf8_lossy(&bytes));
    }
    report
}
