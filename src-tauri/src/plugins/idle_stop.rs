//! Idle stop of lazily started driver processes.
//!
//! A driver process that the headless MCP and service modes start on its first
//! call can be stopped again after it has been idle for a while; the next call
//! starts it anew. The idle period for a plugin is, in order of precedence:
//! 1. the plugin's own override (`plugins.<id>.idleStopSeconds`),
//! 2. the global host setting (`pluginIdleStopSeconds`),
//! 3. disabled.
//!
//! A value of `0` disables the idle stop. The desktop app and `tabularis web`
//! start their drivers eagerly and never stop them for being idle.
//!
//! The resolved values are kept in a small process-wide snapshot refreshed
//! whenever the config is loaded or saved, like [`crate::plugins::call_timeout`].
//!
//! A process with an open service session or transaction must keep running.
//! The session holds an [`IdleHold`] on its plugin id for as long as it lives.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, RwLock};
use std::time::Duration;

use once_cell::sync::Lazy;

use crate::config::AppConfig;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct IdleStopSettings {
    global: Option<u32>,
    per_plugin: HashMap<String, u32>,
}

impl IdleStopSettings {
    pub(crate) fn from_config(config: &AppConfig) -> Self {
        let per_plugin = config
            .plugins
            .as_ref()
            .map(|plugins| {
                plugins
                    .iter()
                    .filter_map(|(id, cfg)| cfg.idle_stop_seconds.map(|s| (id.clone(), s)))
                    .collect()
            })
            .unwrap_or_default();
        Self {
            global: config.plugin_idle_stop_seconds,
            per_plugin,
        }
    }

    /// Idle period for `plugin_id`; `None` means "never stop for being idle".
    pub(crate) fn resolve(&self, plugin_id: &str) -> Option<Duration> {
        let seconds = self
            .per_plugin
            .get(plugin_id)
            .copied()
            .or(self.global)
            .unwrap_or(0);
        (seconds > 0).then(|| Duration::from_secs(u64::from(seconds)))
    }
}

static CURRENT: Lazy<RwLock<IdleStopSettings>> =
    Lazy::new(|| RwLock::new(IdleStopSettings::default()));

/// Refreshes the snapshot from a freshly loaded or saved config.
pub fn apply_config(config: &AppConfig) {
    let next = IdleStopSettings::from_config(config);
    if let Ok(mut current) = CURRENT.write() {
        *current = next;
    }
}

/// Idle period for `plugin_id` under the current config; `None` when disabled.
pub fn for_plugin(plugin_id: &str) -> Option<Duration> {
    #[cfg(test)]
    if let Some(forced) = test_support::forced(plugin_id) {
        return forced;
    }
    CURRENT
        .read()
        .map(|current| current.resolve(plugin_id))
        .unwrap_or(None)
}

static HOLDS: Lazy<Mutex<HashMap<String, usize>>> = Lazy::new(|| Mutex::new(HashMap::new()));

/// Keeps the plugin's process running while it is alive. Dropping it releases
/// the hold.
#[derive(Debug)]
pub struct IdleHold {
    plugin_id: String,
}

/// Prevents the idle stop of `plugin_id`'s process until the guard is dropped.
pub fn hold(plugin_id: &str) -> IdleHold {
    if let Ok(mut holds) = HOLDS.lock() {
        *holds.entry(plugin_id.to_string()).or_insert(0) += 1;
    }
    IdleHold {
        plugin_id: plugin_id.to_string(),
    }
}

/// Plugins whose process must run until this process exits.
static KEPT: Lazy<Mutex<HashSet<String>>> = Lazy::new(|| Mutex::new(HashSet::new()));

/// Never stops `plugin_id`'s process while idle again. Used after a write that
/// bypassed service sessions, which may leave a transaction open inside the
/// plugin that the host cannot see.
pub fn keep_running(plugin_id: &str) {
    if let Ok(mut kept) = KEPT.lock() {
        kept.insert(plugin_id.to_string());
    }
}

/// Whether any [`IdleHold`] on `plugin_id` is alive, or it must keep running.
pub fn is_held(plugin_id: &str) -> bool {
    // A poisoned registry cannot say; keeping the process is the safe answer.
    let kept = KEPT
        .lock()
        .map(|kept| kept.contains(plugin_id))
        .unwrap_or(true);
    kept || HOLDS
        .lock()
        .map(|holds| holds.contains_key(plugin_id))
        .unwrap_or(true)
}

impl Drop for IdleHold {
    fn drop(&mut self) {
        let Ok(mut holds) = HOLDS.lock() else {
            return;
        };
        if let Some(count) = holds.get_mut(&self.plugin_id) {
            *count -= 1;
            if *count == 0 {
                holds.remove(&self.plugin_id);
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Per-plugin-id overrides that win over the config snapshot, so tests that
    //! run in parallel (and others that reload the config) cannot disturb each
    //! other's idle period. Use a plugin id that is unique to the test.

    use super::*;

    static FORCED: Lazy<Mutex<HashMap<String, Option<Duration>>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));

    pub(super) fn forced(plugin_id: &str) -> Option<Option<Duration>> {
        FORCED.lock().ok()?.get(plugin_id).copied()
    }

    pub(crate) struct ForcedIdle {
        plugin_id: String,
    }

    pub(crate) fn force(plugin_id: &str, idle: Option<Duration>) -> ForcedIdle {
        if let Ok(mut forced) = FORCED.lock() {
            forced.insert(plugin_id.to_string(), idle);
        }
        ForcedIdle {
            plugin_id: plugin_id.to_string(),
        }
    }

    impl Drop for ForcedIdle {
        fn drop(&mut self) {
            if let Ok(mut forced) = FORCED.lock() {
                forced.remove(&self.plugin_id);
            }
        }
    }
}

#[cfg(test)]
#[path = "idle_stop_settings_tests.rs"]
mod tests;
