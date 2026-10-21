use deadpool_postgres::Object;
use tokio::sync::OwnedMutexGuard;

use super::{types::DriverContext, SpatialError};
use crate::{cancel::ScopedGuard, client, models::ConnectionParams, session};

pub enum Lease {
    Owned(Box<Option<Object>>),
    Session(OwnedMutexGuard<session::ServiceSlot>, bool),
}
impl Drop for Lease {
    fn drop(&mut self) {
        if let Self::Owned(client) = self {
            if let Some(client) = client.take() {
                drop(Object::take(client));
            }
        }
        if let Self::Session(slot, false) = self {
            if let Some(client) = slot.client.take() {
                drop(Object::take(client));
            }
        }
    }
}
impl Lease {
    pub fn client(&self) -> &Object {
        match self {
            Self::Owned(client) => client.as_ref().as_ref().unwrap(),
            Self::Session(slot, _) => slot.client.as_ref().unwrap(),
        }
    }
    pub async fn acquire(
        context: &DriverContext,
        params: &ConnectionParams,
        scope: &ScopedGuard,
    ) -> Result<Self, SpatialError> {
        client::normalize_service_params(params)?;
        if scope.cancelled() {
            return Err(SpatialError::new(
                "CANCELLED",
                "Request was cancelled before database execution",
            ));
        }
        let lease = if let Some(handle) = &context.session_handle {
            Self::Session(
                session::lock_service(handle, &context.connection_id, context.read_only).await?,
                false,
            )
        } else {
            let pool = client::build_service_pool(params).await?;
            let client = pool.get().await.map_err(|_| {
                SpatialError::new("CONNECTION_NOT_FOUND", "Cannot acquire database connection")
            })?;
            let lease = Self::Owned(Box::new(Some(client)));
            lease
                .client()
                .batch_execute(if context.read_only {
                    "BEGIN READ ONLY"
                } else {
                    "BEGIN READ WRITE"
                })
                .await
                .map_err(SpatialError::database)?;
            lease
        };
        scope.attach(lease.client(), params.clone()).await;
        if scope.cancelled() {
            return Err(SpatialError::new(
                "CANCELLED",
                "Request was cancelled before database execution",
            ));
        }
        let deadline = context.deadline_ms.to_string();
        lease
            .client()
            .query_one(
                "SELECT pg_catalog.set_config('statement_timeout',$1,true)",
                &[&deadline],
            )
            .await
            .map_err(SpatialError::database)?;
        verify_transaction(lease.client(), context.read_only).await?;
        Ok(lease)
    }
    pub async fn finish(
        &mut self,
        scope: &ScopedGuard,
        success: bool,
        read_only: bool,
    ) -> Result<(), SpatialError> {
        scope.seal();
        scope.settle().await;
        let stopped = scope.cancelled() || scope.backend_stopped();
        match self {
            Self::Owned(client) => {
                let Some(pg) = client.as_ref() else {
                    return Ok(());
                };
                let commit = success && !read_only && !stopped;
                if commit {
                    match tokio::time::timeout(
                        std::time::Duration::from_secs(5),
                        pg.batch_execute("COMMIT"),
                    )
                    .await
                    {
                        Ok(Ok(())) => {}
                        _ => {
                            return Err(SpatialError::new(
                                "OUTCOME_UNKNOWN",
                                "Transaction commit could not be confirmed",
                            ))
                        }
                    }
                } else if !session::confirm_rollback(pg).await {
                    return Err(SpatialError::new(
                        "OUTCOME_UNKNOWN",
                        "Transaction rollback could not be confirmed",
                    ));
                }
                if stopped {
                    if let Some(pg) = client.take() {
                        drop(Object::take(pg));
                    }
                } else {
                    client.take();
                }
            }
            Self::Session(slot, completed) => {
                slot.last_used = std::time::Instant::now();
                if !success || stopped {
                    if let Some(pg) = slot.client.take() {
                        let confirmed = session::confirm_rollback(&pg).await;
                        if stopped || !confirmed {
                            drop(Object::take(pg));
                        }
                        if !confirmed {
                            return Err(SpatialError::new(
                                "OUTCOME_UNKNOWN",
                                "Session rollback could not be confirmed",
                            ));
                        }
                    }
                }
                *completed = true;
            }
        }
        Ok(())
    }
}

pub async fn verify_transaction(
    client: &tokio_postgres::Client,
    read_only: bool,
) -> Result<(), SpatialError> {
    client
        .batch_execute("SAVEPOINT service_transaction_probe")
        .await
        .map_err(|_| {
            SpatialError::new(
                "OUTCOME_UNKNOWN",
                "Reserved transaction is no longer active",
            )
        })?;
    let actual: String = client
        .query_one("SHOW transaction_read_only", &[])
        .await
        .map_err(SpatialError::database)?
        .get(0);
    client
        .batch_execute("RELEASE SAVEPOINT service_transaction_probe")
        .await
        .map_err(SpatialError::database)?;
    if (actual == "on") != read_only {
        return Err(SpatialError::new(
            "WRITE_NOT_ALLOWED",
            "Database transaction access mode does not match context",
        ));
    }
    Ok(())
}
