use crate::services::{
    contracts::ErrorCode,
    error::{failure, ServiceResult},
};
use std::{collections::HashSet, sync::Mutex, time::Duration};

#[derive(Default)]
pub struct RendererSubscriptions(Mutex<HashSet<uuid::Uuid>>);

impl RendererSubscriptions {
    pub fn update(&self, id: &str, ready: bool) -> ServiceResult<()> {
        let id = uuid::Uuid::parse_str(id).map_err(|_| failure(ErrorCode::InvalidArgument))?;
        let mut subscriptions = self
            .0
            .lock()
            .map_err(|_| failure(ErrorCode::ResourceLimit))?;
        if ready {
            if subscriptions.len() >= 8 && !subscriptions.contains(&id) {
                return Err(failure(ErrorCode::ResourceLimit));
            }
            subscriptions.insert(id);
        } else {
            subscriptions.remove(&id);
        }
        Ok(())
    }

    pub async fn wait(&self) -> ServiceResult<()> {
        loop {
            if !self
                .0
                .lock()
                .map_err(|_| failure(ErrorCode::ResourceLimit))?
                .is_empty()
            {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}
