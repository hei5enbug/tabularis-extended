use super::*;
use crate::config::PluginConfig;

fn config_with(global: Option<u32>, overrides: &[(&str, Option<u32>)]) -> AppConfig {
    let plugins = overrides
        .iter()
        .map(|(id, seconds)| {
            (
                id.to_string(),
                PluginConfig {
                    idle_stop_seconds: *seconds,
                    ..PluginConfig::default()
                },
            )
        })
        .collect();
    AppConfig {
        plugin_idle_stop_seconds: global,
        plugins: Some(plugins),
        ..AppConfig::default()
    }
}

#[test]
fn is_disabled_when_nothing_is_configured() {
    let settings = IdleStopSettings::from_config(&AppConfig::default());
    assert_eq!(settings.resolve("postgresql"), None);
}

#[test]
fn global_setting_applies_to_every_plugin() {
    let settings = IdleStopSettings::from_config(&config_with(Some(300), &[]));
    assert_eq!(
        settings.resolve("postgresql"),
        Some(Duration::from_secs(300))
    );
    assert_eq!(settings.resolve("duckdb"), Some(Duration::from_secs(300)));
}

#[test]
fn plugin_override_wins_over_global() {
    let settings = IdleStopSettings::from_config(&config_with(
        Some(300),
        &[("postgresql", Some(30)), ("duckdb", None)],
    ));
    assert_eq!(
        settings.resolve("postgresql"),
        Some(Duration::from_secs(30))
    );
    assert_eq!(settings.resolve("duckdb"), Some(Duration::from_secs(300)));
}

#[test]
fn zero_disables_the_idle_stop() {
    let settings = IdleStopSettings::from_config(&config_with(Some(0), &[]));
    assert_eq!(settings.resolve("postgresql"), None);

    let settings =
        IdleStopSettings::from_config(&config_with(Some(60), &[("postgresql", Some(0))]));
    assert_eq!(settings.resolve("postgresql"), None);
    assert_eq!(settings.resolve("duckdb"), Some(Duration::from_secs(60)));
}

#[test]
fn plugin_override_can_enable_it_when_global_is_disabled() {
    let settings =
        IdleStopSettings::from_config(&config_with(Some(0), &[("postgresql", Some(45))]));
    assert_eq!(
        settings.resolve("postgresql"),
        Some(Duration::from_secs(45))
    );
    assert_eq!(settings.resolve("duckdb"), None);
}

#[test]
fn reads_the_camel_case_config_keys() {
    let config: AppConfig = serde_json::from_str(
        r#"{"pluginIdleStopSeconds":120,"plugins":{"duckdb":{"idleStopSeconds":10}}}"#,
    )
    .unwrap();
    let settings = IdleStopSettings::from_config(&config);
    assert_eq!(settings.resolve("duckdb"), Some(Duration::from_secs(10)));
    assert_eq!(
        settings.resolve("postgresql"),
        Some(Duration::from_secs(120))
    );
}

#[test]
fn holds_count_per_plugin_and_release_on_drop() {
    let id = "idle-stop-hold-count";
    assert!(!is_held(id));

    let first = hold(id);
    let second = hold(id);
    assert!(is_held(id));
    assert!(!is_held("idle-stop-hold-count-other"));

    drop(first);
    assert!(is_held(id), "one hold is still alive");
    drop(second);
    assert!(!is_held(id));
    assert!(
        !HOLDS.lock().unwrap().contains_key(id),
        "a released plugin leaves no entry behind"
    );
}
