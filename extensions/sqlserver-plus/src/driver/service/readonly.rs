use super::{
    exact::{self, Parameter},
    ServiceError,
};
use mssql_tds::{connection::tds_client::TdsClient, core::CancelHandle};
use serde_json::{json, Value};

const DATABASE_PROBE: &str = r#"
SELECT 'principal', CAST(CASE WHEN USER_NAME()='dbo' OR SUSER_SNAME()<>ORIGINAL_LOGIN() THEN 1 ELSE 0 END AS nvarchar(128));
SELECT 'role', p.name FROM sys.user_token t JOIN sys.database_principals p ON t.principal_id=p.principal_id WHERE p.type='R' AND p.name NOT IN ('public','db_datareader','db_denydatawriter');
SELECT 'permission', permission_name FROM sys.fn_my_permissions(NULL,'DATABASE');
SELECT 'permission', p.permission_name FROM sys.schemas s CROSS APPLY sys.fn_my_permissions(QUOTENAME(s.name),'SCHEMA') p WHERE s.name NOT IN ('sys','INFORMATION_SCHEMA') AND s.schema_id<16384;
SELECT 'permission', p.permission_name FROM sys.objects o JOIN sys.schemas s ON o.schema_id=s.schema_id CROSS APPLY sys.fn_my_permissions(QUOTENAME(s.name)+'.'+QUOTENAME(o.name),'OBJECT') p WHERE o.is_ms_shipped=0;
SELECT 'grant', permission_name FROM sys.database_permissions WHERE grantee_principal_id IN (SELECT principal_id FROM sys.user_token) AND state IN ('G','W') AND (class<>1 OR OBJECTPROPERTY(major_id,'IsMSShipped')=0);
SELECT 'ownership', name FROM sys.database_principals WHERE owning_principal_id IN (SELECT principal_id FROM sys.user_token) AND principal_id<>USER_ID();
SELECT 'ownership', name FROM sys.schemas WHERE principal_id IN (SELECT principal_id FROM sys.user_token) AND schema_id<16384;
SELECT 'ownership', name FROM sys.objects WHERE principal_id IN (SELECT principal_id FROM sys.user_token) AND is_ms_shipped=0;
SELECT 'external', name FROM sys.external_data_sources;
"#;
fn allowed(permission: &str, server: bool) -> bool {
    permission.starts_with("VIEW ")
        || if server {
            permission == "CONNECT SQL"
        } else {
            matches!(permission, "CONNECT" | "SELECT" | "SHOWPLAN" | "UNMASK")
        }
}
fn sets(value: &Value) -> impl Iterator<Item = &Value> {
    std::iter::once(value).chain(value["additional_results"].as_array().into_iter().flatten())
}
fn verify_database(value: &Value) -> Result<(), ServiceError> {
    for set in sets(value) {
        for row in set["rows"].as_array().into_iter().flatten() {
            let tag = row[0].as_str().unwrap_or("");
            let value = row[1].as_str().unwrap_or("");
            let reject = match tag {
                "principal" => value != "0",
                "permission" | "grant" => !allowed(value, false),
                _ => true,
            };
            if reject {
                return Err(ServiceError::new(
                    "WRITE_NOT_ALLOWED",
                    "The physical database principal cannot be proven read-only",
                ));
            }
        }
    }
    Ok(())
}
fn verify_server(value: &Value) -> Result<(), ServiceError> {
    let mut permissions = 0;
    let mut database_visibility = false;
    for set in sets(value) {
        for row in set["rows"].as_array().into_iter().flatten() {
            let permitted = match row[0].as_str() {
                Some("visibility") => {
                    if row[1].as_i64() != Some(1) {
                        return Err(ServiceError::new(
                            "CAPABILITY_UNAVAILABLE",
                            "The accessible database catalog cannot be verified",
                        ));
                    }
                    database_visibility = true;
                    true
                }
                Some("permission") => {
                    permissions += 1;
                    row[1]
                        .as_str()
                        .is_some_and(|permission| allowed(permission, true))
                }
                Some("securable") => match row[1].as_str() {
                    Some("SERVER") => row[2]
                        .as_str()
                        .is_some_and(|permission| allowed(permission, true)),
                    Some("ENDPOINT") => {
                        row[2].as_str() == Some("CONNECT") && row[3].as_i64() == Some(2)
                    }
                    _ => false,
                },
                _ => false,
            };
            if !permitted {
                return Err(ServiceError::new(
                    "WRITE_NOT_ALLOWED",
                    "The server principal or configuration cannot be proven read-only",
                ));
            }
        }
    }
    if permissions == 0 || !database_visibility {
        return Err(ServiceError::new(
            "CAPABILITY_UNAVAILABLE",
            "The server permissions are unavailable",
        ));
    }
    Ok(())
}
pub async fn probe(
    client: &mut TdsClient,
    cancel: Option<&CancelHandle>,
) -> Result<(), ServiceError> {
    let unavailable = |_| {
        ServiceError::new(
            "CAPABILITY_UNAVAILABLE",
            "Read-only database permissions could not be verified",
        )
    };
    let header = exact::collect(
        client,
        "SELECT CAST(SERVERPROPERTY('EngineEdition') AS int), DB_NAME()",
        Vec::new(),
        cancel,
        10_000,
        super::types::FRAME_LIMIT,
        false,
    )
    .await
    .map_err(unavailable)?;
    let edition = header["rows"][0][0].as_i64().ok_or_else(|| {
        ServiceError::new(
            "CAPABILITY_UNAVAILABLE",
            "The SQL engine edition is unavailable",
        )
    })?;
    if edition == 5 {
        let data = exact::collect(
            client,
            DATABASE_PROBE,
            Vec::new(),
            cancel,
            10_000,
            super::types::FRAME_LIMIT,
            false,
        )
        .await
        .map_err(unavailable)?;
        return verify_database(&data);
    }
    let server = exact::collect(client, "SELECT 'visibility', HAS_PERMS_BY_NAME(NULL,NULL,'VIEW ANY DATABASE'); SELECT 'permission', permission_name FROM sys.fn_my_permissions(NULL,'SERVER'); SELECT 'prohibited', name FROM sys.servers WHERE server_id>0; SELECT 'prohibited', name FROM sys.configurations WHERE name IN ('cross db ownership chaining','Ad Hoc Distributed Queries','clr enabled','xp_cmdshell','external scripts enabled','Ole Automation Procedures') AND value_in_use<>0; SELECT 'prohibited', name FROM sys.server_principals WHERE owning_principal_id IN (SELECT principal_id FROM sys.login_token); SELECT 'securable', p.class_desc, p.permission_name, COALESCE(e.type,0) FROM sys.server_permissions p LEFT JOIN sys.endpoints e ON p.class=105 AND p.major_id=e.endpoint_id WHERE p.grantee_principal_id IN (SELECT principal_id FROM sys.login_token) AND p.state IN ('G','W')", Vec::new(), cancel, 10_000, super::types::FRAME_LIMIT, false).await.map_err(unavailable)?;
    verify_server(&server)?;
    let databases = exact::collect(client, "SELECT name, is_db_chaining_on, is_trustworthy_on FROM sys.databases WHERE database_id>4 AND HAS_DBACCESS(name)=1", Vec::new(), cancel, 10_000, super::types::FRAME_LIMIT, false).await.map_err(unavailable)?;
    let current = header["rows"][0][1].as_str().ok_or_else(|| {
        ServiceError::new(
            "CAPABILITY_UNAVAILABLE",
            "The current database is unavailable",
        )
    })?;
    let mut names = Vec::new();
    for row in databases["rows"].as_array().into_iter().flatten() {
        if row[1].as_bool() != Some(false) || row[2].as_bool() != Some(false) {
            return Err(ServiceError::new(
                "WRITE_NOT_ALLOWED",
                "Cross-database ownership chaining or trustworthy databases are unsupported",
            ));
        }
        names.push(
            row[0]
                .as_str()
                .ok_or_else(|| ServiceError::argument("Invalid database catalog"))?
                .to_owned(),
        );
    }
    if !names.iter().any(|name| name == current) {
        names.push(current.to_owned());
    }
    for name in names {
        let text = format!(
            "EXEC {}.sys.sp_executesql @stmt=@probe",
            crate::driver::helpers::bracket_quote(&name)
        );
        let parameters = exact::bind(&[Parameter {
            name: "@probe".into(),
            value: json!(DATABASE_PROBE),
        }])?;
        let data = exact::collect(
            client,
            &text,
            parameters,
            cancel,
            10_000,
            super::types::FRAME_LIMIT,
            false,
        )
        .await
        .map_err(unavailable)?;
        verify_database(&data)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::verify_server;
    use serde_json::json;

    #[test]
    fn database_목록의_가시성이_없거나_불명확하면_읽기_전용_실행을_거부한다() {
        // given
        let rows = [
            json!({"rows":[["visibility",0],["permission","CONNECT SQL"]]}),
            json!({"rows":[["visibility",null],["permission","CONNECT SQL"]]}),
            json!({"rows":[["permission","CONNECT SQL"]]}),
        ];
        // when
        let actual = rows.map(|rows| verify_server(&rows));
        // then
        assert!(actual
            .iter()
            .all(|result| result.as_ref().unwrap_err().code == "CAPABILITY_UNAVAILABLE"));
    }

    macro_rules! rejects_configuration {
        ($name:ident, $configuration:literal) => {
            #[test]
            fn $name() {
                // given
                let rows = json!({"rows":[["visibility",1],["permission","CONNECT SQL"],["prohibited",$configuration]]});
                // when
                let actual = verify_server(&rows);
                // then
                assert_eq!(actual.unwrap_err().code, "WRITE_NOT_ALLOWED");
            }
        };
    }

    rejects_configuration!(
        cross_db_ownership_chaining은_읽기_전용_실행을_거부한다,
        "cross db ownership chaining"
    );
    rejects_configuration!(
        ad_hoc_distributed_queries는_읽기_전용_실행을_거부한다,
        "Ad Hoc Distributed Queries"
    );
    rejects_configuration!(clr_enabled는_읽기_전용_실행을_거부한다, "clr enabled");
    rejects_configuration!(xp_cmdshell은_읽기_전용_실행을_거부한다, "xp_cmdshell");
    rejects_configuration!(
        external_scripts_enabled는_읽기_전용_실행을_거부한다,
        "external scripts enabled"
    );
    rejects_configuration!(
        ole_automation_procedures는_읽기_전용_실행을_거부한다,
        "Ole Automation Procedures"
    );
}
