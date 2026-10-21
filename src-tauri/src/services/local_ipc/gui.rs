use super::super::{
    contracts::ErrorCode,
    error::{failure, ServiceResult},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::HashMap, sync::Mutex};
use tokio::sync::oneshot;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuiApplyRequest {
    pub map_id: String,
    pub state_version: u64,
    pub generation: u64,
    pub state: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuiApplyAck {
    pub state_version: u64,
    pub rendered_version: Option<u64>,
    pub generation: u64,
    pub gui_applied: bool,
}

pub(crate) fn validate_ack(request: &GuiApplyRequest, ack: &GuiApplyAck) -> ServiceResult<()> {
    validate_apply_request(request)?;
    if ack
        .rendered_version
        .is_some_and(|version| version > 9_007_199_254_740_991)
    {
        return Err(failure(ErrorCode::InvalidArgument));
    }
    if ack.generation != request.generation
        || ack.state_version != request.state_version
        || (ack.gui_applied && ack.rendered_version != Some(request.state_version))
    {
        return Err(failure(ErrorCode::VersionConflict));
    }
    Ok(())
}

struct Pending {
    request: GuiApplyRequest,
    sender: oneshot::Sender<GuiApplyAck>,
}
struct PendingGuard<'a> {
    store: &'a PendingGuiAcks,
    request: GuiApplyRequest,
}
impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut entries) = self.store.entries.lock() {
            if entries.get(&self.request.map_id).is_some_and(|pending| {
                pending.request.generation == self.request.generation
                    && pending.request.state_version == self.request.state_version
            }) {
                entries.remove(&self.request.map_id);
            }
        }
    }
}
#[derive(Default)]
pub struct PendingGuiAcks {
    entries: Mutex<HashMap<String, Pending>>,
}

impl PendingGuiAcks {
    pub fn register(
        &self,
        request: GuiApplyRequest,
    ) -> ServiceResult<oneshot::Receiver<GuiApplyAck>> {
        validate_apply_request(&request)?;
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| failure(ErrorCode::ResourceLimit))?;
        if entries.len() >= 64 || entries.contains_key(&request.map_id) {
            return Err(failure(ErrorCode::ResourceLimit));
        }
        let (sender, receiver) = oneshot::channel();
        entries.insert(request.map_id.clone(), Pending { request, sender });
        Ok(receiver)
    }
    pub fn acknowledge(&self, map_id: &str, ack: GuiApplyAck) -> ServiceResult<()> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| failure(ErrorCode::ResourceLimit))?;
        let pending = entries
            .get(map_id)
            .ok_or_else(|| failure(ErrorCode::VersionConflict))?;
        validate_ack(&pending.request, &ack)?;
        entries
            .remove(map_id)
            .expect("checked pending acknowledgement")
            .sender
            .send(ack)
            .map_err(|_| failure(ErrorCode::ApplyTimeout))
    }
    pub fn discard(&self, map_id: &str) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.remove(map_id);
        }
    }
    pub async fn apply<F>(&self, request: GuiApplyRequest, publish: F) -> ServiceResult<GuiApplyAck>
    where
        F: FnOnce(&GuiApplyRequest) -> ServiceResult<()>,
    {
        let receiver = self.register(request.clone())?;
        let _guard = PendingGuard {
            store: self,
            request: request.clone(),
        };
        let result = match publish(&request) {
            Ok(()) => match tokio::time::timeout(std::time::Duration::from_secs(5), receiver).await
            {
                Ok(ack) => ack.map_err(|_| apply_timeout(&request)),
                Err(_) => Err(apply_timeout(&request)),
            },
            Err(error) => Err(error),
        };
        result
    }
}

pub(crate) fn apply_timeout(request: &GuiApplyRequest) -> super::super::contracts::ServiceError {
    let mut error = failure(ErrorCode::ApplyTimeout);
    error.outcome = super::super::contracts::Outcome::Unknown;
    error.details = serde_json::json!({"state_version":request.state_version,"rendered_version":null,"gui_applied":false});
    error
}

pub(crate) fn validate_apply_request(request: &GuiApplyRequest) -> ServiceResult<()> {
    super::super::security::opaque_id(&request.map_id)?;
    if request.state_version > 9_007_199_254_740_991 || request.generation > 9_007_199_254_740_991 {
        return Err(failure(ErrorCode::InvalidArgument));
    }
    Ok(())
}
