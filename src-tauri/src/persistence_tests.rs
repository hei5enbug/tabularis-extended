#[cfg(test)]
mod tests {
    use crate::persistence::{connections_lock_path, load_connections_file, save_connections_file};

    #[test]
    fn save_preserves_unknown_fields_without_resurrecting_known_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.json");
        std::fs::write(
            &path,
            r#"{"schemaVersion":3,"connections":[{"id":"conn-1","name":"prod","environment":"production","readReplicaLag":42,"params":{"driver":"mysql","host":"old","database":"testdb","awsRegion":"eu-west-1","connectionUri":"mysql://x"}},{"id":"conn-2","name":"delete","params":{"driver":"mysql","database":"testdb","awsRegion":"us-east-1"}}]}"#,
        )
        .unwrap();

        let mut file = load_connections_file(&path).unwrap();
        file.connections
            .retain(|connection| connection.id != "conn-2");
        file.connections[0].environment = None;
        file.connections[0].params.host = Some("new".to_string());
        save_connections_file(&path, &file).unwrap();

        let saved: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let connection = &saved["connections"][0];
        assert_eq!(saved["schemaVersion"], 3);
        assert_eq!(connection["readReplicaLag"], 42);
        assert_eq!(connection["params"]["awsRegion"], "eu-west-1");
        assert_eq!(connection["params"]["host"], "new");
        assert!(connection.get("environment").is_none());
        assert_eq!(saved["connections"].as_array().unwrap().len(), 1);
        assert!(connection["params"].get("connectionUri").is_none());
        assert!(serde_json::from_value::<crate::models::ConnectionsFile>(saved).is_ok());
    }

    #[test]
    fn 연결_수정은_revision을_갱신하고_오래된_파일_저장을_거부한다() {
        // given
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile/connections.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"connections":[{"id":"conn-1","name":"before","params":{"driver":"mysql","database":"app"}}]}"#,
        )
        .unwrap();
        let mut current = load_connections_file(&path).unwrap();
        let mut stale = current.clone();
        let previous_revision = current.connections[0].revision.clone();
        assert!(!previous_revision.is_empty());

        // when
        current.connections[0].name = "after".to_string();
        save_connections_file(&path, &current).unwrap();
        stale.connections[0].params.host = Some("stale-write".to_string());
        let stale_result = save_connections_file(&path, &stale);

        // then
        let persisted = load_connections_file(&path).unwrap();
        assert!(stale_result
            .unwrap_err()
            .contains("Connection file revision conflict"));
        assert_eq!(persisted.connections[0].name, "after");
        assert_eq!(persisted.connections[0].params.host, None);
        assert_ne!(persisted.connections[0].revision, previous_revision);
    }

    #[test]
    fn 일반과_dev_연결파일은_프로필_바깥의_같은_잠금을_사용한다() {
        // given
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().join("profile");
        let normal = profile.join("connections.json");
        let dev = profile.join("connections.dev.json");

        // when
        let normal_lock = connections_lock_path(&normal).unwrap();
        let dev_lock = connections_lock_path(&dev).unwrap();

        // then
        assert_eq!(normal_lock, dev_lock);
        assert_eq!(normal_lock.parent(), Some(dir.path()));
    }
}
