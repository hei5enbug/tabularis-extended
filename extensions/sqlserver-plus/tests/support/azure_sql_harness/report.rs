use super::{Error, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{fs, io::Write, path::Path};

#[derive(Serialize)]
pub struct Check {
    pub name: &'static str,
    pub status: &'static str,
    pub evidence: &'static str,
}
#[derive(Serialize)]
pub struct Report {
    pub success: bool,
    pub integration_complete: bool,
    pub evidence_kind: &'static str,
    pub checks: Vec<Check>,
    pub error: Option<&'static str>,
    pub rpc_version: u32,
    pub auth_modes: Vec<String>,
    pub os: &'static str,
    pub elapsed_ms: u64,
    pub calls: usize,
    pub writes_dispatched: usize,
    pub unknown_write_outcomes: usize,
    pub cleanup_remaining: usize,
    pub remaining_id_sha256: Option<String>,
    pub cancellation_phase: &'static str,
    pub cancellation_write_outcome: &'static str,
    pub source_sha256: String,
}
impl Report {
    pub fn new() -> Self {
        let mut hash = Sha256::new();
        for bytes in [
            include_bytes!("../../../src/handlers/service.rs").as_slice(),
            include_bytes!("../../../src/driver/service/types.rs").as_slice(),
            include_bytes!("run.rs").as_slice(),
            include_bytes!("config.rs").as_slice(),
            include_bytes!("credentials.rs").as_slice(),
            include_bytes!("rpc.rs").as_slice(),
        ] {
            hash.update(bytes);
            hash.update([0]);
        }
        Self {
            success: false,
            integration_complete: false,
            evidence_kind: "preflight_only",
            checks: Vec::new(),
            error: None,
            rpc_version: 1,
            auth_modes: Vec::new(),
            os: std::env::consts::OS,
            elapsed_ms: 0,
            calls: 0,
            writes_dispatched: 0,
            unknown_write_outcomes: 0,
            cleanup_remaining: 0,
            remaining_id_sha256: None,
            cancellation_phase: "not_observed",
            cancellation_write_outcome: "not_observed",
            source_sha256: format!("{:x}", hash.finalize()),
        }
    }
    pub fn check(&mut self, name: &'static str, status: &'static str, evidence: &'static str) {
        self.checks.push(Check {
            name,
            status,
            evidence,
        });
    }
    pub fn deferred(&mut self) {
        for name in [
            "browser_device_flow",
            "mfa",
            "token_acquisition",
            "refresh_provider",
            "wrong_host_certificate",
            "untrusted_certificate",
            "expired_certificate",
            "host_gui_mcp_cli_parity",
            "server_execution_cancel_phase",
            "actual_write_cancel",
            "lost_write_response_fault",
            "windows_manual_harness_owner_acl",
        ] {
            self.check(name, "deferred", "not_observed");
        }
    }
    pub fn bytes(&self) -> Result<Vec<u8>> {
        serde_json::to_vec(self).map_err(|_| Error("REPORT_FAILED"))
    }
}
#[cfg(unix)]
pub fn publish(report: &Report, destination: &Path) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    if !destination.is_absolute() || destination.file_name().is_none() {
        return Err(Error("REPORT_FAILED"));
    }
    let parent = destination.parent().ok_or(Error("REPORT_FAILED"))?;
    let metadata = fs::symlink_metadata(parent).map_err(|_| Error("REPORT_FAILED"))?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.mode() & 0o077 != 0
        || metadata.uid() != unsafe { getuid() }
    {
        return Err(Error("REPORT_NOT_PRIVATE"));
    }
    if fs::symlink_metadata(destination).is_ok() {
        return Err(Error("REPORT_EXISTS"));
    }
    let temporary = parent.join(format!(".azure-report-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|_| Error("REPORT_FAILED"))?;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|_| Error("REPORT_FAILED"))?;
        file.write_all(&report.bytes()?)
            .map_err(|_| Error("REPORT_FAILED"))?;
        file.sync_all().map_err(|_| Error("REPORT_FAILED"))?;
        fs::hard_link(&temporary, destination).map_err(|_| Error("REPORT_EXISTS"))?;
        Ok(())
    })();
    let _ = fs::remove_file(temporary);
    result
}
#[cfg(not(unix))]
pub fn publish(_: &Report, _: &Path) -> Result<()> {
    Err(Error("UNSUPPORTED_PLATFORM"))
}
