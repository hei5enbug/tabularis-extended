use std::convert::Infallible;
use std::fs;
use std::path::PathBuf;
use std::process::Command as StdCommand;
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use mssql_tiberius_bridge::Config;
use serde_json::{json, Value};
use tokio::time::Instant;

use super::*;
use crate::driver::pool::build_config;

fn claims(now: i64) -> Value {
    json!({
        "tid": "TENANT-GUID",
        "oid": "OBJECT-GUID",
        "upn": "PERSON@example.test",
        "unique_name": "PERSON@example.test",
        "preferred_username": "PERSON@example.test",
        "aud": SQL_RESOURCE,
        "exp": now + 3600
    })
}

fn token(claims: &Value) -> String {
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).unwrap());
    let signature = URL_SAFE_NO_PAD.encode(b"synthetic-signature");
    format!("{header}.{payload}.{signature}")
}

fn params() -> ConnectionParams {
    let mut params = ConnectionParams {
        driver: "sqlserver".into(),
        host: Some("sample.database.windows.net".into()),
        port: Some(1433),
        username: Some("person@example.test".into()),
        password: None,
        ssl_mode: Some("verify-full".into()),
        database: crate::models::DatabaseSelection::Single("master".into()),
        ..Default::default()
    };
    params.extra.insert("auth_mode".into(), "entra_user".into());
    params
        .extra
        .insert("auth_source".into(), "azure_cli".into());
    params
        .extra
        .insert("tenant_id".into(), "tenant-guid".into());
    params
        .extra
        .insert("entra_object_id".into(), "object-guid".into());
    params
}

#[cfg(unix)]
struct FakeCli {
    directory: PathBuf,
    executable: PathBuf,
    calls: PathBuf,
}

#[cfg(unix)]
impl FakeCli {
    fn new(script: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;
        use std::sync::atomic::{AtomicU64, Ordering};

        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "sqlserver-plus-azure-cli-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&directory).unwrap();
        let executable = directory.join("az");
        let calls = directory.join("calls");
        fs::write(&executable, script).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            directory,
            executable,
            calls,
        }
    }

    fn auth(&self) -> AzureCliAuth {
        let mut params = params();
        params.extra.insert(
            "azure_cli_path".into(),
            self.executable.to_string_lossy().into_owned(),
        );
        AzureCliAuth::from_params(&params).unwrap().unwrap()
    }
}

#[cfg(unix)]
impl Drop for FakeCli {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

#[cfg(unix)]
fn shell_quote(path: &std::path::Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

#[cfg(unix)]
fn token_cli(token: &str, calls_path: &std::path::Path) -> String {
    format!(
        "#!/bin/sh\n[ \"$1\" = account ] && [ \"$2\" = get-access-token ] && [ \"$3\" = --resource ] && [ \"$4\" = {resource} ] && [ \"$5\" = --tenant ] && [ \"$6\" = tenant-guid ] && [ \"$7\" = --output ] && [ \"$8\" = json ] || exit 17\nprintf 'called\\n' >> {calls}\nprintf '%s\\n' '{{\"accessToken\":\"{token}\"}}'\n",
        resource = SQL_RESOURCE,
        calls = shell_quote(calls_path),
    )
}

/// JWT 메타데이터가 설정된 Entra 사용자의 신원과 일치하면 토큰을 허용한다.
#[test]
fn accepts_token_with_matching_identity_and_case_insensitive_username() {
    // given
    let now = 2_000_000_000;
    let jwt = token(&claims(now));

    // when
    let result = validate_access_token(
        &jwt,
        "tenant-guid",
        "object-guid",
        "person@example.test",
        now,
    );

    // then
    assert_eq!(result, Ok(()));
}

/// JWT의 테넌트, principal, audience 및 만료 시각이 맞지 않으면 토큰을 거부한다.
#[test]
fn rejects_token_with_mismatched_metadata_or_near_expiry() {
    // given
    let now = 2_000_000_000;
    let mut wrong_tenant = claims(now);
    wrong_tenant["tid"] = json!("another-tenant");
    let mut wrong_object = claims(now);
    wrong_object["oid"] = json!("another-object");
    let mut wrong_user = claims(now);
    wrong_user["preferred_username"] = json!("another@example.test");
    let mut wrong_audience = claims(now);
    wrong_audience["aud"] = json!("https://management.azure.com/");
    let mut near_expiry = claims(now);
    near_expiry["exp"] = json!(now + 120);
    let cases = [
        wrong_tenant,
        wrong_object,
        wrong_user,
        wrong_audience,
        near_expiry,
    ];

    // when
    let results = cases
        .iter()
        .map(|case| {
            validate_access_token(
                &token(case),
                "tenant-guid",
                "object-guid",
                "person@example.test",
                now,
            )
        })
        .collect::<Vec<_>>();

    // then
    assert!(results.iter().all(Result::is_err));
}

/// JWT 형식이 깨졌거나 사용자 신원 필드가 빠지면 토큰을 거부한다.
#[test]
fn rejects_malformed_token_and_missing_identity_fields() {
    // given
    let now = 2_000_000_000;
    let mut missing_username = params();
    missing_username.username = None;
    let mut missing_object_id = params();
    missing_object_id.extra.remove("entra_object_id");
    let auth_errors = [missing_username, missing_object_id]
        .iter()
        .map(AzureCliAuth::from_params)
        .collect::<Vec<_>>();

    // when
    let malformed = validate_access_token(
        "not-a-jwt",
        "tenant-guid",
        "object-guid",
        "person@example.test",
        now,
    );
    let unsigned = validate_access_token(
        &format!(
            "{}.{}.{}",
            URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#),
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims(now)).unwrap()),
            URL_SAFE_NO_PAD.encode(b"signature")
        ),
        "tenant-guid",
        "object-guid",
        "person@example.test",
        now,
    );

    // then
    assert!(auth_errors.iter().all(Result::is_err));
    assert!(malformed.is_err());
    assert!(unsigned.is_err());
}

/// 인증 모드가 없으면 기존 SQL 인증 설정을 사용할 수 있다.
#[test]
fn keeps_existing_sql_auth_when_auth_mode_is_absent() {
    // given
    let params = ConnectionParams::default();
    let settings = crate::settings::PluginSettings::default();

    // when
    let config = build_config(&params, &settings);

    // then
    assert!(config.is_ok());
}

/// 인증 모드, 필수 신원, TLS, 대상 서버가 맞지 않으면 구성을 거부한다.
#[test]
fn rejects_invalid_authentication_identity_tls_and_target() {
    // given
    let settings = crate::settings::PluginSettings::default();
    let mut missing_tenant = params();
    missing_tenant.extra.remove("tenant_id");
    let mut missing_object = params();
    missing_object.extra.remove("entra_object_id");
    let mut missing_username = params();
    missing_username.username = None;
    let mut bad_source = params();
    bad_source
        .extra
        .insert("auth_source".into(), "other".into());
    let mut missing_source = params();
    missing_source.extra.remove("auth_source");
    let mut bad_mode = params();
    bad_mode
        .extra
        .insert("auth_mode".into(), "device_code".into());
    let mut relative_cli_path = params();
    relative_cli_path
        .extra
        .insert("azure_cli_path".into(), "tools/az".into());
    let mut stray_source = ConnectionParams::default();
    stray_source
        .extra
        .insert("auth_source".into(), "azure_cli".into());
    let mut mixed_password = params();
    mixed_password.password = Some("not-used".into());
    let mut integrated = params();
    integrated.integrated_auth = true;
    let mut downgraded_tls = params();
    downgraded_tls.ssl_mode = Some("require".into());
    let mut wrong_host = params();
    wrong_host.host = Some("sql.contoso.com".into());
    let cases = [
        missing_tenant,
        missing_object,
        missing_username,
        bad_source,
        missing_source,
        bad_mode,
        relative_cli_path,
        stray_source,
        mixed_password,
        integrated,
        downgraded_tls,
        wrong_host,
    ];

    // when
    let results = cases
        .iter()
        .map(|params| build_config(params, &settings))
        .collect::<Vec<_>>();

    // then
    assert!(results.iter().all(Result::is_err));
}

/// verify_identity는 verify-full로 정규화되고 Entra 연결은 인증서 검증을 유지한다.
#[test]
fn accepts_identity_tls_alias_and_rejects_global_certificate_trust() {
    // given
    let mut params = params();
    params.ssl_mode = Some("verify_identity".into());
    let settings = crate::settings::PluginSettings::default();
    let trusted_settings = crate::settings::PluginSettings {
        trust_server_certificate: true,
        ..Default::default()
    };

    // when
    let config = build_config(&params, &settings);
    let trusted_config = build_config(&params, &trusted_settings);

    // then
    let config = format!("{:?}", config.unwrap());
    assert!(config.contains("encryption: Required"));
    assert!(config.contains("trust_cert: false"));
    assert!(trusted_config.is_err());
}

/// 각 물리 연결은 가짜 Azure CLI에서 새 토큰을 받아 설정 복사본에 적용한다.
#[cfg(unix)]
#[tokio::test]
async fn acquires_a_token_for_each_physical_connection_attempt() {
    // given
    let now = chrono::Utc::now().timestamp();
    let jwt = token(&claims(now));
    let cli = FakeCli::new("#!/bin/sh\nexit 1\n");
    fs::write(&cli.executable, token_cli(&jwt, &cli.calls)).unwrap();
    let auth = cli.auth();
    let config = Config::new();

    // when
    let mut tokens = Vec::new();
    for _ in 0..2 {
        let token = connect_with_auth(
            &config,
            Some(&auth),
            Duration::from_secs(2),
            |connection_config| async move {
                Ok::<_, Infallible>(connection_config.to_client_context().access_token.unwrap())
            },
        )
        .await
        .unwrap();
        tokens.push(token);
    }

    // then
    assert_eq!(tokens, vec![jwt.clone(), jwt]);
    assert_eq!(fs::read_to_string(&cli.calls).unwrap().lines().count(), 2);
}

/// 연결 제한 시간은 인증 토큰을 받은 뒤의 TDS 연결에도 적용된다.
#[tokio::test]
async fn applies_connection_deadline_to_connector() {
    // given
    let config = Config::new();

    // when
    let result = connect_with_auth(
        &config,
        None,
        Duration::from_millis(20),
        |_connection_config| async move {
            tokio::time::sleep(Duration::from_secs(1)).await;
            Ok::<_, Infallible>(())
        },
    )
    .await;

    // then
    assert_eq!(result, Err(ConnectError::Timeout));
}

/// CLI 표준 출력과 오류 출력은 인증 오류에 노출되지 않는다.
#[cfg(unix)]
#[tokio::test]
async fn sanitizes_cli_failure_output() {
    // given
    let secret = "synthetic-token-secret";
    let cli = FakeCli::new(&format!(
        "#!/bin/sh\nprintf '%s' '{secret}'\nprintf '%s' '{secret}' >&2\nexit 19\n"
    ));
    let auth = cli.auth();

    // when
    let result = auth
        .acquire_access_token(Instant::now() + Duration::from_secs(2))
        .await;

    // then
    let error = result.unwrap_err();
    assert_eq!(
        error,
        AcquireError::Failed("Azure CLI token acquisition failed")
    );
    assert!(!format!("{error:?}").contains(secret));
}

/// CLI 출력이 제한 시간을 넘기면 자식 프로세스를 종료하고 회수한다.
#[cfg(unix)]
#[tokio::test]
async fn kills_and_reaps_cli_process_on_deadline() {
    // given
    let cli = FakeCli::new("#!/bin/sh\nexec /bin/sleep 30\n");
    let pid_path = cli.directory.join("pid");
    fs::write(
        &cli.executable,
        format!(
            "#!/bin/sh\nprintf '%s' \"$$\" > {}\nexec /bin/sleep 30\n",
            shell_quote(&pid_path)
        ),
    )
    .unwrap();
    let auth = cli.auth();
    let acquisition = tokio::spawn(async move {
        auth.acquire_access_token(Instant::now() + Duration::from_secs(5))
            .await
    });

    // when: wait for the fixture's startup signal before observing cleanup.
    let readiness_deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    let pid = loop {
        match fs::read_to_string(&pid_path) {
            Ok(pid) if !pid.trim().is_empty() => break pid,
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("failed to read fake CLI PID: {error}"),
        }
        assert!(
            tokio::time::Instant::now() < readiness_deadline,
            "fake CLI did not create its PID startup signal within three seconds"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    let result = acquisition.await.unwrap();

    // then
    assert_eq!(result, Err(AcquireError::Timeout));
    let output = StdCommand::new("/bin/kill")
        .arg("-0")
        .arg(pid.trim())
        .output()
        .unwrap();
    assert!(!output.status.success());
}

/// 표준 출력이 허용 크기를 넘으면 토큰 처리를 중단한다.
#[cfg(unix)]
#[tokio::test]
async fn rejects_oversized_cli_output() {
    // given
    let cli = FakeCli::new("#!/bin/sh\nprintf '%1100000s' x\n");
    let auth = cli.auth();

    // when
    let result = auth
        .acquire_access_token(Instant::now() + Duration::from_secs(10))
        .await;

    // then
    assert_eq!(
        result,
        Err(AcquireError::Failed(
            "Azure CLI output exceeded the allowed size"
        ))
    );
}

/// 디버그 표현에 Entra 신원 정보가 나타나지 않는다.
#[test]
fn redacts_identity_from_debug_output() {
    // given
    let mut params = params();
    params.username = Some("private@example.test".into());
    params
        .extra
        .insert("tenant_id".into(), "private-tenant".into());
    params
        .extra
        .insert("entra_object_id".into(), "private-object".into());
    let auth = AzureCliAuth::from_params(&params).unwrap().unwrap();

    // when
    let debug = format!("{auth:?}");

    // then
    assert!(!debug.contains("private@example.test"));
    assert!(!debug.contains("private-tenant"));
    assert!(!debug.contains("private-object"));
}
