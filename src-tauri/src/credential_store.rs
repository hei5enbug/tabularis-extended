use keyring::Entry;
use serde::Serialize;

pub trait CredentialStore: Send + Sync {
    fn get(&self, service: &str, account: &str) -> Result<Option<String>, String>;
    fn set(&self, service: &str, account: &str, value: &str) -> Result<(), String>;
    fn delete(&self, service: &str, account: &str) -> Result<(), String>;
}

#[derive(Default)]
pub struct KeyringCredentialStore;

impl CredentialStore for KeyringCredentialStore {
    fn get(&self, service: &str, account: &str) -> Result<Option<String>, String> {
        let entry = Entry::new(service, account).map_err(|error| error.to_string())?;
        match entry.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }

    fn set(&self, service: &str, account: &str, value: &str) -> Result<(), String> {
        Entry::new(service, account)
            .and_then(|entry| entry.set_password(value))
            .map_err(|error| error.to_string())
    }

    fn delete(&self, service: &str, account: &str) -> Result<(), String> {
        match Entry::new(service, account).and_then(|entry| entry.delete_credential()) {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }
}

#[derive(Clone)]
pub struct CredentialChange {
    pub account: String,
    pub value: Option<String>,
}

#[derive(Clone)]
pub struct CredentialUndo {
    service: String,
    previous: Vec<(String, Option<String>)>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum CredentialTransactionError {
    Failed { message: String },
    RecoveryRequired { message: String },
}

pub fn apply_changes(
    store: &dyn CredentialStore,
    service: &str,
    changes: &[CredentialChange],
) -> Result<CredentialUndo, CredentialTransactionError> {
    let mut previous = Vec::with_capacity(changes.len());
    for change in changes {
        let old_value = store
            .get(service, &change.account)
            .map_err(|message| CredentialTransactionError::Failed { message })?;
        previous.push((change.account.clone(), old_value));
    }

    for (index, change) in changes.iter().enumerate() {
        let result = match change.value.as_deref() {
            Some(value) => store.set(service, &change.account, value),
            None => store.delete(service, &change.account),
        };
        if let Err(error) = result {
            let undo = CredentialUndo {
                service: service.to_string(),
                previous: previous[..=index].to_vec(),
            };
            if let Err(rollback) = rollback_changes(store, &undo) {
                return Err(CredentialTransactionError::RecoveryRequired {
                    message: format!("{error}; credential rollback failed: {rollback}"),
                });
            }
            return Err(CredentialTransactionError::Failed { message: error });
        }
    }

    Ok(CredentialUndo {
        service: service.to_string(),
        previous,
    })
}

pub fn rollback_changes(store: &dyn CredentialStore, undo: &CredentialUndo) -> Result<(), String> {
    let mut failures = Vec::new();
    for (account, previous) in undo.previous.iter().rev() {
        let result = match previous.as_deref() {
            Some(value) => store.set(&undo.service, account, value),
            None => store.delete(&undo.service, account),
        };
        if let Err(error) = result {
            failures.push(format!("{account}: {error}"));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeStore {
        values: Mutex<HashMap<(String, String), String>>,
        fail_set_for: Mutex<Option<String>>,
    }

    impl CredentialStore for FakeStore {
        fn get(&self, service: &str, account: &str) -> Result<Option<String>, String> {
            Ok(self
                .values
                .lock()
                .unwrap()
                .get(&(service.to_string(), account.to_string()))
                .cloned())
        }

        fn set(&self, service: &str, account: &str, value: &str) -> Result<(), String> {
            if self.fail_set_for.lock().unwrap().as_deref() == Some(account) {
                return Err("fixture write failure".to_string());
            }
            self.values.lock().unwrap().insert(
                (service.to_string(), account.to_string()),
                value.to_string(),
            );
            Ok(())
        }

        fn delete(&self, service: &str, account: &str) -> Result<(), String> {
            self.values
                .lock()
                .unwrap()
                .remove(&(service.to_string(), account.to_string()));
            Ok(())
        }
    }

    #[test]
    fn 파일_저장_실패_후_이전_자격증명을_복구한다() {
        // given
        let store = FakeStore::default();
        store.set("fork", "db:1", "before").unwrap();
        let changes = [CredentialChange {
            account: "db:1".to_string(),
            value: Some("after".to_string()),
        }];

        // when
        let undo = apply_changes(&store, "fork", &changes).unwrap();
        rollback_changes(&store, &undo).expect("가짜 키체인 값 복구");

        // then
        assert_eq!(
            store.get("fork", "db:1").unwrap().as_deref(),
            Some("before")
        );
    }

    #[test]
    fn 자격증명_쓰기_실패_시_기존_값을_유지한다() {
        // given
        let store = FakeStore::default();
        store.set("fork", "first", "before").unwrap();
        *store.fail_set_for.lock().unwrap() = Some("second".to_string());
        let changes = [
            CredentialChange {
                account: "first".to_string(),
                value: Some("updated".to_string()),
            },
            CredentialChange {
                account: "second".to_string(),
                value: Some("new".to_string()),
            },
        ];

        // when
        let result = apply_changes(&store, "fork", &changes);

        // then
        assert!(matches!(
            result,
            Err(CredentialTransactionError::Failed { .. })
        ));
        assert_eq!(
            store.get("fork", "first").unwrap().as_deref(),
            Some("before")
        );
        assert_eq!(store.get("fork", "second").unwrap(), None);
    }
}
