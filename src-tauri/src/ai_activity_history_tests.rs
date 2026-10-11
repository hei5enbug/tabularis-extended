use std::collections::HashSet;
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use tempfile::TempDir;

use super::{
    activity_path_in, append_and_rotate_in, read_query_history_events_in, rotated_path_in,
    AiActivityEvent, MAX_QUERY_HISTORY_LINE_BYTES, MAX_ROTATED_FILES,
};

const CHILD_DIR_ENV: &str = "TABULARIS_AI_HISTORY_TEST_DIR";
const CHILD_WORKER_ENV: &str = "TABULARIS_AI_HISTORY_TEST_WORKER";
const CHILD_MODE_ENV: &str = "TABULARIS_AI_HISTORY_TEST_MODE";
const CHILD_TEST_FILTER: &str =
    "ai_activity::history_tests::child_테스트_프로세스에서_이벤트를_기록한다";

fn synthetic_event(id: &str, session_id: &str) -> AiActivityEvent {
    AiActivityEvent {
        id: id.to_string(),
        session_id: session_id.to_string(),
        timestamp: "2026-10-11T00:00:00Z".to_string(),
        tool: "start_query".to_string(),
        connection_id: Some("synthetic-connection".to_string()),
        connection_name: Some("synthetic connection".to_string()),
        query: Some("SELECT synthetic_value".to_string()),
        query_kind: Some("select".to_string()),
        duration_ms: 7,
        status: "succeeded".to_string(),
        rows: Some(1),
        error: None,
        client_hint: None,
        approval_id: None,
        query_id: Some(id.to_string()),
        event_phase: Some("terminal".to_string()),
    }
}

fn spawn_append_workers(dir: &Path, mode: &str, worker_ids: &[&str]) -> Vec<Child> {
    let executable = std::env::current_exe().expect("the test executable path is available");
    worker_ids
        .iter()
        .map(|worker_id| {
            Command::new(&executable)
                .arg("--exact")
                .arg(CHILD_TEST_FILTER)
                .arg("--nocapture")
                .env(CHILD_DIR_ENV, dir)
                .env(CHILD_WORKER_ENV, worker_id)
                .env(CHILD_MODE_ENV, mode)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("child test process starts")
        })
        .collect()
}

fn wait_for_workers(worker_ids: &[&str], children: Vec<Child>) {
    assert_eq!(worker_ids.len(), children.len());
    for (worker_id, child) in worker_ids.iter().zip(children) {
        let output = child
            .wait_with_output()
            .expect("child test process completes");
        assert!(
            output.status.success(),
            "writer {worker_id} failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn given_두_프로세스가_같은_로그에_쓸_때_when_잠금_아래_추가하면_then_80개_고유_이벤트가_남는다() {
    // Given: two independent test processes target the same isolated log directory.
    let temp = TempDir::new().expect("temporary log directory is created");
    let workers = ["append-worker-a", "append-worker-b"];

    // When: both processes append 40 records through the real cross-process file lock.
    let children = spawn_append_workers(temp.path(), "no-rotation", &workers);
    wait_for_workers(&workers, children);

    // Then: all records are complete, deserializable, and unique.
    let events = read_query_history_events_in(temp.path(), 1_000)
        .expect("the bounded reader accepts the complete log");
    assert_eq!(events.len(), 80);
    let ids = events
        .iter()
        .map(|event| event.id.as_str())
        .collect::<HashSet<_>>();
    assert_eq!(ids.len(), 80);
    assert!(events.iter().all(|event| event.query_id.is_some()));
}

#[test]
fn given_동시_기록이_회전_임계값을_넘을_때_when_다섯_보관파일을_순환하면_then_모든_기록이_온전하고_한도_이내다(
) {
    // Given: two processes will write 120 unique records with a 20-line rotation threshold.
    let temp = TempDir::new().expect("temporary log directory is created");
    let workers = ["rotation-worker-a", "rotation-worker-b"];

    // When: both processes append concurrently and force six complete rotation batches.
    let children = spawn_append_workers(temp.path(), "rotate", &workers);
    wait_for_workers(&workers, children);

    // Then: no more than five archives remain and every retained line is valid JSONL.
    let mut events = Vec::new();
    let active = activity_path_in(temp.path());
    let paths = std::iter::once(active)
        .chain((1..=MAX_ROTATED_FILES).map(|index| rotated_path_in(temp.path(), index)))
        .collect::<Vec<_>>();
    let mut present_archives = 0usize;
    for path in paths {
        if !path.exists() {
            continue;
        }
        if path != activity_path_in(temp.path()) {
            present_archives += 1;
        }
        let contents = fs::read_to_string(&path).expect("retained log file is readable");
        assert!(!contents.is_empty(), "retained files contain whole records");
        let mut line_count = 0usize;
        for line in contents.lines() {
            assert!(
                !line.is_empty(),
                "rotation never leaves partial blank lines"
            );
            let event = serde_json::from_str::<AiActivityEvent>(line)
                .expect("every retained JSONL line is a complete event");
            events.push(event);
            line_count += 1;
        }
        assert!(
            line_count <= 20,
            "rotation batch cannot exceed its threshold"
        );
    }
    assert_eq!(present_archives, MAX_ROTATED_FILES);
    assert_eq!(events.len(), 100, "the five newest rotation batches remain");
    assert!(events.len() <= MAX_ROTATED_FILES * 20 + 19);
    let ids = events
        .iter()
        .map(|event| event.id.as_str())
        .collect::<HashSet<_>>();
    assert_eq!(ids.len(), events.len());
}

#[test]
fn given_로그에_손상행과_초과행이_있을_때_when_제한_읽기하면_then_다음_유효행과_레코드_한도를_지킨다(
) {
    // Given: active and rotated files contain valid records around malformed and oversized lines.
    let temp = TempDir::new().expect("temporary log directory is created");
    let active_path = activity_path_in(temp.path());
    let mut active = File::create(&active_path).expect("active log is created");
    write_json_line(&mut active, &synthetic_event("active-first", "active"));
    let mut oversized = serde_json::to_value(synthetic_event("oversized-valid", "active"))
        .expect("synthetic event becomes JSON");
    oversized["padding"] = serde_json::json!("x".repeat(1024 * 1024));
    let oversized = serde_json::to_vec(&oversized).expect("oversized event serializes");
    assert!(oversized.len() > MAX_QUERY_HISTORY_LINE_BYTES);
    active
        .write_all(&oversized)
        .expect("oversized line is written");
    active
        .write_all(b"\nnot-json\n")
        .expect("corrupt line is written");
    write_json_line(
        &mut active,
        &synthetic_event("active-after-corruption", "active"),
    );
    write_json_line(
        &mut active,
        &synthetic_event("active-after-bound", "active"),
    );
    drop(active);

    let mut rotated =
        File::create(rotated_path_in(temp.path(), 1)).expect("one rotated history file is created");
    write_json_line(&mut rotated, &synthetic_event("rotated-valid", "rotated"));

    // When: the scan budget ends after four records, then a larger budget reads both files.
    let bounded =
        read_query_history_events_in(temp.path(), 4).expect("bounded scan skips corrupt content");
    let complete = read_query_history_events_in(temp.path(), 10)
        .expect("bounded reader continues into the rotated file");

    // Then: corrupt lines consume scan budget, the next valid event survives, and later files are bounded.
    assert_eq!(
        bounded
            .iter()
            .map(|event| event.id.as_str())
            .collect::<Vec<_>>(),
        vec!["active-first", "active-after-corruption"]
    );
    assert_eq!(
        complete
            .iter()
            .map(|event| event.id.as_str())
            .collect::<Vec<_>>(),
        vec![
            "active-first",
            "active-after-corruption",
            "active-after-bound",
            "rotated-valid"
        ]
    );
    assert!(MAX_QUERY_HISTORY_LINE_BYTES < oversized.len());
}

#[test]
fn given_기존_이벤트_json에_새_필드가_없을_때_when_역직렬화_후_직렬화하면_then_기본값과_생략을_유지한다(
) {
    // Given: a legacy event payload from before managed-query fields were added.
    let legacy = serde_json::json!({
        "id": "legacy-event",
        "sessionId": "legacy-session",
        "timestamp": "2026-10-10T12:00:00Z",
        "tool": "run_query",
        "connectionId": "legacy-connection",
        "connectionName": "legacy connection",
        "query": "SELECT 1",
        "queryKind": "select",
        "durationMs": 5,
        "status": "success",
        "rows": 1,
        "error": null,
        "clientHint": null,
        "approvalId": null
    });

    // When: it is read into the current event shape and serialized again.
    let event: AiActivityEvent =
        serde_json::from_value(legacy).expect("legacy event remains readable");
    let encoded = serde_json::to_value(&event).expect("current event serializes");
    let object = encoded.as_object().expect("event serializes as an object");

    // Then: missing fields default to None and remain omitted from the output.
    assert_eq!(event.query_id, None);
    assert_eq!(event.event_phase, None);
    assert!(!object.contains_key("queryId"));
    assert!(!object.contains_key("eventPhase"));
}

#[test]
fn child_테스트_프로세스에서_이벤트를_기록한다() {
    // Given: a child invocation is marked by its isolated test-only environment.
    let (Some(dir), Some(worker), Some(mode)) = (
        std::env::var_os(CHILD_DIR_ENV),
        std::env::var_os(CHILD_WORKER_ENV),
        std::env::var_os(CHILD_MODE_ENV),
    ) else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    let worker = worker.to_string_lossy();
    let mode = mode.to_string_lossy();
    let (event_count, max_entries) = match mode.as_ref() {
        "no-rotation" => (40, 1_000),
        "rotate" => (60, 20),
        other => panic!("unknown synthetic writer mode: {other}"),
    };

    // When: the child writes its uniquely identified events using the production lock path.
    for index in 0..event_count {
        let event = synthetic_event(&format!("{worker}-{index:03}"), "process-writer");
        append_and_rotate_in(&dir, &event, max_entries).expect("append and rotation succeed");
    }

    // Then: successful exit confirms all requested writes completed.
}

fn write_json_line(file: &mut File, event: &AiActivityEvent) {
    serde_json::to_writer(&mut *file, event).expect("synthetic event serializes");
    file.write_all(b"\n")
        .expect("JSONL line terminator is written");
}
