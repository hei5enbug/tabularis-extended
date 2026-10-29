#[cfg(unix)]
use super::fake_cli::{claims_with, unix_now, FakeCli};
use super::fake_cli::{token, valid_token, HOST, OBJECT, TENANT, USER};
use super::*;
use serde_json::json;
use std::fs;

fn valid_auth() -> AzureCliAuth {
    AzureCliAuth {
        tenant_id: TENANT.into(),
        entra_object_id: OBJECT.into(),
        username: USER.into(),
        azure_cli_path: DEFAULT_AZURE_CLI_PATH.into(),
    }
}

fn valid_claims() -> Value {
    json!({
        "tid": TENANT,
        "oid": OBJECT,
        "upn": "USER@example.com",
        "aud": "https://database.windows.net/",
        "exp": 10_000
    })
}

fn valid_params() -> ConnectionParams {
    let mut params = ConnectionParams {
        host: Some(HOST.into()),
        username: Some(USER.into()),
        ssl_mode: Some("verify-full".into()),
        database: crate::models::DatabaseSelection::Single("fixture".into()),
        ..Default::default()
    };
    params.extra.insert("auth_mode".into(), "entra_user".into());
    params
        .extra
        .insert("auth_source".into(), "azure_cli".into());
    params.extra.insert("tenant_id".into(), TENANT.into());
    params.extra.insert("entra_object_id".into(), OBJECT.into());
    params
}

fn parse(params: &ConnectionParams) -> Result<Option<AzureCliAuth>, String> {
    AzureCliAuth::from_connection_params(params, &PluginSettings::default())
}

#[test]
fn valid_identity_and_resource_claims_are_accepted() {
    // given
    let token = token(valid_claims());

    // when
    let result = validate_access_token(&token, &valid_auth(), 1_000);

    // then
    assert!(result.is_ok());
}

#[test]
fn mismatched_object_id_is_rejected() {
    // given
    let mut claims = valid_claims();
    claims["oid"] = json!("00000000-0000-0000-0000-0000000000ff");

    // when
    let result = validate_access_token(&token(claims), &valid_auth(), 1_000);

    // then
    assert_eq!(result, Err(DIFFERENT_IDENTITY));
}

#[test]
fn mismatched_tenant_id_is_rejected() {
    // given
    let mut claims = valid_claims();
    claims["tid"] = json!("00000000-0000-0000-0000-0000000000ff");

    // when
    let result = validate_access_token(&token(claims), &valid_auth(), 1_000);

    // then
    assert_eq!(result, Err(DIFFERENT_IDENTITY));
}

#[test]
fn every_present_username_claim_must_match_without_case_sensitivity() {
    // given
    let auth = valid_auth();

    // when
    let results = ["upn", "unique_name", "preferred_username"].map(|key| {
        let mut matching = valid_claims();
        matching[key] = json!("User@EXAMPLE.com");
        let mut other = valid_claims();
        other[key] = json!("other@example.com");
        (
            validate_access_token(&token(matching), &auth, 1_000),
            validate_access_token(&token(other), &auth, 1_000),
        )
    });

    // then
    for (matching, other) in results {
        assert!(matching.is_ok());
        assert_eq!(other, Err(DIFFERENT_IDENTITY));
    }
}

#[test]
fn non_string_username_claim_is_rejected() {
    // given
    let mut claims = valid_claims();
    claims["unique_name"] = json!(7);

    // when
    let result = validate_access_token(&token(claims), &valid_auth(), 1_000);

    // then
    assert_eq!(result, Err(DIFFERENT_IDENTITY));
}

#[test]
fn wrong_audience_is_rejected() {
    // given
    let mut claims = valid_claims();
    claims["aud"] = json!("https://management.azure.com/");

    // when
    let result = validate_access_token(&token(claims), &valid_auth(), 1_000);

    // then
    assert!(result.is_err());
}

#[test]
fn expired_or_near_expiry_token_is_rejected() {
    // given
    let mut near = valid_claims();
    near["exp"] = json!(1_120);
    let mut ok = valid_claims();
    ok["exp"] = json!(1_121);

    // when
    let near = validate_access_token(&token(near), &valid_auth(), 1_000);
    let ok = validate_access_token(&token(ok), &valid_auth(), 1_000);

    // then
    assert!(near.is_err());
    assert!(ok.is_ok());
}

#[test]
fn malformed_tokens_are_rejected() {
    // given
    let tokens = ["", "a.b", "a.b.c.d", "a..c", "a.!!!.c"];

    // when
    let results = tokens.map(|token| validate_access_token(token, &valid_auth(), 1_000));

    // then
    assert!(results.iter().all(Result::is_err));
}

#[test]
fn absent_auth_mode_preserves_default_authentication() {
    // given
    let params = ConnectionParams::default();

    // when
    let result = parse(&params);

    // then
    assert_eq!(result, Ok(None));
}

#[test]
fn valid_entra_user_parameters_produce_an_identity() {
    // given
    let mut params = valid_params();
    params.username = Some(format!("  {USER} "));

    // when
    let result = parse(&params).unwrap().unwrap();

    // then
    assert_eq!(result.username, USER);
    assert_eq!(result.tenant_id, TENANT);
    assert_eq!(result.entra_object_id, OBJECT);
    assert_eq!(result.azure_cli_path, "az");
}

#[test]
fn auth_source_without_auth_mode_is_rejected() {
    // given
    let mut params = ConnectionParams::default();
    params
        .extra
        .insert("auth_source".into(), "azure_cli".into());

    // when
    let result = parse(&params);

    // then
    assert!(result.is_err());
}

#[test]
fn entra_fields_on_other_auth_modes_are_rejected() {
    // given
    let mut sql = valid_params();
    sql.extra.insert("auth_mode".into(), "sql_password".into());
    let mut integrated = valid_params();
    integrated.username = None;
    integrated
        .extra
        .insert("auth_mode".into(), "integrated".into());

    // when
    let results = [parse(&sql), parse(&integrated)];

    // then
    assert!(results.iter().all(Result::is_err));
}

#[test]
fn unsupported_auth_modes_are_rejected() {
    // given
    let modes = ["password", "entra_service_principal", "entra_device_code"];

    // when
    let results = modes.map(|mode| {
        let mut params = valid_params();
        params.extra.insert("auth_mode".into(), mode.into());
        parse(&params)
    });

    // then
    assert!(results.iter().all(Result::is_err));
}

#[test]
fn unsupported_auth_source_is_rejected() {
    // given
    let mut params = valid_params();
    params
        .extra
        .insert("auth_source".into(), "device_code".into());
    let mut missing = valid_params();
    missing.extra.remove("auth_source");

    // when
    let results = [parse(&params), parse(&missing)];

    // then
    assert!(results.iter().all(Result::is_err));
}

#[test]
fn missing_required_entra_identity_is_rejected() {
    // given
    let mut tenant = valid_params();
    tenant.extra.remove("tenant_id");
    let mut object = valid_params();
    object.extra.remove("entra_object_id");
    let mut username = valid_params();
    username.username = None;
    let mut blank = valid_params();
    blank.username = Some("  ".into());

    // when
    let results = [tenant, object, username, blank].map(|params| parse(&params));

    // then
    assert!(results.iter().all(Result::is_err));
}

#[test]
fn entra_auth_rejects_password() {
    // given
    let mut params = valid_params();
    params.password = Some("password".into());

    // when
    let result = parse(&params);

    // then
    assert!(result.is_err());
}

#[test]
fn entra_auth_rejects_integrated_auth() {
    // given
    let mut params = valid_params();
    params.integrated_auth = true;

    // when
    let result = parse(&params);

    // then
    assert!(result.is_err());
}

#[test]
fn entra_auth_rejects_tls_downgrade() {
    // given
    let mut params = valid_params();
    params.ssl_mode = Some("require".into());

    // when
    let result = parse(&params);

    // then
    assert!(result.is_err());
}

#[test]
fn entra_auth_normalizes_verify_identity_to_verify_full() {
    // given
    let mut params = valid_params();
    params.ssl_mode = Some("verify_identity".into());

    // when
    let result = parse(&params);

    // then
    assert!(result.unwrap().is_some());
}

#[test]
fn entra_auth_requires_trusted_certificate_to_be_disabled() {
    // given
    let settings = PluginSettings {
        trust_server_certificate: true,
        ..PluginSettings::default()
    };

    // when
    let result = AzureCliAuth::from_connection_params(&valid_params(), &settings);

    // then
    assert!(result.is_err());
}

#[test]
fn entra_auth_requires_a_public_azure_sql_host() {
    // given
    let hosts = [
        "sql.example.com",
        "database.windows.net",
        ".database.windows.net",
        "synthetic.database.windows.net.example.com",
        "synthetic.database.chinacloudapi.cn",
    ];

    // when
    let results = hosts.map(|host| {
        let mut params = valid_params();
        params.host = Some(host.into());
        parse(&params)
    });

    // then
    assert!(results.iter().all(Result::is_err), "{results:?}");
}

#[test]
fn entra_auth_rejects_unsafe_tenant_values() {
    // given
    let tenants = ["--subscription", "tenant id", "tenant;id", "t/../x"];

    // when
    let results = tenants.map(|tenant| {
        let mut params = valid_params();
        params.extra.insert("tenant_id".into(), tenant.into());
        parse(&params)
    });

    // then
    assert!(results.iter().all(Result::is_err));
}

#[test]
fn azure_cli_path_must_be_an_absolute_path() {
    // given
    let absolute = std::env::temp_dir()
        .join("az")
        .to_string_lossy()
        .into_owned();
    let mut accepted = valid_params();
    accepted
        .extra
        .insert("azure_cli_path".into(), absolute.clone());
    let rejected = ["az", "./az", "", "  "].map(|value| {
        let mut params = valid_params();
        params
            .extra
            .insert("azure_cli_path".into(), value.to_owned());
        parse(&params)
    });

    // when
    let accepted = parse(&accepted).unwrap().unwrap();

    // then
    assert_eq!(accepted.azure_cli_path, absolute);
    assert!(rejected.iter().all(Result::is_err));
}

#[test]
fn access_token_and_identity_debug_output_contain_no_token() {
    // given
    let secret = valid_token();
    let access = AccessToken(secret.clone());
    let auth = valid_auth();

    // when
    let debug = format!("{access:?} {auth:?}");

    // then
    assert!(!debug.contains(&secret));
    assert!(!debug.contains(secret.split('.').nth(1).unwrap()));
    assert_eq!(access.expose(), secret);
}

struct ResolverFixture {
    directory: PathBuf,
}

impl ResolverFixture {
    fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};

        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "sqlserver plus azure cli resolver {} {}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&directory).unwrap();
        Self { directory }
    }

    fn file(&self, relative_path: &str) -> PathBuf {
        let path = self.directory.join(relative_path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"fixture").unwrap();
        path
    }
}

impl Drop for ResolverFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

/// 기본 검색은 절대 PATH 디렉터리에서 az.exe를 az.cmd보다 먼저 선택한다.
#[test]
fn 기본_검색은_절대_path에서_az_exe를_우선한다() {
    // given
    let fixture = ResolverFixture::new();
    let executable = fixture.file("tools/az.exe");
    fixture.file("tools/az.cmd");
    fixture.file("python.exe");
    let path_dirs = vec![fixture.directory.join("tools")];

    // when
    let result = resolve_windows_cli(None, &path_dirs);

    // then
    let result = result.unwrap();
    assert_eq!(result.program, executable);
    assert!(result.prefix_args.is_empty());
}

/// 기본 검색은 az.cmd를 인접한 Python 런타임과 Azure CLI 모듈로 실행한다.
#[test]
fn 기본_검색은_az_cmd를_상위_python으로_실행한다() {
    // given
    let fixture = ResolverFixture::new();
    fixture.file("Azure CLI/wbin/az.cmd");
    let python = fixture.file("Azure CLI/python.exe");
    let path_dirs = vec![fixture.directory.join("Azure CLI/wbin")];

    // when
    let result = resolve_windows_cli(None, &path_dirs);

    // then
    let result = result.unwrap();
    assert_eq!(result.program, python);
    assert_eq!(result.prefix_args, ["-IBm", "azure.cli"]);
}

/// az.cmd 옆 경로에 Python 런타임이 없으면 기본 검색을 거부한다.
#[test]
fn python이_없으면_az_cmd를_거부한다() {
    // given
    let fixture = ResolverFixture::new();
    fixture.file("Azure CLI/wbin/az.cmd");
    let path_dirs = vec![fixture.directory.join("Azure CLI/wbin")];

    // when
    let result = resolve_windows_cli(None, &path_dirs);

    // then
    assert!(matches!(
        result,
        Err("Azure CLI Python runtime was not found")
    ));
}

/// 기본 검색은 상대 PATH 디렉터리에서 Azure CLI를 찾지 않는다.
#[test]
fn 상대_path_디렉터리는_기본_검색에서_제외한다() {
    // given
    let fixture = ResolverFixture::new();
    fixture.file("relative/az.exe");
    let path_dirs = vec![
        PathBuf::from("relative"),
        fixture.directory.join("empty absolute directory"),
    ];
    fs::create_dir_all(&path_dirs[1]).unwrap();

    // when
    let result = resolve_windows_cli(None, &path_dirs);

    // then
    assert!(matches!(
        result,
        Err("Azure CLI was not found in absolute PATH directories")
    ));
}

/// 대소문자가 다른 az.cmd와 공백이 있는 경로를 Python 런타임으로 해석한다.
#[test]
fn 대소문자와_공백이_있는_공식_az_cmd를_해석한다() {
    // given
    let fixture = ResolverFixture::new();
    let command = fixture.file("Program Files/Azure CLI/WBIN/AZ.CMD");
    let python = fixture.file("Program Files/Azure CLI/python.exe");

    // when
    let result = resolve_windows_cli(Some(&command), &[]);

    // then
    let result = result.unwrap();
    assert_eq!(result.program, python);
    assert_eq!(result.prefix_args, ["-IBm", "azure.cli"]);
}

/// 명시한 절대 az.exe 경로는 그대로 실행 파일로 사용한다.
#[test]
fn 명시한_절대_az_exe를_그대로_사용한다() {
    // given
    let fixture = ResolverFixture::new();
    let executable = fixture.file("Program Files/Azure CLI/az.exe");

    // when
    let result = resolve_windows_cli(Some(&executable), &[]);

    // then
    let result = result.unwrap();
    assert_eq!(result.program, executable);
    assert!(result.prefix_args.is_empty());
}

/// 명시한 확장자 없는 경로에서 az.exe가 있으면 az.cmd보다 먼저 선택한다.
#[test]
fn 확장자_없는_명시_경로에서_az_exe를_우선한다() {
    // given
    let fixture = ResolverFixture::new();
    let executable = fixture.file("first/az.exe");
    fixture.file("first/az.cmd");
    let extensionless_executable = fixture.directory.join("first/az");

    // when
    let executable_result = resolve_windows_cli(Some(&extensionless_executable), &[]);

    // then
    assert_eq!(executable_result.unwrap().program, executable);
}

/// 명시한 확장자 없는 경로는 az.exe가 없으면 az.cmd를 Python으로 실행한다.
#[test]
fn 확장자_없는_명시_경로에서_az_cmd를_python으로_해석한다() {
    // given
    let fixture = ResolverFixture::new();
    fixture.file("second/az.cmd");
    let python = fixture.file("python.exe");
    let extensionless_command = fixture.directory.join("second/az");

    // when
    let command_result = resolve_windows_cli(Some(&extensionless_command), &[]);

    // then
    assert_eq!(command_result.unwrap().program, python);
}

/// 명시한 az.exe 파일이 없으면 해당 실행 파일 경로를 거부한다.
#[test]
fn 존재하지_않는_명시_az_exe를_거부한다() {
    // given
    let fixture = ResolverFixture::new();
    let executable = fixture.directory.join("tools/az.exe");

    // when
    let result = resolve_windows_cli(Some(&executable), &[]);

    // then
    assert!(matches!(result, Err("Azure CLI executable was not found")));
}

/// 명시한 배치 파일은 Azure CLI 실행 경로로 허용하지 않는다.
#[test]
fn 명시한_배치_파일을_거부한다() {
    // given
    let fixture = ResolverFixture::new();
    let batch = fixture.file("tools/az.bat");

    // when
    let result = resolve_windows_cli(Some(&batch), &[]);

    // then
    assert!(matches!(
        result,
        Err("Azure CLI scripts and batch files are not supported")
    ));
}

/// 명시한 PowerShell 스크립트는 Azure CLI 실행 경로로 허용하지 않는다.
#[test]
fn 명시한_powershell_스크립트를_거부한다() {
    // given
    let fixture = ResolverFixture::new();
    let script = fixture.file("tools/az.ps1");

    // when
    let result = resolve_windows_cli(Some(&script), &[]);

    // then
    assert!(matches!(
        result,
        Err("Azure CLI scripts and batch files are not supported")
    ));
}

#[cfg(windows)]
/// 환경 설정이 켜진 Windows 검사는 Azure CLI 버전만 확인한다.
#[tokio::test]
async fn 옵트인된_windows_검사는_azure_cli_버전만_확인한다() {
    // given
    if std::env::var("TABULARIS_TEST_WINDOWS_AZURE_CLI").as_deref() != Ok("1") {
        return;
    }
    let cli = resolve_cli_command(None).expect("Azure CLI should be available on PATH");
    let mut child = build_cli_command(&cli)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("Azure CLI version command should start");

    // when
    let result = match tokio::time::timeout(std::time::Duration::from_secs(20), child.wait()).await
    {
        Ok(Ok(status)) => Some(status.success()),
        Ok(Err(_)) => Some(false),
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            None
        }
    };

    // then
    assert_eq!(result, Some(true));
}

#[tokio::test]
async fn missing_cli_executable_is_a_sanitized_failure() {
    // given
    let auth = AzureCliAuth {
        azure_cli_path: std::env::temp_dir()
            .join("sqlserver-plus-missing-az")
            .to_string_lossy()
            .into_owned(),
        ..valid_auth()
    };

    // when
    let actual = auth.acquire_access_token().await;

    // then
    assert_eq!(actual.unwrap_err(), TOKEN_REQUEST_FAILED);
}

#[cfg(unix)]
mod cli_process {
    use super::*;
    use std::time::Duration;

    fn auth_for(cli: &FakeCli) -> AzureCliAuth {
        AzureCliAuth {
            azure_cli_path: cli.path(),
            ..valid_auth()
        }
    }

    #[tokio::test]
    async fn token_is_requested_for_the_tenant_and_sql_resource_without_a_shell() {
        // given
        let secret = valid_token();
        let cli = FakeCli::responding_with(&secret);

        // when
        let actual = auth_for(&cli).acquire_access_token().await.unwrap();

        // then
        assert_eq!(actual.expose(), secret);
        assert_eq!(
            cli.args(),
            [
                "account",
                "get-access-token",
                "--resource",
                "https://database.windows.net/",
                "--tenant",
                TENANT,
                "--output",
                "json"
            ]
        );
        assert!(cli.args().iter().all(|arg| !arg.contains(&secret)));
    }

    #[tokio::test]
    async fn cli_failure_output_never_reaches_the_error() {
        // given
        let cli = FakeCli::script(
            "echo 'stdout-canary eyJhbGciOiJub25lIn0.payload.sig'; echo 'stderr-canary' >&2; exit 3",
        );

        // when
        let actual = auth_for(&cli).acquire_access_token().await;

        // then
        assert_eq!(actual.unwrap_err(), TOKEN_REQUEST_FAILED);
    }

    #[tokio::test]
    async fn invalid_cli_response_is_sanitized() {
        // given
        let cli = FakeCli::script("echo '{\"accessToken\": \"stdout-canary\"'");

        // when
        let actual = auth_for(&cli).acquire_access_token().await;

        // then
        assert_eq!(actual.unwrap_err(), INVALID_RESPONSE);
    }

    #[tokio::test]
    async fn a_token_for_another_principal_is_rejected_without_echoing_it() {
        // given
        let other = token(claims_with(
            TENANT,
            "00000000-0000-0000-0000-0000000000ff",
            USER,
            unix_now() + 3_600,
        ));
        let cli = FakeCli::responding_with(&other);

        // when
        let actual = auth_for(&cli).acquire_access_token().await;

        // then
        assert_eq!(actual.unwrap_err(), DIFFERENT_IDENTITY);
    }

    #[tokio::test]
    async fn a_near_expiry_token_from_the_cli_is_rejected() {
        // given
        let near = token(claims_with(TENANT, OBJECT, USER, unix_now() + 60));
        let cli = FakeCli::responding_with(&near);

        // when
        let actual = auth_for(&cli).acquire_access_token().await;

        // then
        assert!(actual.is_err());
    }

    #[tokio::test]
    async fn dropping_the_request_kills_the_cli_process() {
        // given
        let cli = FakeCli::script("echo $$ > \"$d/pid.txt\"; exec sleep 60");
        let auth = auth_for(&cli);

        // when
        let outcome =
            tokio::time::timeout(Duration::from_millis(500), auth.acquire_access_token()).await;
        let pid = std::fs::read_to_string(cli.file("pid.txt"))
            .unwrap()
            .trim()
            .to_owned();
        let mut stopped = false;
        for _ in 0..100 {
            let alive = std::process::Command::new("kill")
                .args(["-0", &pid])
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success();
            if !alive {
                stopped = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        // then
        assert!(outcome.is_err());
        assert!(stopped, "the Azure CLI child outlived its request");
    }
}
