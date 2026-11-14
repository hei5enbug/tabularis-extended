//! A driver started on its first call must not own a child process before
//! then. The fixture plugin records each start in a marker file next to it.

use super::process_exit_tests::has_exited;
use super::*;
use crate::drivers::driver_trait::DatabaseDriver;
use crate::drivers::registry;

const LIMIT: Duration = Duration::from_secs(10);
const FIXTURE_ID: &str = "lazy-manager-fixture";

fn fixture_copy(dir: &std::path::Path) -> PathBuf {
    fixture_copy_of("marker_plugin.sh", dir)
}

pub(super) fn fixture_copy_of(name: &str, dir: &std::path::Path) -> PathBuf {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let script = dir.join(name);
    std::fs::copy(source, &script).expect("copy the fixture plugin");
    script
}

/// Polls `condition` until it holds or `limit` passes.
pub(super) async fn wait_until(limit: Duration, condition: impl Fn() -> bool) -> bool {
    let deadline = std::time::Instant::now() + limit;
    while std::time::Instant::now() < deadline {
        if condition() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    condition()
}

fn start_count(dir: &std::path::Path) -> usize {
    std::fs::read_to_string(dir.join("started.marker"))
        .map(|content| content.lines().count())
        .unwrap_or(0)
}

async fn lazy_driver(script: PathBuf, interpreter: Option<&str>) -> RpcDriver {
    RpcDriver::new_with_start(
        test_manifest(),
        script,
        interpreter.map(str::to_string),
        vec![],
        HashMap::new(),
        ProcessStart::OnFirstCall,
    )
    .await
    .expect("a lazy driver is constructed without starting anything")
}

/// Gives a wrongly eager spawn time to run the script and write its marker.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(300)).await;
}

fn missing_executable() -> PathBuf {
    PathBuf::from("/nonexistent/tabularis-plugin-that-is-not-installed")
}

#[tokio::test]
async fn first_call_starts_the_process_that_construction_did_not() {
    // Catches an eager spawn in the headless process and a broken first call.
    // given
    let dir = tempfile::tempdir().unwrap();
    let driver = lazy_driver(fixture_copy(dir.path()), Some("sh")).await;
    settle().await;
    assert_eq!(driver.process.pid(), None);
    assert_eq!(start_count(dir.path()), 0);

    // when
    let answer = tokio::time::timeout(LIMIT, driver.process.call("ping", json!({}))).await;

    // then
    assert_eq!(answer.expect("no answer in time").unwrap(), json!("pong"));
    let pid = driver.process.pid().expect("the first call starts the child");
    assert!(!has_exited(pid), "the started plugin should be running");
    assert_eq!(start_count(dir.path()), 1);
    driver.terminate().await;
}

#[tokio::test]
async fn concurrent_first_calls_start_exactly_one_process() {
    // Catches a double spawn when two callers race to be the first.
    // given
    let dir = tempfile::tempdir().unwrap();
    let driver = Arc::new(lazy_driver(fixture_copy(dir.path()), Some("sh")).await);

    // when
    let (first, second) = tokio::time::timeout(LIMIT, async {
        tokio::join!(
            driver.process.call("ping", json!({})),
            driver.process.call("ping", json!({}))
        )
    })
    .await
    .expect("no answer in time");

    // then
    assert_eq!(first.unwrap(), json!("pong"));
    assert_eq!(second.unwrap(), json!("pong"));
    assert_eq!(start_count(dir.path()), 1);
    driver.terminate().await;
}

#[tokio::test]
async fn shutdown_before_any_call_starts_nothing() {
    // Catches spawning a process only to stop it, and a hang on shutdown.
    // given
    let dir = tempfile::tempdir().unwrap();
    let driver = lazy_driver(fixture_copy(dir.path()), Some("sh")).await;

    // when
    let stopped = tokio::time::timeout(Duration::from_secs(2), driver.shutdown()).await;
    settle().await;

    // then
    assert!(stopped.is_ok(), "shutdown of an unstarted driver hung");
    assert_eq!(driver.process.pid(), None);
    assert_eq!(start_count(dir.path()), 0);
    let late_call = driver.process.call("ping", json!({})).await;
    assert!(late_call.is_err(), "a stopped driver must reject new calls");
    assert_eq!(start_count(dir.path()), 0);
}

#[tokio::test]
async fn terminate_before_any_call_starts_nothing() {
    // Catches spawning on the SIGTERM path of the headless process.
    // given
    let dir = tempfile::tempdir().unwrap();
    let driver = lazy_driver(fixture_copy(dir.path()), Some("sh")).await;

    // when
    let stopped = tokio::time::timeout(Duration::from_secs(2), driver.terminate()).await;
    settle().await;

    // then
    assert!(stopped.is_ok(), "terminate of an unstarted driver hung");
    assert_eq!(driver.process.pid(), None);
    assert_eq!(start_count(dir.path()), 0);
}

#[tokio::test]
async fn missing_executable_fails_the_first_call_and_not_the_construction() {
    // Catches a hang or a panic in the manager task when the start fails.
    // given
    let driver = lazy_driver(missing_executable(), None).await;

    // when
    let first = tokio::time::timeout(LIMIT, driver.process.call("ping", json!({}))).await;
    let retry = tokio::time::timeout(LIMIT, driver.process.call("ping", json!({}))).await;

    // then
    for outcome in [first, retry] {
        let error = outcome
            .expect("a failed start must not hang the call")
            .unwrap_err();
        assert!(
            error.contains("Failed to start plugin process"),
            "unexpected error: {error}"
        );
    }
    assert_eq!(driver.process.pid(), None);
    driver.terminate().await;
}

#[tokio::test]
async fn eager_driver_with_a_missing_executable_fails_construction() {
    // Catches changing the desktop behaviour of reporting a spawn failure at load.
    // given
    let missing = missing_executable();

    // when
    let created = RpcDriver::new(test_manifest(), missing, None, vec![], HashMap::new()).await;

    // then
    assert!(created
        .err()
        .is_some_and(|error| error.contains("Failed to start plugin process")));
}

#[tokio::test]
async fn eager_driver_has_a_running_process_right_after_construction() {
    // Catches changing the desktop and web behaviour of spawning at load.
    // given
    let dir = tempfile::tempdir().unwrap();

    // when
    let driver = RpcDriver::new(
        test_manifest(),
        fixture_copy(dir.path()),
        Some("sh".to_string()),
        vec![],
        HashMap::new(),
    )
    .await
    .expect("spawn the fixture plugin");

    // then
    let pid = driver.process.pid().expect("an eager driver has a pid");
    assert!(!has_exited(pid));
    driver.terminate().await;
}

struct SwitchReset;

impl Drop for SwitchReset {
    fn drop(&mut self) {
        crate::plugins::manager::set_start_processes_on_first_call(false);
    }
}

async fn load_fixture_plugin(dir: &std::path::Path) -> Arc<dyn DatabaseDriver> {
    fixture_copy(dir);
    let manifest = json!({
        "name": FIXTURE_ID,
        "version": "1.0.0",
        "description": "fixture",
        "executable": "marker_plugin.sh",
        "interpreter": "sh",
    });
    std::fs::write(dir.join(".tabularium"), manifest.to_string()).unwrap();
    crate::plugins::manager::load_plugin_from_dir(dir, None, HashMap::new())
        .await
        .expect("load the fixture plugin");
    registry::get_driver(FIXTURE_ID)
        .await
        .expect("the fixture plugin is registered")
}

#[tokio::test]
async fn headless_switch_registers_a_plugin_without_starting_it() {
    // Catches `--mcp` and `service` spawning every installed driver at startup.
    // given
    let _guard = registry::REGISTRY_TEST_LOCK.lock().await;
    let _reset = SwitchReset;
    crate::plugins::manager::start_driver_processes_on_first_call();
    let dir = tempfile::tempdir().unwrap();

    // when
    let driver = load_fixture_plugin(dir.path()).await;
    settle().await;

    // then
    assert_eq!(driver.pid(), None);
    assert_eq!(start_count(dir.path()), 0);
    registry::unregister_driver(FIXTURE_ID).await;
}

#[tokio::test]
async fn without_the_headless_switch_a_loaded_plugin_starts_at_registration() {
    // Catches the desktop app and `tabularis web` losing their eager spawn.
    // given
    let _guard = registry::REGISTRY_TEST_LOCK.lock().await;
    crate::plugins::manager::set_start_processes_on_first_call(false);
    let dir = tempfile::tempdir().unwrap();

    // when
    let driver = load_fixture_plugin(dir.path()).await;

    // then
    assert!(driver.pid().is_some());
    registry::unregister_driver(FIXTURE_ID).await;
}
