//! Command-line argument parsing for the Tabularis binary.
//!
//! Keeping this in its own module means `lib.rs` does not have to know about
//! clap, and the flag surface (`--mcp`, `--debug`, `--explain`, `--help`,
//! `--version`) lives in one place.

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
pub struct Args {
    #[command(subcommand)]
    pub command: Option<Command>,
    /// Start in MCP Server mode (Model Context Protocol)
    #[arg(long)]
    pub mcp: bool,

    /// Enable debug logging (including sqlx queries)
    #[arg(long)]
    pub debug: bool,

    /// Open a Visual Explain window for a previously-saved EXPLAIN file
    /// (Postgres `EXPLAIN (FORMAT JSON)` output).
    #[arg(long, value_name = "FILE")]
    pub explain: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    Service(ServiceArgs),
}

#[derive(clap::Args, Debug)]
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
    fn defaults() -> Self {
        Self {
            command: None,
            mcp: false,
            debug: false,
            explain: None,
        }
    }
}

fn is_service_invocation(argv: &[std::ffi::OsString]) -> bool {
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
        return arg == "service";
    }
    false
}

fn service_error(message: &'static str) -> clap::Error {
    use clap::CommandFactory;
    Args::command().error(clap::error::ErrorKind::ArgumentConflict, message)
}

/// Parse an argument vector. `service` invocations are strict; every other
/// launch falls back to GUI defaults on a parse failure, except `--help` and
/// `--version`, which are returned for clap to print.
pub(crate) fn parse_args(argv: Vec<std::ffi::OsString>) -> Result<Args, clap::Error> {
    let argv: Vec<_> = argv
        .into_iter()
        .filter(|arg| !arg.to_string_lossy().starts_with("-psn_"))
        .collect();
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
    if args.mcp && args.command.is_some() {
        return Err(service_error("MCP and service modes are exclusive"));
    }
    Ok(args)
}

/// Parse the process arguments, with platform-friendly fallback behaviour.
///
/// - `--help` / `--version` surface as `Err(DisplayHelp|DisplayVersion)` with the
///   formatted message attached; let clap print them and exit cleanly.
/// - Any other parse failure of a non-service launch falls back to defaults so
///   that GUI launches (e.g. macOS passing `-psn_*`, or a `tabularis://` deep
///   link passed as a positional argument) still reach the Tauri builder.
/// - The `service` subcommand is strict: invalid arguments exit with an error.
pub fn parse() -> Args {
    parse_args(std::env::args_os().collect()).unwrap_or_else(|err| err.exit())
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod tests;
