//! A read statement run for the legacy `run_query` tool: it waits for the job
//! to end and cancels the job on the server when nobody waits for it any more.

use super::super::super::contracts::MAX_DEADLINE_MS;
use super::*;

/// How long past the deadline the wait lasts, so that the cancellation the
/// service starts at the deadline can finish and be reported.
pub(crate) const WAIT_GRACE_MS: u64 = 10_000;
const CANCEL_REQUEST_MS: u64 = 5_000;

pub(crate) struct ReadQuery {
    pub connection_id: String,
    pub text: String,
    pub page_size: u64,
    pub deadline_ms: u64,
    pub database: Option<String>,
    pub schema: Option<String>,
}

/// The job's last response and, when it ended at its deadline, what became of
/// the cancellation the service asked the driver for.
pub(crate) struct ReadOutcome {
    pub response: ServiceResponse,
    pub cancel_state: Option<String>,
}

/// Asks the job to cancel when dropped while armed. The request is spawned and
/// best effort: the caller is already gone.
struct CancelOnDrop {
    transport: Arc<ServiceTransport>,
    request: ServiceRequest,
    caller: CallerContext,
    job_id: String,
    owner: String,
    armed: bool,
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let transport = self.transport.clone();
        let caller = self.caller.clone();
        let cancel = request::control_request(
            &self.request,
            Operation::JobCancel,
            &self.job_id,
            &self.owner,
            CANCEL_REQUEST_MS,
        );
        runtime.spawn(async move {
            let _ = tokio::time::timeout(
                Duration::from_millis(CANCEL_REQUEST_MS),
                transport.submit_request(cancel, caller),
            )
            .await;
        });
    }
}

impl ServiceTransport {
    /// Runs `query` as a read and returns once the job is terminal. Dropping
    /// the returned future, or a wait that ends before the job does, cancels
    /// the job on the server.
    pub(crate) async fn run_read_query(
        self: &Arc<Self>,
        query: ReadQuery,
        caller: CallerContext,
    ) -> ServiceResult<ReadOutcome> {
        request::validate_caller(&caller)?;
        if query.deadline_ms > MAX_DEADLINE_MS {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        let mut input = json!({
            "language":"sql",
            "text":query.text,
            "parameters":[],
            "page_size":query.page_size,
        });
        for (key, value) in [("database", &query.database), ("schema", &query.schema)] {
            if let Some(value) = value {
                input[key] = json!(value);
            }
        }
        let raw = json!({
            "protocol_version":1,
            "operation":"query.execute",
            "request_id":uuid::Uuid::new_v4().to_string(),
            "connection_id":query.connection_id,
            "deadline_ms":query.deadline_ms,
            "input":input,
        });
        let decoded = request::decode(&self.validator, &raw)?;
        let extras = Extras {
            database: query.database,
            ..Extras::default()
        };
        self.check_database(&decoded, &extras, &caller, &None)
            .await?;
        let queued = self.submit_request(decoded.clone(), caller.clone()).await?;
        if request::terminal(queued.status) {
            let response = request::checked_response(&self.validator, &decoded, queued, true)?;
            return Ok(ReadOutcome {
                response,
                cancel_state: None,
            });
        }
        let (job_id, owner) = Self::job_identity(&queued)?;
        let mut guard = CancelOnDrop {
            transport: self.clone(),
            request: decoded.clone(),
            caller: caller.clone(),
            job_id: job_id.clone(),
            owner: owner.clone(),
            armed: true,
        };
        let waited = self
            .wait_for_job(
                &decoded,
                &caller,
                &job_id,
                &owner,
                Duration::from_millis(query.deadline_ms.saturating_add(WAIT_GRACE_MS)),
                Default::default(),
            )
            .await?;
        let Waited::Terminal(response) = waited else {
            return Ok(ReadOutcome {
                response: request::unknown_response(&decoded, Some(job_id)),
                cancel_state: None,
            });
        };
        guard.armed = false;
        let cancel_state = match &response.error {
            Some(error) if error.code == ErrorCode::DeadlineExceeded => {
                self.cancel_state(&decoded, &caller, &job_id, &owner).await
            }
            _ => None,
        };
        Ok(ReadOutcome {
            response,
            cancel_state,
        })
    }

    async fn cancel_state(
        &self,
        decoded: &ServiceRequest,
        caller: &CallerContext,
        job_id: &str,
        owner: &str,
    ) -> Option<String> {
        let get = request::control_request(decoded, Operation::JobGet, job_id, owner, 5_000);
        let state = self.submit_request(get, caller.clone()).await.ok()?;
        state.data["cancel_state"].as_str().map(str::to_owned)
    }
}

#[cfg(test)]
#[path = "query_run_tests.rs"]
mod tests;
