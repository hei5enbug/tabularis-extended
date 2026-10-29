use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    time::Duration,
};
pub const PASSWORD: &str = "SyntheticService!42";
/// The principal tests name "sa" maps to the administrator that `SQLSERVER_TEST_USER`
/// and `SQLSERVER_TEST_PASSWORD` configure, like the other live test targets.
pub const ADMIN: &str = "sa";
fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}
/// Docker container that runs the server, for fixture SQL through `sqlcmd`.
pub fn container() -> String {
    env_or(
        "SQLSERVER_TEST_CONTAINER",
        "tabularis-plan-sqlserver2022-02-01a1000d",
    )
}
pub fn admin_user() -> String {
    env_or("SQLSERVER_TEST_USER", ADMIN)
}
pub fn admin_password() -> String {
    env_or("SQLSERVER_TEST_PASSWORD", "Str0ng!Passw0rd")
}
fn port() -> u16 {
    env_or("SQLSERVER_TEST_PORT", "63304")
        .parse()
        .expect("SQLSERVER_TEST_PORT must be a valid port")
}
pub fn sql(database: &str, text: &str) {
    let output = Command::new("docker")
        .args([
            "--config",
            "/tmp/tabularis-docker-public",
            "exec",
            &container(),
            "/opt/mssql-tools18/bin/sqlcmd",
            "-S",
            "localhost",
            "-U",
            &admin_user(),
            "-P",
            &admin_password(),
            "-C",
            "-b",
            "-d",
            database,
            "-Q",
            text,
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "Synthetic fixture SQL failed: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}
pub fn sql_scalar(database: &str, text: &str) -> i64 {
    let output = Command::new("docker")
        .args([
            "--config",
            "/tmp/tabularis-docker-public",
            "exec",
            &container(),
            "/opt/mssql-tools18/bin/sqlcmd",
            "-S",
            "localhost",
            "-U",
            &admin_user(),
            "-P",
            &admin_password(),
            "-C",
            "-b",
            "-h",
            "-1",
            "-W",
            "-d",
            database,
            "-Q",
            &format!("SET NOCOUNT ON; SET QUOTED_IDENTIFIER ON; {text}"),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "Synthetic fixture observation failed: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .trim()
        .parse()
        .expect("Synthetic observation must return an integer")
}
pub struct Fixture {
    pub database: String,
    pub reader: String,
    pub writer: String,
}
impl Fixture {
    pub fn new() -> Self {
        let run = uuid::Uuid::new_v4().simple().to_string();
        let database = format!("a1_{}", &run[..16]);
        let reader = format!("{database}_reader");
        let writer = format!("{database}_writer");
        sql("master", &format!("CREATE DATABASE [{database}]; CREATE LOGIN [{reader}] WITH PASSWORD='{PASSWORD}', CHECK_POLICY=OFF; CREATE LOGIN [{writer}] WITH PASSWORD='{PASSWORD}', CHECK_POLICY=OFF;"));
        sql(&database, &format!("CREATE USER [{reader}] FOR LOGIN [{reader}]; CREATE USER [{writer}] FOR LOGIN [{writer}]; ALTER ROLE db_datareader ADD MEMBER [{reader}]; ALTER ROLE db_denydatawriter ADD MEMBER [{reader}]; ALTER ROLE db_datareader ADD MEMBER [{writer}]; ALTER ROLE db_datawriter ADD MEMBER [{writer}]; CREATE TABLE dbo.items(id int PRIMARY KEY,value nvarchar(100)); INSERT dbo.items VALUES(1,'initial');"));
        Self {
            database,
            reader,
            writer,
        }
    }
    pub fn params(&self, principal: &str) -> Value {
        let username = if principal == ADMIN {
            admin_user()
        } else {
            principal.to_owned()
        };
        json!({"driver":"sqlserver","host":env_or("SQLSERVER_TEST_HOST","127.0.0.1"),"port":port(),"database":self.database,"username":username,"ssl_mode":"require"})
    }
    pub fn request(
        &self,
        method: &str,
        input: Value,
        principal: &str,
        read_only: bool,
        request: &str,
    ) -> Value {
        let params = self.params(principal);
        let password = if principal == ADMIN {
            admin_password()
        } else {
            PASSWORD.to_owned()
        };
        json!({"jsonrpc":"2.0","id":request,"method":method,"params":{"params":params,"driver_context":{"protocol_version":1,"connection_id":"service-fixture","request_id":request,"deadline_ms":10_000,"read_only":read_only,"auth":{"kind":"db_password","password":password,"identity":params["username"]}},"input":input}})
    }
    pub fn exact(&self, text: &str, principal: &str, read_only: bool, request: &str) -> Value {
        self.request(
            "execute_query_exact",
            json!({"text":text,"parameters":[]}),
            principal,
            read_only,
            request,
        )
    }
    pub fn observe(&self, text: &str, request: &str) -> Value {
        let mut query = self.exact(text, "sa", false, request);
        query["params"]["driver_context"]["connection_id"] = json!("fixture-observer");
        query
    }
    pub fn attention_capture(&self) {
        sql("master", &format!("CREATE EVENT SESSION [{}] ON SERVER ADD EVENT sqlserver.attention(ACTION(sqlserver.session_id)) ADD TARGET package0.ring_buffer; ALTER EVENT SESSION [{}] ON SERVER STATE=START;", self.database, self.database));
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        sql("master", &format!("IF EXISTS(SELECT 1 FROM sys.server_event_sessions WHERE name='{}') BEGIN ALTER EVENT SESSION [{}] ON SERVER STATE=STOP; DROP EVENT SESSION [{}] ON SERVER; END; ALTER DATABASE [{}] SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE [{}]; DROP LOGIN [{}]; DROP LOGIN [{}];", self.database, self.database, self.database, self.database, self.database, self.reader, self.writer));
    }
}
pub struct Plugin {
    child: Child,
    stdin: Option<ChildStdin>,
    responses: mpsc::Receiver<Value>,
    pending: HashMap<String, Value>,
    stderr: mpsc::Receiver<Vec<u8>>,
}
impl Plugin {
    pub fn new() -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_sqlserver-plugin"));
        command.env_clear();
        for key in ["PATH", "TMPDIR"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let (tx, responses) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else {
                    break;
                };
                let Ok(response) = serde_json::from_str(&line) else {
                    break;
                };
                if tx.send(response).is_err() {
                    break;
                }
            }
        });
        let (errors, stderr_rx) = mpsc::channel();
        std::thread::spawn(move || {
            use std::io::Read;
            let mut data = Vec::new();
            let _ = BufReader::new(stderr).read_to_end(&mut data);
            let _ = errors.send(data);
        });
        Self {
            child,
            stdin,
            responses,
            pending: HashMap::new(),
            stderr: stderr_rx,
        }
    }
    pub fn send(&mut self, request: &Value) {
        self.raw(format!("{}\n", serde_json::to_string(request).unwrap()).as_bytes());
    }
    pub fn raw(&mut self, bytes: &[u8]) {
        let stdin = self.stdin.as_mut().unwrap();
        stdin.write_all(bytes).unwrap();
        stdin.flush().unwrap();
    }
    pub fn receive(&mut self, id: &Value) -> Value {
        let key = id.to_string();
        if let Some(response) = self.pending.remove(&key) {
            return response;
        }
        loop {
            let response = self
                .responses
                .recv_timeout(Duration::from_secs(15))
                .expect("Synthetic RPC did not finish in time");
            if response["id"] == *id {
                return response;
            }
            self.pending.insert(response["id"].to_string(), response);
        }
    }
    pub fn call(&mut self, request: &Value) -> Value {
        self.send(request);
        self.receive(&request["id"])
    }
    pub fn receive_any(&mut self, ids: &[Value]) -> Value {
        for id in ids {
            if let Some(response) = self.pending.remove(&id.to_string()) {
                return response;
            }
        }
        loop {
            let response = self
                .responses
                .recv_timeout(Duration::from_secs(15))
                .expect("Synthetic RPC did not finish in time");
            if ids.iter().any(|id| response["id"] == *id) {
                return response;
            }
            self.pending.insert(response["id"].to_string(), response);
        }
    }
    pub fn eof(&mut self) {
        self.stdin.take();
    }
    pub fn wait(&mut self) -> Vec<u8> {
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            if self.child.try_wait().unwrap().is_some() {
                return self.stderr.recv_timeout(Duration::from_secs(1)).unwrap();
            }
            assert!(
                std::time::Instant::now() < deadline,
                "Synthetic plugin did not exit"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Plugin {
    fn drop(&mut self) {
        self.stdin.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
pub fn scalar(plugin: &mut Plugin, fixture: &Fixture, text: &str, request: &str) -> i64 {
    let actual = plugin.call(&fixture.observe(text, request));
    actual["result"]["rows"][0][0]
        .as_i64()
        .expect("Synthetic observation must return an integer")
}
pub fn wait_active(plugin: &mut Plugin, fixture: &Fixture) -> i64 {
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    loop {
        let spid = scalar(plugin, fixture, "SELECT COALESCE(MAX(session_id),0) FROM sys.dm_exec_requests WHERE database_id=DB_ID() AND wait_type='WAITFOR'", "observe_active");
        if spid != 0 {
            return spid;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Synthetic WAITFOR did not begin"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
pub fn connection(plugin: &mut Plugin, fixture: &Fixture, spid: i64) -> String {
    let actual = plugin.call(&fixture.observe(&format!("SELECT CONVERT(nvarchar(36),connection_id) FROM sys.dm_exec_connections WHERE session_id={spid}"), "observe_connection"));
    actual["result"]["rows"][0][0]
        .as_str()
        .expect("Synthetic connection must have a GUID")
        .to_owned()
}
pub fn gone(plugin: &mut Plugin, fixture: &Fixture, connection: &str) -> bool {
    uuid::Uuid::parse_str(connection).expect("Synthetic connection must have a GUID");
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        if scalar(plugin, fixture, &format!("SELECT COUNT(*) FROM sys.dm_exec_connections WHERE connection_id=CONVERT(uniqueidentifier,'{connection}')"), "observe_closed") == 0 { return true; }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
pub fn attention(plugin: &mut Plugin, fixture: &Fixture, spid: i64) -> i64 {
    let query = format!("SELECT CAST(t.target_data AS xml).value('count(/RingBufferTarget/event[@name=\"attention\"]/action[@name=\"session_id\"]/value[text()=\"{spid}\"])','int') FROM sys.dm_xe_session_targets t JOIN sys.dm_xe_sessions s ON t.event_session_address=s.address WHERE s.name='{}'", fixture.database);
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        let count = scalar(plugin, fixture, &query, "observe_attention");
        if count > 0 || std::time::Instant::now() >= deadline {
            return count;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
pub struct PipeOwner {
    child: Child,
    stdin: ChildStdin,
}
impl PipeOwner {
    pub fn new() -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_sqlserver-plugin"));
        command.env_clear();
        for key in ["PATH", "TMPDIR"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        Self { child, stdin }
    }
    pub fn send(&mut self, request: &Value) {
        self.stdin
            .write_all(format!("{}\n", request).as_bytes())
            .unwrap();
        self.stdin.flush().unwrap();
    }
    pub fn fail_stdout(&mut self) {
        self.child.stdout.take();
        self.send(&json!({"jsonrpc":"2.0","id":"wake_writer","method":"initialize","params":{}}));
    }
    pub fn wait(&mut self) -> Vec<u8> {
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(
                    status.success(),
                    "Synthetic owner failure must exit cleanly"
                );
                use std::io::Read;
                let mut data = Vec::new();
                self.child
                    .stderr
                    .take()
                    .unwrap()
                    .read_to_end(&mut data)
                    .unwrap();
                return data;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "Synthetic owner failure did not exit"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for PipeOwner {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
