use super::*;

#[test]
fn lists_codex_as_command_client() {
    let clients = get_all_clients();
    let codex = clients
        .iter()
        .find(|client| client.id == "codex")
        .expect("Codex should be listed as an MCP client");

    assert_eq!(codex.name, "Codex");
    assert_eq!(codex.client_type, "command");
    assert!(
        codex
            .config_path
            .as_ref()
            .is_some_and(|path| path.ends_with(".codex/config.toml")),
        "Codex config path should point to ~/.codex/config.toml"
    );
}

#[test]
fn builds_codex_manual_command() {
    assert_eq!(
        build_manual_command(
            "codex",
            "/Applications/Tabularis Extended.app/tabularis-extended"
        )
        .as_deref(),
        Some("codex mcp add tabularis-extended -- /Applications/Tabularis Extended.app/tabularis-extended --mcp")
    );
}

#[test]
fn builds_claude_code_manual_command() {
    assert_eq!(
        build_manual_command(
            "claude_code",
            "/Applications/Tabularis Extended.app/tabularis-extended"
        )
        .as_deref(),
        Some(
            "claude mcp add --scope user tabularis-extended /Applications/Tabularis Extended.app/tabularis-extended -- --mcp"
        )
    );
}

#[test]
fn detects_codex_toml_config() {
    let temp_dir = tempfile::tempdir().unwrap();
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        r#"
[mcp_servers.tabularis-extended]
command = "/Applications/Tabularis Extended.app/tabularis-extended"
args = ["--mcp"]
"#,
    )
    .unwrap();

    assert!(is_command_client_installed(&config_path));
}

#[test]
fn rejects_unknown_host_client_without_writing_configuration() {
    let error = install_config_blocking("unknown-client")
        .expect_err("unknown MCP clients must be rejected");

    assert_eq!(error, "Unknown client: unknown-client");
}

#[test]
fn claude_desktop_config_is_directly_under_the_os_claude_directory() {
    let clients = get_all_clients();
    let claude = clients.iter().find(|client| client.id == "claude").unwrap();
    assert_eq!(
        claude.config_path,
        Some(
            BaseDirs::new()
                .unwrap()
                .config_dir()
                .join("Claude")
                .join("claude_desktop_config.json")
        )
    );
}

#[test]
fn official_entries_do_not_count_as_the_fork_being_installed() {
    let temp_dir = tempfile::tempdir().unwrap();
    let toml = temp_dir.path().join("config.toml");
    fs::write(&toml, "[mcp_servers.tabularis]\ncommand = \"/official\"\n").unwrap();
    let json = temp_dir.path().join("claude.json");
    fs::write(
        &json,
        r#"{"mcpServers":{"tabularis":{"command":"/official","args":["--mcp"]}}}"#,
    )
    .unwrap();

    assert!(!is_command_client_installed(&toml));
    assert!(!is_command_client_installed(&json));
    assert!(!is_tabularis_in_mcp_servers(&json));
}

#[test]
fn install_keeps_an_existing_official_entry_and_adds_the_fork_key() {
    let temp_dir = tempfile::tempdir().unwrap();
    let path = temp_dir.path().join("mcp.json");
    let official = json!({"command": "/Applications/Tabularis.app/tabularis", "args": ["--mcp"], "env": {"A": "1"}});
    let other = json!({"command": "other"});
    fs::write(
        &path,
        serde_json::to_string_pretty(&json!({
            "mcpServers": {"tabularis": official, "other": other},
            "theme": "dark"
        }))
        .unwrap(),
    )
    .unwrap();

    write_server_entry(&path, "/fork/tabularis-extended").unwrap();

    let after: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        serde_json::to_string(&after["mcpServers"]["tabularis"]).unwrap(),
        serde_json::to_string(&official).unwrap()
    );
    assert_eq!(after["mcpServers"]["other"], other);
    assert_eq!(after["theme"], "dark");
    assert_eq!(
        after["mcpServers"][MCP_SERVER_KEY],
        json!({"command": "/fork/tabularis-extended", "args": ["--mcp"]})
    );
    assert!(is_tabularis_in_mcp_servers(&path));
}

#[test]
fn install_creates_a_missing_config_and_refuses_invalid_ones() {
    let temp_dir = tempfile::tempdir().unwrap();
    let fresh = temp_dir.path().join("fresh.json");
    write_server_entry(&fresh, "/fork/tabularis-extended").unwrap();
    let written: Value = serde_json::from_str(&fs::read_to_string(&fresh).unwrap()).unwrap();
    assert_eq!(
        written["mcpServers"][MCP_SERVER_KEY]["args"],
        json!(["--mcp"])
    );
    assert!(written["mcpServers"].get("tabularis").is_none());

    let broken = temp_dir.path().join("broken.json");
    fs::write(&broken, "{not json").unwrap();
    assert!(write_server_entry(&broken, "/fork/tabularis-extended").is_err());
    assert_eq!(fs::read_to_string(&broken).unwrap(), "{not json");
}
