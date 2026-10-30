//! Stops plugin processes when the MCP server is terminated by a signal
//! (issue #1280).
//!
//! When a client closes stdin, `run_mcp_server` returns and dropping the
//! Tokio runtime kills every plugin child (`kill_on_drop`). MCP clients also
//! stop servers with SIGTERM, for example when the server is still busy a
//! moment after stdin closed, and terminals send SIGHUP and SIGINT. The
//! default action for those signals ends the process without dropping the
//! runtime, so the plugins are left running unless they exit on stdin EOF by
//! themselves.

/// Kill the plugin processes, then exit, on SIGTERM or SIGHUP. SIGINT is not
/// handled here: it cancels the running request and the server shuts down in
/// order (see `service_native::cli::run_mcp`).
#[cfg(unix)]
pub(crate) fn stop_plugins_on_termination_signal() {
    use tokio::signal::unix::{signal, SignalKind};

    for kind in [SignalKind::terminate(), SignalKind::hangup()] {
        let mut stream = match signal(kind) {
            Ok(stream) => stream,
            Err(error) => {
                log::warn!(
                    "Cannot handle signal {}; plugins rely on stdin EOF to exit: {error}",
                    kind.as_raw_value()
                );
                continue;
            }
        };
        tokio::spawn(async move {
            if stream.recv().await.is_some() {
                eprintln!(
                    "[MCP] Received signal {}, stopping plugin drivers",
                    kind.as_raw_value()
                );
                crate::drivers::registry::terminate_external_drivers().await;
                // `exit` skips `Drop`, which would remove this process's
                // local-ipc record and socket.
                crate::services::local_ipc::remove_own_instances();
                std::process::exit(128 + kind.as_raw_value());
            }
        });
    }
}

/// Windows has no termination signal for a windowless process: clients call
/// `TerminateProcess`, and the job object in `plugins::process_lifetime`
/// stops the plugins.
#[cfg(not(unix))]
pub(crate) fn stop_plugins_on_termination_signal() {}
