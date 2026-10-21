//! Literal values for read-only round-trip checks. The harness never reads or
//! writes a table; every value is bound as a SELECT parameter.
pub const DECIMAL: &str = "12345678901234567890.123456789012345678";
pub const OBSERVED_AT: &str = "2030-01-02T03:04:05+09:00";
