use super::{
    config::{self, Config, Flags},
    credentials::{Auth, Credentials, SecretReader, StdinSecrets},
    fixture,
    report::{self, Report},
    rpc::{LiveRpc, Response, Rpc},
    Error, Result, REQUEST_MS,
};
use serde_json::{json, Value};
use std::{path::Path, time::Instant};

struct Harness<'a> {
    config: &'a Config,
    credentials: &'a Credentials,
    rpc: &'a mut dyn Rpc,
    report: &'a mut Report,
    connection: String,
    parallel_connection: String,
    session: Option<String>,
}
impl Harness<'_> {
    /// Connection fields exactly as the host sends them: Entra identity lives
    /// in `extra`, and no token or password ever travels with the request.
    fn extra(&self) -> Value {
        let principal = &self.config.principal;
        if !principal.is_entra() {
            return json!({});
        }
        let mut extra = json!({
            "auth_source": "azure_cli",
            "tenant_id": principal.tenant_id,
            "entra_object_id": principal.entra_object_id,
        });
        if let Some(path) = &principal.azure_cli_path {
            extra["azure_cli_path"] = json!(path);
        }
        extra
    }
    fn request(
        &self,
        connection: &str,
        method: &str,
        input: Value,
        session: Option<&str>,
    ) -> Value {
        let request = uuid::Uuid::new_v4().to_string();
        let principal = &self.config.principal;
        let auth = self
            .credentials
            .principal
            .as_ref()
            .map_or(Value::Null, Auth::wire);
        json!({"jsonrpc":"2.0","id":request,"method":method,"params":{"params":{"driver":"sqlserver-plus","host":self.config.host,"port":1433,"database":self.config.database,"username":principal.username,"auth_mode":principal.auth_mode,"ssl_mode":"verify-full","connection_id":connection,"extra":self.extra()},"driver_context":{"protocol_version":1,"connection_id":connection,"request_id":request,"deadline_ms":REQUEST_MS,"read_only":true,"auth":auth,"session_handle":session},"input":input}})
    }
    fn call(&mut self, method: &str, input: Value, session: Option<&str>) -> Result<Response> {
        let request = self.request(&self.connection.clone(), method, input, session);
        self.report.calls += 1;
        self.rpc.call(request)
    }
    fn data(&mut self, method: &str, input: Value, session: Option<&str>) -> Result<Value> {
        let response = self.call(method, input, session)?;
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
    fn sequence(&mut self) -> Result<()> {
        let id = uuid::Uuid::new_v4().to_string();
        self.report.calls += 1;
        let capabilities = self.rpc.call(json!({"jsonrpc":"2.0","id":id,"method":"initialize","params":{"service_protocol":1,"settings":{"trust_server_certificate":false}}}))?;
        let caps = &capabilities.result["service_capabilities"];
        self.require(
            "initialize_protocol",
            capabilities.error.is_none()
                && caps["service_protocol"] == 1
                && ["exact_query", "sessions", "cancel", "catalog", "rows"]
                    .iter()
                    .all(|key| caps[*key] == true)
                && caps["entra_token"] == false
                && caps.get("readonly_guarantee").is_none(),
            "native_capability_response_without_host_token",
        )?;
        let tested = self.data("service_test", json!({}), None)?;
        self.require(
            "principal_auth_verified_tls",
            tested["success"] == true,
            "verify_full_without_overrides",
        )?;
        if self.config.principal.is_entra() {
            self.report.check(
                "driver_owned_entra_login",
                "pass",
                "azure_cli_token_acquired_by_driver",
            );
        } else {
            self.report.check(
                "driver_owned_entra_login",
                "not_applicable",
                "sql_password_mode",
            );
        }
        let engine = self.data("execute_query_exact", json!({"text":"SELECT CAST(SERVERPROPERTY('EngineEdition') AS int),DB_NAME()","parameters":[]}), None)?;
        self.require(
            "azure_sql_database",
            engine["rows"] == json!([[5, self.config.database]]),
            "native_engine_edition_and_database",
        )?;
        let databases = self.data(
            "service_catalog",
            json!({"operation":"catalog.databases"}),
            None,
        )?;
        self.require(
            "database_catalog_scope",
            databases["databases"]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|v| v == &self.config.database)),
            "configured_database_listed",
        )?;
        let schemas = self.data(
            "service_catalog",
            json!({"operation":"catalog.schemas","database":self.config.database}),
            None,
        )?;
        self.require(
            "schema_catalog",
            schemas["schemas"]
                .as_array()
                .is_some_and(|rows| !rows.is_empty()),
            "schemas_listed",
        )?;
        let multisets = self.data("execute_query_exact", json!({"text":"SELECT @run AS duplicate,@run AS duplicate; SELECT CAST(@decimal AS decimal(38,18)) AS measure,CAST(@when AS datetimeoffset(7)) AS observed_at,CAST(NULL AS nvarchar(100)) AS nullable","parameters":[{"name":"@run","value":uuid::Uuid::new_v4().to_string()},{"name":"@decimal","value":fixture::DECIMAL},{"name":"@when","value":fixture::OBSERVED_AT}]}), None)?;
        self.require(
            "duplicate_columns_and_multisets",
            multisets["columns"] == json!(["duplicate", "duplicate"])
                && multisets["rows"][0][0] == multisets["rows"][0][1]
                && multisets["additional_results"][0]["rows"]
                    == json!([[fixture::DECIMAL, fixture::OBSERVED_AT, null]])
                && multisets["column_metadata"][0]["column_index"] == 0
                && multisets["column_metadata"][1]["column_index"] == 1,
            "separate_result_sets_decimal38_datetimeoffset_null",
        )?;
        self.reserved_session()?;
        self.cancellation()?;
        Ok(())
    }
    fn reserved_session(&mut self) -> Result<()> {
        let opened = self.data(
            "service_session_open",
            json!({"database":self.config.database,"mode":"read_only"}),
            None,
        )?;
        let handle = opened["session_handle"]
            .as_str()
            .filter(|v| !v.is_empty())
            .ok_or(Error("INVALID_SESSION_RESPONSE"))?
            .to_owned();
        self.session = Some(handle.clone());
        let spid = json!({"text":"SELECT @@SPID","parameters":[]});
        let first = self.data("execute_query_exact", spid.clone(), Some(&handle))?;
        let second = self.data("execute_query_exact", spid, Some(&handle))?;
        self.require(
            "reserved_session_identity",
            first["rows"][0][0].as_u64().is_some() && first["rows"] == second["rows"],
            "same_reserved_physical_spid",
        )?;
        let rollback = self.data(
            "service_session_finish",
            json!({"action":"rollback"}),
            Some(&handle),
        )?;
        self.session = None;
        self.require(
            "session_rollback",
            rollback["rollback_confirmed"] == true && rollback["in_transaction"] == false,
            "native_rollback_confirmation",
        )?;
        let invalidated = self.data("service_invalidate_auth", json!({}), None)?;
        self.require(
            "auth_invalidation",
            invalidated["invalidated"] == true,
            "discard_acceptance_without_rollback_claim",
        )?;
        let fresh = self.data("service_test", json!({}), None)?;
        self.require(
            "fresh_login_after_invalidation",
            fresh["success"] == true,
            "new_physical_login_after_invalidation",
        )
    }
    fn cancellation(&mut self) -> Result<()> {
        let request = self.request(
            &self.connection.clone(),
            "execute_query_exact",
            json!({"text":"WAITFOR DELAY '00:00:10'; SELECT 1","parameters":[]}),
            None,
        );
        let id = request["id"]
            .as_str()
            .ok_or(Error("INVALID_REQUEST"))?
            .to_owned();
        self.report.calls += 1;
        self.rpc.send(request)?;
        let parallel = self.request(
            &self.parallel_connection.clone(),
            "execute_query_exact",
            json!({"text":"SELECT CAST(@decimal AS decimal(38,18))","parameters":[{"name":"@decimal","value":fixture::DECIMAL}]}),
            None,
        );
        self.report.calls += 1;
        let other = self.rpc.call(parallel)?;
        self.require(
            "parallel_connection",
            other.error.is_none() && other.result["rows"] == json!([[fixture::DECIMAL]]),
            "separate_connection_result",
        )?;
        let cancel_id = uuid::Uuid::new_v4().to_string();
        self.report.calls += 1;
        let ack = self.rpc.call(json!({"jsonrpc":"2.0","id":cancel_id,"method":"cancel_request","params":{"connection_id":self.connection,"request_id":id}}))?;
        self.require(
            "scoped_cancel_ack",
            ack.error.is_none() && ack.result["cancellation_requested"] == true,
            "native_request_scope_ack",
        )?;
        let stopped = self.rpc.receive(&id)?;
        self.report.cancellation_phase = "native_request";
        self.report.cancellation_outcome = match stopped.error {
            Some("CANCELLED") => "cancelled",
            Some("DEADLINE_EXCEEDED") => "deadline_exceeded",
            Some("OUTCOME_UNKNOWN") => "unknown",
            _ => "not_observed",
        };
        self.require(
            "native_request_cancelled",
            self.report.cancellation_outcome != "not_observed",
            "request_cancel_result_without_server_phase_claim",
        )?;
        let after = self.data("service_test", json!({}), None)?;
        self.require(
            "connection_after_cancel",
            after["success"] == true,
            "same_principal_remains_usable",
        )
    }
    fn cleanup(&mut self) {
        if let Some(handle) = self.session.take() {
            let closed = self.call(
                "service_session_finish",
                json!({"action":"rollback"}),
                Some(&handle),
            );
            if !closed
                .as_ref()
                .is_ok_and(|r| r.error.is_none() && r.result["rollback_confirmed"] == true)
            {
                self.report.error.get_or_insert("SESSION_CLEANUP_FAILED");
            }
        }
    }
}

pub fn run(config: &Config, credentials: &Credentials, rpc: &mut dyn Rpc) -> Report {
    let started = Instant::now();
    let mut report = Report::new();
    report.evidence_kind = rpc.evidence_kind();
    report.auth_mode = config.principal.auth_mode.clone();
    let mut harness = Harness {
        config,
        credentials,
        rpc,
        report: &mut report,
        connection: uuid::Uuid::new_v4().to_string(),
        parallel_connection: uuid::Uuid::new_v4().to_string(),
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
    report.success = report.error.is_none();
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
        let credentials = Credentials::read(reader, config)?;
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
        };
        flags.validate()?;
        let path =
            std::env::var_os("TABULARIS_AZURE_SQL_TEST_CONFIG").ok_or(Error("CONFIG_REQUIRED"))?;
        let config = config::load(Path::new(&path))?;
        Ok(authorized(
            &config,
            flags,
            &mut StdinSecrets,
            |credentials| {
                LiveRpc::spawn(&config, credentials).map(|rpc| Box::new(rpc) as Box<dyn Rpc>)
            },
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
