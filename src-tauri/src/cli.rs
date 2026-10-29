//! Command-line argument parsing for the Tabularis binary.
//!
//! Keeping this in its own module means `lib.rs` does not have to know about
//! clap, and the command surface (`--mcp`, `--list-drivers`, `web`, `service`,
//! `--debug`, `--explain`, `--help`, `--version`) lives in one place.

use clap::{Args as ClapArgs, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum WebAuthMode {
    /// Authenticate remote users with a password.
    Password,
    /// Trust authentication performed by a configured reverse proxy.
    Proxy,
}

#[derive(Parser, Debug)]
#[command(version, about, long_about = None, args_conflicts_with_subcommands = true)]
pub struct Args {
    /// Start in MCP Server mode (Model Context Protocol)
    #[arg(long)]
    pub mcp: bool,

    /// List driver manifests without connecting to a database, then exit
    #[arg(long)]
    pub list_drivers: bool,

    #[command(subcommand)]
    pub command: Option<Command>,

    /// Enable debug logging (including sqlx queries)
    #[arg(long, global = true)]
    pub debug: bool,

    /// Open a Visual Explain window for a previously-saved EXPLAIN file
    /// (Postgres `EXPLAIN (FORMAT JSON)` output).
    #[arg(long, value_name = "FILE")]
    pub explain: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Start the browser-based Web UI
    Web(WebArgs),
    Service(ServiceArgs),
}

#[derive(Debug, ClapArgs)]
pub struct WebArgs {
    /// Address for the Web UI server to bind
    #[arg(long, value_name = "HOST", default_value = "127.0.0.1")]
    pub host: String,

    /// Port for the Web UI server to bind
    #[arg(long, value_name = "PORT", default_value_t = 8080)]
    pub port: u16,

    /// Do not open the Web UI in the default browser
    #[arg(long)]
    pub no_open: bool,

    /// Override the directory containing built Web UI assets
    #[arg(long, value_name = "PATH")]
    pub web_root: Option<PathBuf>,

    /// Authentication mode for explicitly configured remote Web UI access
    #[arg(
        long,
        value_name = "MODE",
        value_enum,
        requires_all = ["public_url", "allowed_origins"]
    )]
    pub auth: Option<WebAuthMode>,

    /// Public HTTPS origin used by browsers through the trusted reverse proxy
    #[arg(long, value_name = "URL", requires = "auth")]
    pub public_url: Option<String>,

    /// Browser origin allowed to access remote Web UI sessions; repeat as needed
    #[arg(
        long = "allowed-origin",
        value_name = "ORIGIN",
        action = clap::ArgAction::Append,
        requires = "auth"
    )]
    pub allowed_origins: Vec<String>,

    /// Trust X-Forwarded-For only from this proxy network; repeat as needed
    #[arg(long = "trusted-proxy", value_name = "CIDR", requires = "auth")]
    pub trusted_proxies: Vec<ipnet::IpNet>,

    /// Grant remote sessions high-risk local-administrator capabilities
    #[arg(long, requires = "auth")]
    pub allow_high_risk: bool,

    /// Allow the Web UI to browse this server directory; repeat as needed
    #[arg(
        long = "server-file-browser-root",
        value_name = "PATH",
        action = clap::ArgAction::Append
    )]
    pub server_file_browser_roots: Vec<PathBuf>,
}

#[derive(ClapArgs, Debug)]
#[command(args_conflicts_with_subcommands = true)]
pub struct ServiceArgs {
    #[arg(long, value_name = "FILE|-", conflicts_with = "stdio")]
    pub request: Option<String>,
    #[arg(long)]
    pub stdio: bool,
    #[arg(long)]
    pub write: bool,
    #[arg(long,conflicts_with_all=["stdio","credential_fd"],requires="request")]
    pub credential_stdin: bool,
    #[arg(long,value_parser=clap::value_parser!(u8).range(3..=255))]
    pub credential_fd: Option<u8>,
    #[arg(long,requires_all=["stdio","credential_fd"])]
    pub credential_request_id: Option<String>,
    #[command(subcommand)]
    pub action: Option<ServiceAction>,
}

#[derive(Subcommand, Debug)]
pub enum ServiceAction {
    Approve {
        id: String,
        #[arg(long,value_parser=["approve","deny"])]
        decision: String,
        #[arg(long)]
        reason: Option<String>,
    },
    ArtifactSave {
        id: String,
        #[arg(long)]
        output: std::path::PathBuf,
    },
}

impl ServiceArgs {
    pub fn validate(&self) -> Result<(), String> {
        if self.action.is_none() && self.stdio == self.request.is_some() {
            return Err("Choose --request or --stdio".into());
        }
        if self.credential_stdin && self.request.as_deref() == Some("-") {
            return Err("Protected stdin requires a request file".into());
        }
        if self.stdio && self.credential_fd.is_some() && self.credential_request_id.is_none() {
            return Err("Descriptor stdio requires --credential-request-id".into());
        }
        if self.action.is_some()
            && (self.write || self.credential_stdin || self.credential_fd.is_some())
        {
            return Err("Service action flags are invalid".into());
        }
        Ok(())
    }
}

impl Args {
    /// Log level for this run: `Debug` with `--debug` (which also lets sqlx's
    /// query logging through), otherwise whatever `RUST_LOG` names, and `Info`
    /// as the default.
    pub fn log_level(&self) -> log::LevelFilter {
        if self.debug {
            log::LevelFilter::Debug
        } else {
            crate::logger::resolve_log_level_from_env()
        }
    }

    fn defaults() -> Self {
        Self {
            mcp: false,
            list_drivers: false,
            command: None,
            debug: false,
            explain: None,
        }
    }
}

/// The first argument that is not a flag (or the value of `--explain`).
fn first_positional(argv: &[std::ffi::OsString]) -> Option<String> {
    let mut rest = argv.iter().skip(1);
    while let Some(arg) = rest.next() {
        let arg = arg.to_string_lossy();
        if arg == "--explain" {
            rest.next();
            continue;
        }
        if arg.starts_with('-') {
            continue;
        }
        return Some(arg.into_owned());
    }
    None
}

fn is_service_invocation(argv: &[std::ffi::OsString]) -> bool {
    first_positional(argv).as_deref() == Some("service")
}

fn is_web_invocation(argv: &[std::ffi::OsString]) -> bool {
    first_positional(argv).as_deref() == Some("web")
}

fn service_error(message: &'static str) -> clap::Error {
    use clap::CommandFactory;
    Args::command().error(clap::error::ErrorKind::ArgumentConflict, message)
}

/// Parse an argument vector. `service` and `web` invocations are strict; every
/// other launch falls back to GUI defaults on a parse failure, except `--help`
/// and `--version`, which are returned for clap to print.
pub(crate) fn parse_args(argv: Vec<std::ffi::OsString>) -> Result<Args, clap::Error> {
    let argv: Vec<_> = argv
        .into_iter()
        .filter(|arg| !arg.to_string_lossy().starts_with("-psn_"))
        .collect();
    if is_web_invocation(&argv) {
        return Args::try_parse_from(&argv);
    }
    if !is_service_invocation(&argv) {
        return match Args::try_parse_from(&argv) {
            Ok(args) => Ok(args),
            Err(err)
                if matches!(
                    err.kind(),
                    clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
                ) =>
            {
                Err(err)
            }
            Err(_) => Ok(Args::defaults()),
        };
    }
    let args = Args::try_parse_from(&argv)?;
    if let Some(Command::Service(service)) = &args.command {
        if service.validate().is_err() {
            return Err(service_error("Invalid service arguments"));
        }
    }
    if (args.mcp || args.list_drivers) && args.command.is_some() {
        return Err(service_error(
            "MCP, driver listing, and service modes are exclusive",
        ));
    }
    Ok(args)
}

/// Parse the process arguments, with platform-friendly fallback behaviour.
///
/// - `--help` / `--version` surface as `Err(DisplayHelp|DisplayVersion)` with the
///   formatted message attached; let clap print them and exit cleanly.
/// - Any other parse failure of a desktop launch falls back to defaults so
///   that GUI launches (e.g. macOS passing `-psn_*`, or a `tabularis-extended://` deep
///   link passed as a positional argument) still reach the Tauri builder.
/// - The `service` and `web` subcommands are strict: invalid arguments exit with
///   an error.
pub fn parse() -> Args {
    parse_args(std::env::args_os().collect()).unwrap_or_else(|err| err.exit())
}

#[cfg(test)]
fn try_parse_from<I, T>(arguments: I) -> Result<Args, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString>,
{
    let arguments: Vec<std::ffi::OsString> = arguments.into_iter().map(Into::into).collect();
    match Args::try_parse_from(arguments.clone()) {
        Err(_) if has_only_platform_launch_arguments(&arguments) => Ok(Args::defaults()),
        result => result,
    }
}

#[cfg(test)]
fn has_only_platform_launch_arguments(arguments: &[std::ffi::OsString]) -> bool {
    let mut launch_arguments = arguments.iter().skip(1);
    let Some(first) = launch_arguments.next() else {
        return false;
    };

    std::iter::once(first)
        .chain(launch_arguments)
        .all(|argument| {
            argument.to_str().is_some_and(|value| {
                value.starts_with("-psn_")
                    || value.starts_with("tabularis:")
                    || value
                        .starts_with(&format!("{}:", crate::plugins::deep_link::DEEP_LINK_SCHEME))
            })
        })
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod log_level_tests;
