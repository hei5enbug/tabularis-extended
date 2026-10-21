//! Safe saves of `connections.json` together with the secrets that belong to
//! its connections.
//!
//! Every operation runs under the profile lock, reloads the file, checks the
//! per-connection revision the caller read, and only then touches the keychain.
//! A keychain failure leaves the file untouched; a file failure restores the
//! previous secrets or reports `CONNECTION_RECOVERY_REQUIRED:<connection id>`.

use crate::models::{
    ConnectionAppearance, ConnectionParams, ConnectionsFile, DatabaseSelection, SavedConnection,
};
use crate::persistence::{self, CONNECTION_CONFLICT};
use std::cell::RefCell;
use std::path::Path;

pub const RECOVERY_REQUIRED_PREFIX: &str = "CONNECTION_RECOVERY_REQUIRED:";

pub fn recovery_required(connection_ids: &[String]) -> String {
    format!("{RECOVERY_REQUIRED_PREFIX}{}", connection_ids.join(","))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SecretKind {
    DbPassword,
    SshPassword,
    SshKeyPassphrase,
    ConnectionUri,
}

pub trait SecretBackend {
    fn get(&self, connection_id: &str, kind: SecretKind) -> Result<Option<String>, String>;
    fn set(&self, connection_id: &str, kind: SecretKind, value: &str) -> Result<(), String>;
    fn delete(&self, connection_id: &str, kind: SecretKind) -> Result<(), String>;
}

pub struct KeychainBackend;

impl SecretBackend for KeychainBackend {
    fn get(&self, connection_id: &str, kind: SecretKind) -> Result<Option<String>, String> {
        let suffix = match kind {
            SecretKind::DbPassword => "db",
            SecretKind::SshPassword => "ssh",
            SecretKind::SshKeyPassphrase => "ssh_passphrase",
            SecretKind::ConnectionUri => "connection_uri",
        };
        crate::keychain_utils::get_secret_optional(&format!("{connection_id}:{suffix}"))
    }
    fn set(&self, connection_id: &str, kind: SecretKind, value: &str) -> Result<(), String> {
        use crate::keychain_utils as keychain;
        match kind {
            SecretKind::DbPassword => keychain::set_db_password(connection_id, value),
            SecretKind::SshPassword => keychain::set_ssh_password(connection_id, value),
            SecretKind::SshKeyPassphrase => keychain::set_ssh_key_passphrase(connection_id, value),
            SecretKind::ConnectionUri => keychain::set_connection_uri(connection_id, value),
        }
    }
    fn delete(&self, connection_id: &str, kind: SecretKind) -> Result<(), String> {
        use crate::keychain_utils as keychain;
        match kind {
            SecretKind::DbPassword => keychain::delete_db_password(connection_id),
            SecretKind::SshPassword => keychain::delete_ssh_password(connection_id),
            SecretKind::SshKeyPassphrase => keychain::delete_ssh_key_passphrase(connection_id),
            SecretKind::ConnectionUri => keychain::delete_connection_uri(connection_id),
        }
    }
}

/// A secret change that took effect, for the caller to mirror into its
/// in-memory credential cache once the whole save succeeded.
#[derive(Clone, Debug)]
pub struct AppliedSecret {
    pub connection_id: String,
    pub kind: SecretKind,
    pub value: Option<String>,
}

pub fn apply_to_cache(cache: &crate::credential_cache::CredentialCache, applied: &[AppliedSecret]) {
    use crate::credential_cache as cc;
    for change in applied {
        let id = change.connection_id.as_str();
        match (change.kind, change.value.as_deref()) {
            (SecretKind::DbPassword, Some(value)) => cc::set_db_password_cached(cache, id, value),
            (SecretKind::DbPassword, None) => cc::invalidate_db_password(cache, id),
            (SecretKind::SshPassword, Some(value)) => cc::set_ssh_password_cached(cache, id, value),
            (SecretKind::SshPassword, None) => cc::invalidate_ssh_password(cache, id),
            (SecretKind::SshKeyPassphrase, Some(value)) => {
                cc::set_ssh_key_passphrase_cached(cache, id, value)
            }
            (SecretKind::SshKeyPassphrase, None) => cc::invalidate_ssh_key_passphrase(cache, id),
            (SecretKind::ConnectionUri, Some(value)) => {
                cc::set_connection_uri_cached(cache, id, value)
            }
            (SecretKind::ConnectionUri, None) => cc::invalidate_connection_uri(cache, id),
        }
    }
}

struct Undo {
    connection_id: String,
    kind: SecretKind,
    previous: Option<String>,
}

/// Records the previous value of every secret it changes so a later failure
/// can put them back.
pub struct SecretJournal<'a> {
    backend: &'a dyn SecretBackend,
    undo: Vec<Undo>,
    applied: Vec<AppliedSecret>,
}

impl<'a> SecretJournal<'a> {
    pub fn new(backend: &'a dyn SecretBackend) -> Self {
        Self {
            backend,
            undo: Vec::new(),
            applied: Vec::new(),
        }
    }

    pub fn set(
        &mut self,
        connection_id: &str,
        kind: SecretKind,
        value: &str,
    ) -> Result<(), String> {
        let previous = self.backend.get(connection_id, kind)?;
        self.backend.set(connection_id, kind, value)?;
        self.record(connection_id, kind, previous, Some(value.to_owned()));
        Ok(())
    }

    /// Removes a secret; a failure aborts the save.
    pub fn delete(&mut self, connection_id: &str, kind: SecretKind) -> Result<(), String> {
        let Some(previous) = self.backend.get(connection_id, kind)? else {
            return Ok(());
        };
        self.backend.delete(connection_id, kind)?;
        self.record(connection_id, kind, Some(previous), None);
        Ok(())
    }

    /// Removes a secret that may not be reachable; a failure is ignored and
    /// the cache entry is dropped either way.
    pub fn delete_best_effort(&mut self, connection_id: &str, kind: SecretKind) {
        let previous = self.backend.get(connection_id, kind).ok().flatten();
        let removed = self.backend.delete(connection_id, kind).is_ok();
        match (removed, previous) {
            (true, previous) if previous.is_some() => {
                self.record(connection_id, kind, previous, None)
            }
            _ => self.applied.push(AppliedSecret {
                connection_id: connection_id.to_owned(),
                kind,
                value: None,
            }),
        }
    }

    fn record(
        &mut self,
        connection_id: &str,
        kind: SecretKind,
        previous: Option<String>,
        value: Option<String>,
    ) {
        self.undo.push(Undo {
            connection_id: connection_id.to_owned(),
            kind,
            previous,
        });
        self.applied.push(AppliedSecret {
            connection_id: connection_id.to_owned(),
            kind,
            value,
        });
    }

    /// Restores every changed secret, newest first. A secret that cannot be
    /// restored is reported by connection id only.
    pub fn rollback(&mut self) -> Result<(), String> {
        let mut failed: Vec<String> = Vec::new();
        while let Some(undo) = self.undo.pop() {
            let restored = match &undo.previous {
                Some(value) => self.backend.set(&undo.connection_id, undo.kind, value),
                None => self.backend.delete(&undo.connection_id, undo.kind),
            };
            if restored.is_err() && !failed.contains(&undo.connection_id) {
                failed.push(undo.connection_id);
            }
        }
        if failed.is_empty() {
            Ok(())
        } else {
            Err(recovery_required(&failed))
        }
    }

    pub fn into_applied(self) -> Vec<AppliedSecret> {
        self.applied
    }
}

/// Locked read-modify-write of `connections.json` with keychain compensation.
pub fn with_secret_journal<T>(
    path: &Path,
    backend: &dyn SecretBackend,
    mutation: impl FnOnce(&mut ConnectionsFile, &mut SecretJournal) -> Result<T, String>,
) -> Result<(T, Vec<AppliedSecret>), String> {
    let journal = RefCell::new(SecretJournal::new(backend));
    let result = persistence::update_connections_file_guarded(
        path,
        |file| mutation(file, &mut journal.borrow_mut()),
        |error| match journal.borrow_mut().rollback() {
            Ok(()) => error,
            Err(recovery) => recovery,
        },
    );
    result.map(|value| (value, journal.into_inner().into_applied()))
}

pub fn require_revision(connection: &SavedConnection, expected: u64) -> Result<(), String> {
    if connection.revision == expected {
        Ok(())
    } else {
        Err(CONNECTION_CONFLICT.to_owned())
    }
}

fn next_revision(connection: &SavedConnection) -> Result<u64, String> {
    connection
        .next_revision()
        .ok_or_else(|| "Connection revision limit reached".to_owned())
}

pub struct ConnectionDraft {
    pub name: String,
    pub params: ConnectionParams,
    pub detect_json_in_text_columns: Option<bool>,
    pub environment: Option<String>,
}

pub fn create_connection(
    path: &Path,
    backend: &dyn SecretBackend,
    draft: ConnectionDraft,
) -> Result<(SavedConnection, Vec<AppliedSecret>), String> {
    crate::commands::validate_connection_uri_persistence(&draft.params)?;
    let id = uuid::Uuid::new_v4().to_string();
    with_secret_journal(path, backend, |file, journal| {
        let params = &draft.params;
        let connection_uri = crate::commands::runtime_connection_uri(params).map(str::to_owned);
        let mut params_to_save =
            crate::commands::params_for_persistence(params, connection_uri.is_some());
        if params.save_in_keychain.unwrap_or(false) {
            if let crate::keychain_utils::StoredPasswordChange::Store(pwd) =
                crate::keychain_utils::stored_password_change(params.password.as_deref())
            {
                journal.set(&id, SecretKind::DbPassword, pwd)?;
            }
            if params.ssh_enabled.unwrap_or(false) {
                if let Some(ssh_pwd) = &params.ssh_password {
                    journal.set(&id, SecretKind::SshPassword, ssh_pwd)?;
                }
                if let Some(passphrase) = &params.ssh_key_passphrase {
                    if !passphrase.trim().is_empty() {
                        journal.set(&id, SecretKind::SshKeyPassphrase, passphrase)?;
                    }
                }
            }
            params_to_save.password = None;
            params_to_save.ssh_password = None;
            params_to_save.ssh_key_passphrase = None;
        }
        if let Some(uri) = &connection_uri {
            journal.set(&id, SecretKind::ConnectionUri, uri)?;
        }
        let connection = SavedConnection {
            id: id.clone(),
            name: draft.name.clone(),
            params: params_to_save,
            group_id: None,
            sort_order: None,
            detect_json_in_text_columns: draft.detect_json_in_text_columns,
            appearance: None,
            tag_ids: None,
            environment: draft.environment.clone(),
            revision: crate::models::INITIAL_REVISION,
        };
        file.connections.push(connection.clone());
        Ok(connection)
    })
}

pub struct ConnectionEdit {
    pub id: String,
    pub name: String,
    pub params: ConnectionParams,
    pub detect_json_in_text_columns: Option<bool>,
    pub environment: Option<String>,
    pub expected_revision: u64,
}

pub struct UpdateOutcome {
    pub connection: SavedConnection,
    pub previous_driver: String,
    pub previous_database: DatabaseSelection,
}

pub fn update_connection(
    path: &Path,
    backend: &dyn SecretBackend,
    edit: ConnectionEdit,
) -> Result<(UpdateOutcome, Vec<AppliedSecret>), String> {
    crate::commands::validate_connection_uri_persistence(&edit.params)?;
    with_secret_journal(path, backend, |file, journal| {
        let id = edit.id.as_str();
        let params = &edit.params;
        let index = file
            .connections
            .iter()
            .position(|connection| connection.id == id)
            .ok_or("Connection not found")?;
        let existing = file.connections[index].clone();
        require_revision(&existing, edit.expected_revision)?;
        let revision = next_revision(&existing)?;

        let existing_uri_in_keychain = existing.params.connection_uri_in_keychain.unwrap_or(false);
        let previous_driver = existing.params.driver.clone();
        let same_driver = previous_driver == params.driver;
        let connection_uri = crate::commands::runtime_connection_uri(params).map(str::to_owned);
        // The frontend sends the URI back only when the user retyped it. An edit
        // that leaves the field untouched keeps the stored secret; one that
        // clears the marker deletes it. A stored URI belongs to the driver that
        // produced it, so switching drivers drops it.
        let preserve_stored_uri = connection_uri.is_none()
            && same_driver
            && params.save_in_keychain.unwrap_or(false)
            && existing_uri_in_keychain
            && params.connection_uri_in_keychain != Some(false);
        let mut params_to_save = crate::commands::params_for_persistence(
            params,
            connection_uri.is_some() || preserve_stored_uri,
        );

        if params.save_in_keychain.unwrap_or(false) {
            match crate::keychain_utils::stored_password_change(params.password.as_deref()) {
                crate::keychain_utils::StoredPasswordChange::Store(pwd) => {
                    journal.set(id, SecretKind::DbPassword, pwd)?
                }
                // An explicit empty password (a plugin hid the login inputs)
                // must not leave the previous secret behind.
                crate::keychain_utils::StoredPasswordChange::Delete => {
                    journal.delete(id, SecretKind::DbPassword)?
                }
                crate::keychain_utils::StoredPasswordChange::Keep => {}
            }
            if params.ssh_enabled.unwrap_or(false) {
                if let Some(ssh_pwd) = &params.ssh_password {
                    journal.set(id, SecretKind::SshPassword, ssh_pwd)?;
                }
                if let Some(passphrase) = &params.ssh_key_passphrase {
                    if !passphrase.trim().is_empty() {
                        journal.set(id, SecretKind::SshKeyPassphrase, passphrase)?;
                    }
                }
            } else {
                journal.delete_best_effort(id, SecretKind::SshPassword);
                journal.delete_best_effort(id, SecretKind::SshKeyPassphrase);
            }
            params_to_save.password = None;
            params_to_save.ssh_password = None;
            params_to_save.ssh_key_passphrase = None;
        } else {
            journal.delete_best_effort(id, SecretKind::DbPassword);
            journal.delete_best_effort(id, SecretKind::SshPassword);
            journal.delete_best_effort(id, SecretKind::SshKeyPassphrase);
        }

        match (&connection_uri, preserve_stored_uri) {
            (Some(uri), _) => journal.set(id, SecretKind::ConnectionUri, uri)?,
            (None, true) => {}
            (None, false) => journal.delete(id, SecretKind::ConnectionUri)?,
        }

        let updated = SavedConnection {
            id: id.to_owned(),
            name: edit.name.clone(),
            params: params_to_save,
            group_id: existing.group_id.clone(),
            sort_order: existing.sort_order,
            detect_json_in_text_columns: edit.detect_json_in_text_columns,
            appearance: existing.appearance.clone(),
            tag_ids: existing.tag_ids.clone(),
            environment: edit.environment.clone(),
            revision,
        };
        file.connections[index] = updated.clone();
        Ok(UpdateOutcome {
            connection: updated,
            previous_driver,
            previous_database: existing.params.database,
        })
    })
}

/// Changes only the stored password. Driver, TLS mode, database selection and
/// `extra` stay exactly as saved.
pub fn update_connection_password(
    path: &Path,
    backend: &dyn SecretBackend,
    id: &str,
    password: Option<&str>,
    expected_revision: u64,
) -> Result<(SavedConnection, Vec<AppliedSecret>), String> {
    with_secret_journal(path, backend, |file, journal| {
        let connection = file
            .connections
            .iter_mut()
            .find(|connection| connection.id == id)
            .ok_or("Connection not found")?;
        require_revision(connection, expected_revision)?;
        let revision = next_revision(connection)?;
        let change = crate::keychain_utils::stored_password_change(password);
        if connection.params.save_in_keychain.unwrap_or(false) {
            match change {
                crate::keychain_utils::StoredPasswordChange::Store(value) => {
                    journal.set(id, SecretKind::DbPassword, value)?
                }
                crate::keychain_utils::StoredPasswordChange::Delete => {
                    journal.delete(id, SecretKind::DbPassword)?
                }
                crate::keychain_utils::StoredPasswordChange::Keep => {}
            }
        } else {
            match change {
                crate::keychain_utils::StoredPasswordChange::Store(value) => {
                    connection.params.password = Some(value.to_owned())
                }
                crate::keychain_utils::StoredPasswordChange::Delete => {
                    connection.params.password = None
                }
                crate::keychain_utils::StoredPasswordChange::Keep => {}
            }
        }
        connection.revision = revision;
        Ok(connection.clone())
    })
}

pub struct DeleteOutcome {
    pub removed: bool,
    pub appearance: Option<ConnectionAppearance>,
}

pub fn delete_connection(
    path: &Path,
    backend: &dyn SecretBackend,
    id: &str,
) -> Result<(DeleteOutcome, Vec<AppliedSecret>), String> {
    with_secret_journal(path, backend, |file, journal| {
        let existing = file
            .connections
            .iter()
            .find(|connection| connection.id == id)
            .cloned();
        journal.delete_best_effort(id, SecretKind::DbPassword);
        journal.delete_best_effort(id, SecretKind::SshPassword);
        journal.delete_best_effort(id, SecretKind::SshKeyPassphrase);
        journal.delete(id, SecretKind::ConnectionUri)?;
        file.connections.retain(|connection| connection.id != id);
        Ok(DeleteOutcome {
            removed: existing.is_some(),
            appearance: existing.and_then(|connection| connection.appearance),
        })
    })
}

#[cfg(test)]
#[path = "connection_store_tests.rs"]
mod connection_store_tests;
