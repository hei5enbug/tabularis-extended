pub mod config;
pub mod credentials;
pub mod fixture;
pub mod report;
pub mod rpc;
pub mod run;

#[derive(Clone, Copy)]
pub struct Error(pub &'static str);
impl std::fmt::Debug for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;
pub const REQUEST_MS: u64 = 120_000;
pub const FRAME_BYTES: usize = 16 * 1024 * 1024;
pub const LOG_BYTES: usize = 8 * 1024 * 1024;
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}
