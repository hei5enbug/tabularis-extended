//! A driver process started on its first call is stopped after it has idled
//! and started again by the next call. The fixture plugin logs each start and
//! each request it receives in `events.log` next to it. Every test forces its
//! idle period on a plugin id of its own, so the tests can run in parallel.

use super::lazy_start_tests::{fixture_copy_of, wait_until};
use super::process_exit_tests::has_exited;
use super::*;
use crate::drivers::driver_trait::DatabaseDriver;
use crate::plugins::idle_stop;

const IDLE: Duration = Duration::from_millis(300);
const LIMIT: Duration = Duration::from_secs(10);

fn idle_fixture(dir: &std::path::Path) -> PathBuf {
    fixture_copy_of("idle_plugin.sh", dir)
}

fn events(dir: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("events.log"))
        .map(|content| content.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

fn manifest_for(id: &str) -> PluginManifest {
    PluginManifest {
        id: id.to_string(),
        ..test_manifest()
    }
}

async fn driver_started(id: &str, script: PathBuf, start: ProcessStart) -> RpcDriver {
    RpcDriver::new_with_start(
        manifest_for(id),
        script,
        Some("sh".to_string()),
        vec![],
        HashMap::new(),
        start,
    )
    .await
    .expect("construct the driver")
}

async fn lazy_driver(id: &str, dir: &std::path::Path) -> RpcDriver {
    driver_started(id, idle_fixture(dir), ProcessStart::OnFirstCall).await
}

async fn ping(driver: &RpcDriver) -> Result<Value, String> {
    tokio::time::timeout(LIMIT, driver.process.call("ping", json!({})))
        .await
        .expect("no answer in time")
}

async fn wait_for_stop(driver: &RpcDriver) {
    let stopped = wait_until(LIMIT, || driver.process.pid().is_none()).await;
    assert!(stopped, "the idle process was not stopped in time");
}

async fn exits_soon(pid: u32) -> bool {
    wait_until(LIMIT, || has_exited(pid)).await
}

#[tokio::test]
async fn idle_process_is_stopped_and_the_next_call_starts_a_new_one_after_initialize() {
    // Catches an idle process that is never stopped, one that is not
    // restarted, and a restart that hands the caller an uninitialized plugin.
    // given
    let id = "idle-stop-restart";
    let _idle = idle_stop::test_support::force(id, Some(IDLE));
    let dir = tempfile::tempdir().unwrap();
    let driver = lazy_driver(id, dir.path()).await;
    assert_eq!(ping(&driver).await.unwrap(), json!("pong"));
    let first_pid = driver
        .process
        .pid()
        .expect("the first call starts the child");

    // when
    wait_for_stop(&driver).await;

    // then
    assert!(exits_soon(first_pid).await, "the idle child kept running");
    assert_eq!(
        events(dir.path()),
        ["start", "initialize", "ping"],
        "nothing but the first call reached the plugin"
    );

    // when
    let answer = ping(&driver).await;

    // then
    assert_eq!(answer.unwrap(), json!("pong"));
    let second_pid = driver
        .process
        .pid()
        .expect("the next call restarts the child");
    assert_ne!(first_pid, second_pid);
    assert_eq!(
        events(dir.path()),
        ["start", "initialize", "ping", "start", "initialize", "ping"],
        "the restarted child must be initialized before the call reaches it"
    );
    driver.terminate().await;
}

#[tokio::test]
async fn process_stops_again_after_each_restart() {
    // Catches a restart that leaves the task without its idle timer.
    // given
    let id = "idle-stop-repeat";
    let _idle = idle_stop::test_support::force(id, Some(IDLE));
    let dir = tempfile::tempdir().unwrap();
    let driver = lazy_driver(id, dir.path()).await;

    for _ in 0..3 {
        // when
        assert_eq!(ping(&driver).await.unwrap(), json!("pong"));
        let pid = driver.process.pid().expect("running child");

        // then
        wait_for_stop(&driver).await;
        assert!(exits_soon(pid).await);
    }
    let starts = events(dir.path()).iter().filter(|e| *e == "start").count();
    assert_eq!(starts, 3);
    driver.terminate().await;
}

#[tokio::test]
async fn process_with_a_call_in_flight_is_not_stopped() {
    // Catches stopping a child that is still computing an answer.
    // given
    let id = "idle-stop-in-flight";
    let _idle = idle_stop::test_support::force(id, Some(IDLE));
    let dir = tempfile::tempdir().unwrap();
    let driver = Arc::new(lazy_driver(id, dir.path()).await);
    assert_eq!(ping(&driver).await.unwrap(), json!("pong"));
    let pid = driver.process.pid().expect("running child");

    // when
    let slow = tokio::spawn({
        let driver = driver.clone();
        async move { driver.process.call("slow", json!({})).await }
    });
    tokio::time::sleep(IDLE * 2 + Duration::from_millis(100)).await;

    // then
    assert_eq!(
        driver.process.pid(),
        Some(pid),
        "stopped under a running call"
    );
    assert!(!has_exited(pid));
    assert_eq!(slow.await.unwrap().unwrap(), json!("pong"));
    assert_eq!(
        events(dir.path()).iter().filter(|e| *e == "start").count(),
        1
    );
    driver.terminate().await;
}

#[tokio::test]
async fn process_with_a_hold_is_kept_until_the_hold_is_dropped() {
    // Catches stopping a child that owns an open service session.
    // given
    let id = "idle-stop-held";
    let _idle = idle_stop::test_support::force(id, Some(IDLE));
    let dir = tempfile::tempdir().unwrap();
    let driver = lazy_driver(id, dir.path()).await;
    assert_eq!(ping(&driver).await.unwrap(), json!("pong"));
    let pid = driver.process.pid().expect("running child");
    let hold = idle_stop::hold(id);

    // when
    tokio::time::sleep(IDLE * 4).await;

    // then
    assert_eq!(driver.process.pid(), Some(pid), "stopped under a hold");
    assert!(!has_exited(pid));

    // when
    drop(hold);

    // then
    wait_for_stop(&driver).await;
    assert!(exits_soon(pid).await);
    driver.terminate().await;
}

#[tokio::test]
async fn process_kept_running_after_a_sessionless_write_is_never_stopped() {
    // Catches stopping a child that may hold a transaction a legacy write opened.
    // given
    let id = "idle-stop-kept";
    let _idle = idle_stop::test_support::force(id, Some(IDLE));
    let dir = tempfile::tempdir().unwrap();
    let driver = lazy_driver(id, dir.path()).await;
    assert_eq!(ping(&driver).await.unwrap(), json!("pong"));
    let pid = driver.process.pid().expect("running child");

    // when
    idle_stop::keep_running(id);
    tokio::time::sleep(IDLE * 4).await;

    // then
    assert_eq!(
        driver.process.pid(),
        Some(pid),
        "stopped after a sessionless write"
    );
    assert!(!has_exited(pid));
    driver.terminate().await;
}

#[tokio::test]
async fn eager_process_is_never_stopped_for_being_idle() {
    // Catches the desktop app and `tabularis web` losing their always-on drivers.
    // given
    let id = "idle-stop-eager";
    let _idle = idle_stop::test_support::force(id, Some(Duration::from_millis(200)));
    let dir = tempfile::tempdir().unwrap();
    let driver = driver_started(id, idle_fixture(dir.path()), ProcessStart::Eager).await;
    let pid = driver.process.pid().expect("an eager driver has a pid");
    assert_eq!(ping(&driver).await.unwrap(), json!("pong"));

    // when
    tokio::time::sleep(Duration::from_millis(1200)).await;

    // then
    assert_eq!(driver.process.pid(), Some(pid));
    assert!(!has_exited(pid));
    assert_eq!(ping(&driver).await.unwrap(), json!("pong"));
    assert_eq!(
        events(dir.path()).iter().filter(|e| *e == "start").count(),
        1
    );
    driver.terminate().await;
}

#[tokio::test]
async fn lazy_process_is_kept_when_the_idle_stop_is_disabled() {
    // Catches an idle stop that is on by default.
    // given
    let id = "idle-stop-disabled";
    let _idle = idle_stop::test_support::force(id, None);
    let dir = tempfile::tempdir().unwrap();
    let driver = lazy_driver(id, dir.path()).await;
    assert_eq!(ping(&driver).await.unwrap(), json!("pong"));
    let pid = driver.process.pid().expect("running child");

    // when
    tokio::time::sleep(Duration::from_millis(1200)).await;

    // then
    assert_eq!(driver.process.pid(), Some(pid));
    assert!(!has_exited(pid));
    driver.terminate().await;
}

#[tokio::test]
async fn terminate_while_stopped_ends_cleanly_and_starts_nothing() {
    // Catches a task that hangs, or starts a child, when it is shut down
    // while it waits for the next call.
    // given
    let id = "idle-stop-terminate";
    let _idle = idle_stop::test_support::force(id, Some(IDLE));
    let dir = tempfile::tempdir().unwrap();
    let driver = lazy_driver(id, dir.path()).await;
    assert_eq!(ping(&driver).await.unwrap(), json!("pong"));
    wait_for_stop(&driver).await;

    // when
    let stopped = tokio::time::timeout(Duration::from_secs(3), driver.terminate()).await;

    // then
    assert!(stopped.is_ok(), "terminate of a stopped driver hung");
    assert_eq!(driver.process.pid(), None);
    assert!(driver.process.call("ping", json!({})).await.is_err());
    assert_eq!(
        events(dir.path()).iter().filter(|e| *e == "start").count(),
        1
    );
}

#[tokio::test]
async fn shutdown_while_stopped_ends_cleanly() {
    // Catches the graceful path waiting for a child that no longer exists.
    // given
    let id = "idle-stop-shutdown";
    let _idle = idle_stop::test_support::force(id, Some(IDLE));
    let dir = tempfile::tempdir().unwrap();
    let driver = lazy_driver(id, dir.path()).await;
    assert_eq!(ping(&driver).await.unwrap(), json!("pong"));
    wait_for_stop(&driver).await;

    // when
    let stopped = tokio::time::timeout(Duration::from_secs(3), driver.shutdown()).await;

    // then
    assert!(stopped.is_ok(), "shutdown of a stopped driver hung");
    assert_eq!(
        events(dir.path()).iter().filter(|e| *e == "start").count(),
        1
    );
}

#[tokio::test]
async fn failed_initialize_after_a_restart_fails_the_call_and_the_next_call_retries() {
    // Catches handing a call to a child that refused to initialize, and a
    // restart that cannot be retried.
    // given
    let id = "idle-stop-reinit-fails";
    let _idle = idle_stop::test_support::force(id, Some(IDLE));
    let dir = tempfile::tempdir().unwrap();
    let driver = lazy_driver(id, dir.path()).await;
    assert_eq!(ping(&driver).await.unwrap(), json!("pong"));
    wait_for_stop(&driver).await;
    std::fs::write(dir.path().join("fail_initialize"), "").unwrap();

    // when
    let refused = ping(&driver).await;

    // then
    let error = refused.expect_err("the call must fail when the child refuses to initialize");
    assert!(
        error.contains("could not be restarted"),
        "unexpected error: {error}"
    );
    assert_eq!(
        events(dir.path()).last().map(String::as_str),
        Some("initialize"),
        "the call must not reach a child that is not initialized"
    );
    wait_for_stop(&driver).await;

    // when
    std::fs::remove_file(dir.path().join("fail_initialize")).unwrap();
    let retried = ping(&driver).await;

    // then
    assert_eq!(retried.unwrap(), json!("pong"));
    assert_eq!(events(dir.path()).last().map(String::as_str), Some("ping"));
    driver.terminate().await;
}
