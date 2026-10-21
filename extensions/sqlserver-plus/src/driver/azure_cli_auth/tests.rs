use super::fake_cli::{
    claims_with, token, unix_now, valid_token, FakeCli, HOST, OBJECT, TENANT, USER,
};
use super::*;
use serde_json::json;

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
