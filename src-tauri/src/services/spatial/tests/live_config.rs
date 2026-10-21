use std::path::PathBuf;

pub(super) struct LiveConfig {
    pub(super) plugin: PathBuf,
    pub(super) port: u16,
}

pub(super) fn postgis17() -> LiveConfig {
    load("TABULARIS_TEST_POSTGIS17_PORT")
}

pub(super) fn postgis18() -> LiveConfig {
    load("TABULARIS_TEST_POSTGIS18_PORT")
}

fn load(port_variable: &str) -> LiveConfig {
    let plugin = std::env::var_os("TABULARIS_TEST_POSTGIS_PLUGIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            panic!("TABULARIS_TEST_POSTGIS_PLUGIN must specify an absolute existing file")
        });
    assert!(
        plugin.is_absolute() && plugin.is_file(),
        "TABULARIS_TEST_POSTGIS_PLUGIN must specify an absolute existing file"
    );
    let value = std::env::var(port_variable)
        .unwrap_or_else(|_| panic!("{port_variable} must specify a nonzero u16 port"));
    assert!(
        !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()),
        "{port_variable} must specify a nonzero u16 port"
    );
    let port = value
        .parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
        .unwrap_or_else(|| panic!("{port_variable} must specify a nonzero u16 port"));
    LiveConfig { plugin, port }
}
