#[cfg(target_os = "windows")]
use directories::ProjectDirs;

use directories::BaseDirs;

use serde_json::json;
use std::fs;
use std::path::PathBuf;
use tauri::{AppHandle, Runtime};

/// "file"  → standard mcpServers JSON config file
/// "command" → installed via CLI (e.g. `claude mcp add`)
#[derive(serde::Serialize, Clone)]
pub struct McpClientStatus {
    pub client_id: String,
    pub client_name: String,
    pub installed: bool,
    pub config_path: Option<String>,
    pub executable_path: String,
    pub client_type: String,
    pub manual_command: Option<String>,
}

struct McpClient {
    id: &'static str,
    name: &'static str,
    config_path: Option<PathBuf>,
    client_type: &'static str,
}

fn get_all_clients() -> Vec<McpClient> {
    let base = BaseDirs::new();

    let claude_path = {
        #[cfg(target_os = "macos")]
        {
            base.as_ref().map(|b| {
                b.home_dir()
                    .join("Library/Application Support/Claude/claude_desktop_config.json")
            })
        }
        #[cfg(target_os = "windows")]
        {
            ProjectDirs::from("", "", "Claude")
                .map(|p| p.config_dir().join("claude_desktop_config.json"))
        }
        #[cfg(target_os = "linux")]
        {
            base.as_ref()
                .map(|b| b.config_dir().join("Claude/claude_desktop_config.json"))
        }
    };

    // Claude Code stores user-scope MCP in ~/.claude.json
    let claude_code_path = base.as_ref().map(|b| b.home_dir().join(".claude.json"));

    let codex_path = base
        .as_ref()
        .map(|b| b.home_dir().join(".codex/config.toml"));

    let cursor_path = base.as_ref().map(|b| b.home_dir().join(".cursor/mcp.json"));

    let windsurf_path = base
        .as_ref()
        .map(|b| b.home_dir().join(".codeium/windsurf/mcp_config.json"));

    let antigravity_path = base
        .as_ref()
        .map(|b| b.home_dir().join(".gemini/antigravity/mcp_config.json"));

    vec![
        McpClient {
            id: "claude",
            name: "Claude Desktop",
            config_path: claude_path,
            client_type: "file",
        },
        McpClient {
            id: "claude_code",
            name: "Claude Code",
            config_path: claude_code_path,
            client_type: "command",
        },
        McpClient {
            id: "codex",
            name: "Codex",
            config_path: codex_path,
            client_type: "command",
        },
        McpClient {
            id: "cursor",
            name: "Cursor",
            config_path: cursor_path,
            client_type: "file",
        },
        McpClient {
            id: "windsurf",
            name: "Windsurf",
            config_path: windsurf_path,
            client_type: "file",
        },
        McpClient {
            id: "antigravity",
            name: "Antigravity",
            config_path: antigravity_path,
            client_type: "file",
        },
    ]
}

/// Key under which the fork registers itself in MCP client configs. The
/// official app uses `tabularis`; that entry is never read, replaced or
/// removed from here.
pub(crate) const MCP_SERVER_KEY: &str = "tabularis-extended";

fn is_tabularis_in_mcp_servers(path: &PathBuf) -> bool {
    if !path.exists() {
        return false;
    }
    fs::read_to_string(path)
        .ok()
        .and_then(|c| serde_json::from_str::<serde_json::Value>(&c).ok())
        .and_then(|j| j.get("mcpServers").cloned())
        .map(|s| s.get(MCP_SERVER_KEY).is_some())
        .unwrap_or(false)
}

/// Command-driven clients store MCP config in different formats; for status
/// purposes, consider Tabularis installed when their config references it.
fn is_command_client_installed(path: &PathBuf) -> bool {
    if !path.exists() {
        return false;
    }
    fs::read_to_string(path)
        .map(|c| {
            c.contains(&format!("\"{MCP_SERVER_KEY}\""))
                || c.contains(&format!("[mcp_servers.{MCP_SERVER_KEY}]"))
                || c.contains(&format!("[mcp_servers.\"{MCP_SERVER_KEY}\"]"))
        })
        .unwrap_or(false)
}

fn build_command_client_invocation(
    client_id: &str,
    exe_str: &str,
) -> Option<(String, Vec<String>)> {
    match client_id {
        "claude_code" => Some((
            "claude".to_string(),
            vec![
                "mcp".to_string(),
                "add".to_string(),
                "--scope".to_string(),
                "user".to_string(),
                MCP_SERVER_KEY.to_string(),
                exe_str.to_string(),
                "--".to_string(),
                "--mcp".to_string(),
            ],
        )),
        "codex" => Some((
            "codex".to_string(),
            vec![
                "mcp".to_string(),
                "add".to_string(),
                MCP_SERVER_KEY.to_string(),
                "--".to_string(),
                exe_str.to_string(),
                "--mcp".to_string(),
            ],
        )),
        _ => None,
    }
}

fn build_manual_command(client_id: &str, exe_str: &str) -> Option<String> {
    let (program, args) = build_command_client_invocation(client_id, exe_str)?;
    Some(format!("{} {}", program, args.join(" ")))
}

#[tauri::command]
pub async fn get_mcp_status<R: Runtime>(
    _app: AppHandle<R>,
) -> Result<Vec<McpClientStatus>, String> {
    let exe_path = std::env::current_exe()
        .map_err(|e| format!("Failed to get executable path: {}", e))?
        .to_string_lossy()
        .to_string();

    let clients = get_all_clients();
    let statuses = clients
        .into_iter()
        .map(|c| {
            let installed = match c.client_type {
                "command" => c
                    .config_path
                    .as_ref()
                    .map(|p| is_command_client_installed(p))
                    .unwrap_or(false),
                _ => c
                    .config_path
                    .as_ref()
                    .map(|p| is_tabularis_in_mcp_servers(p))
                    .unwrap_or(false),
            };
            McpClientStatus {
                client_id: c.id.to_string(),
                client_name: c.name.to_string(),
                installed,
                config_path: c.config_path.map(|p| p.to_string_lossy().to_string()),
                executable_path: exe_path.clone(),
                client_type: c.client_type.to_string(),
                manual_command: build_manual_command(c.id, &exe_path),
            }
        })
        .collect();

    Ok(statuses)
}

/// Adds the fork's server entry to a JSON client config and leaves every
/// other entry, including the official `tabularis` one, as it was. An
/// unreadable or non-JSON config is refused instead of being replaced.
fn write_server_entry(config_path: &std::path::Path, exe_str: &str) -> Result<(), String> {
    let mut config: serde_json::Value = if config_path.exists() {
        let content = fs::read_to_string(config_path).map_err(|e| e.to_string())?;
        if content.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&content)
                .map_err(|e| format!("Config is not valid JSON, left unchanged: {e}"))?
        }
    } else {
        json!({})
    };

    let root = config
        .as_object_mut()
        .ok_or("Config root is not a JSON object, left unchanged")?;
    let servers = root
        .entry("mcpServers")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("mcpServers is not a JSON object, left unchanged")?;
    servers.insert(
        MCP_SERVER_KEY.to_string(),
        json!({
            "command": exe_str,
            "args": ["--mcp"]
        }),
    );

    let new_content = serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?;
    fs::write(config_path, new_content).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn install_mcp_config<R: Runtime>(
    _app: AppHandle<R>,
    client_id: String,
) -> Result<String, String> {
    let exe_path =
        std::env::current_exe().map_err(|e| format!("Failed to get executable path: {}", e))?;
    let exe_str = exe_path.to_string_lossy().to_string();

    let clients = get_all_clients();
    let client = clients
        .iter()
        .find(|c| c.id == client_id)
        .ok_or_else(|| format!("Unknown client: {}", client_id))?;

    if client.client_type == "command" {
        let (program, args) = build_command_client_invocation(client.id, &exe_str)
            .ok_or_else(|| format!("Unsupported command client: {}", client.id))?;
        let manual_command = build_manual_command(client.id, &exe_str)
            .unwrap_or_else(|| format!("{} {}", program, args.join(" ")));
        let output = std::process::Command::new(&program)
            .args(&args)
            .output()
            .map_err(|e| {
                format!(
                    "{} CLI not found. Run manually:\n{}\n(Error: {})",
                    client.name, manual_command, e
                )
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("{} mcp add failed: {}", program, stderr));
        }

        return Ok(client.name.to_string());
    }

    // File-based clients
    let config_path = client
        .config_path
        .as_ref()
        .ok_or("Could not determine config path for this OS")?;

    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    write_server_entry(config_path, &exe_str)?;

    Ok(client.name.to_string())
}

#[cfg(test)]
mod tests {
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
            build_manual_command("codex", "/Applications/Tabularis Extended.app/tabularis-extended").as_deref(),
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

        let after: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
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
        let written: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&fresh).unwrap()).unwrap();
        assert_eq!(written["mcpServers"][MCP_SERVER_KEY]["args"], json!(["--mcp"]));
        assert!(written["mcpServers"].get("tabularis").is_none());

        let broken = temp_dir.path().join("broken.json");
        fs::write(&broken, "{not json").unwrap();
        assert!(write_server_entry(&broken, "/fork/tabularis-extended").is_err());
        assert_eq!(fs::read_to_string(&broken).unwrap(), "{not json");
    }
}
