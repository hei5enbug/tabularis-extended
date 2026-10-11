use super::*;

/// Spawns a plugin that never answers and records every stdin line in a file.
/// fd 3 keeps the stdout pipe open, otherwise the host sees EOF and treats
/// the plugin as exited.
#[cfg(unix)]
async fn silent_recording_plugin(dir: &std::path::Path) -> (PluginProcess, PathBuf) {
    let log = dir.join("stdin.log");
    let script = dir.join("plugin.sh");
    std::fs::write(&script, format!("exec 3>&1 cat > '{}'\n", log.display())).unwrap();
    let process = PluginProcess::new("test-plugin".to_string(), script, Some("/bin/sh".into()))
        .await
        .expect("spawn sh");
    (process, log)
}

#[cfg(unix)]
async fn wait_for_lines(log: &std::path::Path, count: usize, within: Duration) -> Vec<Value> {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        let lines: Vec<Value> = std::fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        if lines.len() >= count || tokio::time::Instant::now() >= deadline {
            return lines;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[cfg(unix)]
#[tokio::test]
async fn timed_out_call_sends_a_cancel_notification_for_its_id() {
    let dir = tempfile::tempdir().unwrap();
    let (process, log) = silent_recording_plugin(dir.path()).await;

    let error = process
        .send_request(
            "execute_query",
            json!({ "query": "SELECT pg_sleep(330)" }),
            Some(Duration::from_millis(100)),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("timed out"));

    let lines = wait_for_lines(&log, 2, Duration::from_secs(5)).await;
    process.shutdown().await;

    assert_eq!(lines.len(), 2, "expected request + cancel, got {lines:?}");
    assert_eq!(lines[0]["method"], "execute_query");
    let id = lines[0]["id"].as_u64().unwrap();
    assert_eq!(
        lines[1],
        json!({ "jsonrpc": "2.0", "method": "cancel", "params": { "id": id } })
    );
}

#[cfg(unix)]
#[tokio::test]
async fn 주어진_시간제한이없는호출은_호출자가중단할때취소한다() {
    let dir = tempfile::tempdir().unwrap();
    let (process, log) = silent_recording_plugin(dir.path()).await;
    let process = Arc::new(process);

    let call = tokio::spawn({
        let process = process.clone();
        async move {
            process
                .send_request("execute_query", json!({ "query": "SELECT 1" }), None)
                .await
        }
    });

    let lines = wait_for_lines(&log, 1, Duration::from_secs(5)).await;
    let lines_later = wait_for_lines(&log, 2, Duration::from_millis(300)).await;
    assert!(!call.is_finished(), "an unlimited call must keep waiting");
    assert_eq!(
        lines_later.len(),
        1,
        "no cancel expected before caller drop"
    );
    call.abort();
    let lines_after_drop = wait_for_lines(&log, 2, Duration::from_secs(5)).await;
    process.shutdown().await;

    assert_eq!(lines.len(), 1);
    assert_eq!(lines_after_drop.len(), 2, "drop should send a cancel");
    let id = lines_after_drop[0]["id"].as_u64().unwrap();
    assert_eq!(
        lines_after_drop[1],
        json!({ "jsonrpc": "2.0", "method": "cancel", "params": { "id": id } })
    );
}

#[cfg(unix)]
#[tokio::test]
async fn 주어진_취소가_호스트전송보다먼저처리되면_실행요청을보내지않는다() {
    let dir = tempfile::tempdir().unwrap();
    let (process, log) = silent_recording_plugin(dir.path()).await;
    let (processed, acknowledged) = oneshot::channel();
    process
        .cancel_sender
        .send(PluginCancel {
            id: 1,
            generation: process.generation,
            processed: Some(processed),
        })
        .unwrap();
    acknowledged.await.unwrap();

    let error = process
        .send_request(
            "execute_query",
            json!({ "query": "SELECT 1" }),
            Some(Duration::from_secs(2)),
        )
        .await
        .unwrap_err();
    let lines = wait_for_lines(&log, 1, Duration::from_millis(250)).await;
    process.shutdown().await;

    assert_eq!(error.to_string(), "REQUEST_CANCELLED_BEFORE_DISPATCH");
    assert!(
        lines.is_empty(),
        "the plugin received an unexpected request"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn 주어진_선행취소로_managed요청이생략되면_not_dispatched를반환한다() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("stdin.log");
    let script = dir.path().join("plugin.sh");
    std::fs::write(&script, format!("exec 3>&1 cat > '{}'\n", log.display())).unwrap();
    let process = PluginProcess::new("postgresql".to_string(), script, Some("/bin/sh".into()))
        .await
        .expect("spawn fake plugin");
    let (processed, acknowledged) = oneshot::channel();
    process
        .cancel_sender
        .send(PluginCancel {
            id: 1,
            generation: process.generation,
            processed: Some(processed),
        })
        .unwrap();
    acknowledged.await.unwrap();

    let mut manifest = test_manifest();
    manifest.id = "postgresql".to_string();
    manifest.capabilities.sql_dialect = Some(crate::drivers::driver_trait::SqlDialect::Postgres);
    let driver = RpcDriver {
        manifest,
        process: Arc::new(process),
        data_types: Vec::new(),
        connection_metadata: false,
        metadata_cache: Arc::new(ConnectionMetadataCache::default()),
        connection_params: None,
    };
    let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    let handle = driver
        .start_managed_query(
            &ConnectionParams::default(),
            "SELECT 1",
            1,
            None,
            cancel_rx,
            tokio::time::Instant::now() + Duration::from_secs(2),
        )
        .await
        .expect("request is queued to the process actor");
    let outcome = handle.result.await;
    let lines = wait_for_lines(&log, 1, Duration::from_millis(250)).await;
    driver.process.shutdown().await;

    assert!(matches!(outcome, Err(ManagedQueryFailure::NotDispatched)));
    assert!(
        lines.is_empty(),
        "managed SQL reached the plugin before cancel"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn 주어진_응답줄이_일부만도착한동안_새요청이오면_첫응답을보존한다() {
    let dir = tempfile::tempdir().unwrap();
    let ready = dir.path().join("partial-ready");
    let script = dir.path().join("plugin.sh");
    std::fs::write(
        &script,
        format!(
            r#"exec 3>&1
count=0
while IFS= read -r line; do
  count=$((count + 1))
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  if [ "$count" -eq 1 ]; then
    printf '{{"jsonrpc":"2.0","id":%s,"result":{{"value":' "$id"
    printf ready > '{}'
    sleep 0.2
    printf '1}}}}\n'
  else
    printf '{{"jsonrpc":"2.0","id":%s,"result":{{"value":2}}}}\n' "$id"
  fi
done
"#,
            ready.display()
        ),
    )
    .unwrap();
    let process = Arc::new(
        PluginProcess::new("test-plugin".to_string(), script, Some("/bin/sh".into()))
            .await
            .expect("spawn fake plugin"),
    );

    let first = tokio::spawn({
        let process = process.clone();
        async move { process.send_request("first", json!({}), None).await }
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !ready.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the plugin wrote a partial response");
    let second = tokio::spawn({
        let process = process.clone();
        async move { process.send_request("second", json!({}), None).await }
    });

    let (first_result, second_result) = tokio::join!(first, second);
    process.shutdown().await;

    assert_eq!(first_result.unwrap().unwrap(), json!({ "value": 1 }));
    assert_eq!(second_result.unwrap().unwrap(), json!({ "value": 2 }));
}

/// Timed-out callers must leave the pending request and its admission permit
/// alive until the plugin returns the original terminal response.
#[cfg(unix)]
#[tokio::test]
async fn 주어진_postgresql_호출이_취소된뒤_원응답전까지_세_슬롯을_유지한다() {
    let dir = tempfile::tempdir().unwrap();
    let release = dir.path().join("release");
    let log = dir.path().join("stdin.log");
    let script = dir.path().join("plugin.sh");
    std::fs::write(
        &script,
        format!(
            r#"exec 3>&1
while IFS= read -r line; do
  printf '%s\n' "$line" >> '{}'
  case "$line" in
    *'"method":"cancel"'*)
      id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
      while [ ! -f '{}' ]; do sleep 0.01; done
      printf '{{"jsonrpc":"2.0","id":%s,"result":{{}}}}\n' "$id"
      ;;
  esac
done
"#,
            log.display(),
            release.display()
        ),
    )
    .unwrap();
    let process = Arc::new(
        PluginProcess::new("postgresql".to_string(), script, Some("/bin/sh".into()))
            .await
            .expect("spawn fake plugin"),
    );

    let calls = (0..3)
        .map(|_| {
            let process = process.clone();
            tokio::spawn(async move {
                process
                    .send_request(
                        "execute_query",
                        json!({ "query": "SELECT 1" }),
                        Some(Duration::from_millis(200)),
                    )
                    .await
            })
        })
        .collect::<Vec<_>>();
    for call in calls {
        assert!(call.await.unwrap().is_err());
    }

    let slots = process.ordinary_slots.as_ref().unwrap();
    assert_eq!(slots.available_permits(), 0);

    std::fs::write(&release, "release").unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while slots.available_permits() != 3 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("original terminal responses release all permits");
    process.shutdown().await;
}
