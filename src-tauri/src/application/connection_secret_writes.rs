use crate::runtime::RuntimeContext;

#[derive(Default)]
pub(super) struct SecretWrites(Vec<(String, Option<String>)>);

impl SecretWrites {
    pub(super) fn set(
        &mut self,
        runtime: &RuntimeContext,
        account: String,
        value: &str,
    ) -> Result<(), String> {
        let previous = runtime.secrets.get(&account)?;
        runtime.secrets.set(&account, value)?;
        self.0.push((account, previous));
        Ok(())
    }

    pub(super) fn delete(
        &mut self,
        runtime: &RuntimeContext,
        account: String,
    ) -> Result<(), String> {
        let previous = runtime.secrets.get(&account)?;
        runtime.secrets.delete(&account)?;
        self.0.push((account, previous));
        Ok(())
    }

    /// Puts every changed secret back, newest first. Returns `false` when a
    /// secret could not be restored, so the caller can report that the
    /// connection needs recovery instead of a plain failure.
    pub(super) fn rollback(&mut self, runtime: &RuntimeContext) -> bool {
        let mut restored_all = true;
        for (account, previous) in self.0.drain(..).rev() {
            let result = match previous {
                Some(value) => runtime.secrets.set(&account, &value),
                None => runtime.secrets.delete(&account),
            };
            if let Err(error) = result {
                restored_all = false;
                log::warn!("Failed to restore connection credential {account}: {error}");
            }
        }
        restored_all
    }
}
