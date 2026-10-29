use super::*;

fn params(driver: &str, database: DatabaseSelection) -> ConnectionParams {
    ConnectionParams {
        driver: driver.to_string(),
        database,
        ..Default::default()
    }
}

#[test]
fn network_drivers_accept_a_database_outside_the_configured_selection() {
    for driver in ["postgres", "mysql", "custom-network-plugin"] {
        for selection in [
            DatabaseSelection::default(),
            DatabaseSelection::Single("sandbox".to_string()),
            DatabaseSelection::Multiple(vec!["sandbox".to_string(), "analytics".to_string()]),
        ] {
            let mut connection = params(driver, selection.clone());
            reject_unsupported_database_override(
                Some("production"),
                &DriverCapabilities::default(),
            )
            .expect("a network driver must not be bound to the configured selection");
            assert_eq!(connection.database, selection);

            let listed = vec!["sandbox".to_string(), "production".to_string()];
            select_listed_database(&mut connection, &listed, "production").unwrap();
            assert_eq!(
                connection.database,
                DatabaseSelection::Single("production".to_string())
            );
        }
    }
}

#[test]
fn a_database_the_driver_does_not_list_is_rejected_without_changing_params() {
    let saved = DatabaseSelection::Multiple(vec!["sandbox".to_string(), "analytics".to_string()]);
    let listed = vec!["sandbox".to_string(), "analytics".to_string()];
    for requested in [
        "production",
        "Sandbox",
        "sandbox ",
        " sandbox",
        "../private",
        "",
    ] {
        let mut connection = params("postgres", saved.clone());
        let error = select_listed_database(&mut connection, &listed, requested)
            .expect_err("an unlisted database must be rejected");
        assert_eq!(error.code, -32602);
        assert_eq!(
            connection.database, saved,
            "rejection must not mutate params"
        );
    }
}

#[test]
fn local_and_implicit_database_capabilities_reject_overrides() {
    // Plugin IDs need not identify the storage model; capabilities do.
    for capabilities in [
        DriverCapabilities {
            file_based: true,
            ..Default::default()
        },
        DriverCapabilities {
            folder_based: true,
            ..Default::default()
        },
        DriverCapabilities {
            single_database: true,
            ..Default::default()
        },
    ] {
        for requested in [
            "/tmp/sandbox.sqlite",
            "/tmp/private.sqlite",
            "~/private.sqlite",
            "../private.sqlite",
            "file:/tmp/private.sqlite?mode=ro",
            ":memory:",
        ] {
            let error = reject_unsupported_database_override(Some(requested), &capabilities)
                .expect_err("MCP must not replace a local path or implicit database");
            assert_eq!(error.code, -32602);
        }
    }
}

#[test]
fn omitted_database_is_accepted_for_every_connection_type() {
    for capabilities in [
        DriverCapabilities::default(),
        DriverCapabilities {
            file_based: true,
            ..Default::default()
        },
        DriverCapabilities {
            folder_based: true,
            ..Default::default()
        },
        DriverCapabilities {
            single_database: true,
            ..Default::default()
        },
    ] {
        reject_unsupported_database_override(None, &capabilities).unwrap();
    }
}
