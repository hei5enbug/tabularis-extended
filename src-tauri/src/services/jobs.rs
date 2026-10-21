use super::{
    backend::CancellationSignal,
    contracts::{ErrorCode, JobStatus, Outcome, ServiceRequest, ServiceResponse},
    dispatch::CallerContext,
    error::{failure, ServiceResult},
    storage::{now_ms, PrivateStore},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex, Weak,
    },
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub const GLOBAL_CONCURRENCY: usize = 8;
pub const CONNECTION_CONCURRENCY: usize = 4;
pub const QUEUE_CAPACITY: usize = 64;
const JOB_TTL_MS: u64 = 15 * 60 * 1000;
const MAX_JOB_RECORDS: usize = 256;
const MAX_JOB_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
pub struct JobRecord {
    pub job_id: String,
    pub request_id: String,
    pub connection_id: Option<String>,
    pub owner_instance_id: String,
    pub caller_id: String,
    #[serde(default)]
    pub caller_owner_instance_id: String,
    pub status: JobStatus,
    pub is_write: bool,
    pub write_dispatched: bool,
    #[serde(default)]
    pub write_confirmed: bool,
    pub cancel_requested: bool,
    pub created_at_ms: u64,
    pub expires_at_ms: u64,
    pub response: Option<ServiceResponse>,
}

struct ActiveJob {
    record: Mutex<JobRecord>,
    signal: CancellationSignal,
}
struct State {
    active_requests: HashMap<(Option<String>, String), String>,
    active_jobs: HashMap<String, Arc<ActiveJob>>,
    connections: HashMap<String, Weak<Semaphore>>,
}

pub struct JobManager {
    instance_id: String,
    state: Mutex<State>,
    global: Arc<Semaphore>,
    waiting: AtomicUsize,
    store: PrivateStore,
}

impl JobManager {
    pub fn new(instance_id: String, root: PathBuf) -> ServiceResult<Arc<Self>> {
        let manager = Arc::new(Self {
            instance_id,
            state: Mutex::new(State {
                active_requests: HashMap::new(),
                active_jobs: HashMap::new(),
                connections: HashMap::new(),
            }),
            global: Arc::new(Semaphore::new(GLOBAL_CONCURRENCY)),
            waiting: AtomicUsize::new(0),
            store: PrivateStore::new(root, 10 * 1024 * 1024)?,
        });
        manager.recover()?;
        Ok(manager)
    }
    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }
    fn recover(&self) -> ServiceResult<()> {
        let _lock = self.store.lock()?;
        for id in self.store.ids()? {
            if let Some(mut record) = self.store.read::<JobRecord>(&id)? {
                if record.expires_at_ms <= now_ms() {
                    self.store.remove(&id)?;
                    continue;
                }
                if record.owner_instance_id == self.instance_id
                    && matches!(record.status, JobStatus::Queued | JobStatus::Running)
                {
                    interrupt_record(&mut record);
                    self.store.write(&id, &record)?;
                }
            }
        }
        Ok(())
    }
    pub fn recover_owner(&self, owner_instance_id: &str) -> ServiceResult<()> {
        let _lock = self.store.lock()?;
        for id in self.store.ids()? {
            if let Some(mut record) = self.store.read::<JobRecord>(&id)? {
                if record.owner_instance_id == owner_instance_id
                    && matches!(record.status, JobStatus::Queued | JobStatus::Running)
                {
                    interrupt_record(&mut record);
                    self.store.write(&id, &record)?;
                }
            }
        }
        Ok(())
    }
    fn persist(&self, record: &JobRecord) -> ServiceResult<()> {
        let _lock = self.store.lock()?;
        for id in self.store.ids()? {
            if self
                .store
                .read::<JobRecord>(&id)?
                .is_some_and(|job| job.expires_at_ms <= now_ms())
            {
                self.store.remove(&id)?;
            }
        }
        if self.store.read::<JobRecord>(&record.job_id)?.is_none()
            && self.store.ids()?.len() >= MAX_JOB_RECORDS
        {
            return Err(failure(ErrorCode::ResourceLimit));
        }
        let new_bytes = serde_json::to_vec(record)
            .map_err(|_| failure(ErrorCode::ResourceLimit))?
            .len() as u64;
        let old_bytes = self
            .store
            .read::<JobRecord>(&record.job_id)?
            .map(|value| {
                serde_json::to_vec(&value)
                    .map(|value| value.len() as u64)
                    .unwrap_or(0)
            })
            .unwrap_or(0);
        if self
            .store
            .bytes()?
            .saturating_sub(old_bytes)
            .saturating_add(new_bytes)
            > MAX_JOB_BYTES
        {
            return Err(failure(ErrorCode::ResourceLimit));
        }
        self.store.write(&record.job_id, record)
    }
    pub fn queue(
        self: &Arc<Self>,
        request: &ServiceRequest,
        caller: &CallerContext,
        is_write: bool,
        control: bool,
    ) -> ServiceResult<JobLease> {
        let record = JobRecord {
            job_id: format!("j-{}", uuid::Uuid::new_v4()),
            request_id: request.request_id.clone(),
            connection_id: request.connection_id.clone(),
            owner_instance_id: self.instance_id.clone(),
            caller_id: caller.caller_id.clone(),
            caller_owner_instance_id: caller.owner_instance_id.clone(),
            status: JobStatus::Queued,
            is_write,
            write_dispatched: false,
            write_confirmed: false,
            cancel_requested: false,
            created_at_ms: now_ms(),
            expires_at_ms: now_ms().saturating_add(JOB_TTL_MS),
            response: None,
        };
        let active = Arc::new(ActiveJob {
            record: Mutex::new(record.clone()),
            signal: CancellationSignal::default(),
        });
        let connection_semaphore = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| failure(ErrorCode::ResourceLimit))?;
            let request_key = (request.connection_id.clone(), request.request_id.clone());
            if state.active_requests.contains_key(&request_key) {
                return Err(failure(ErrorCode::InvalidArgument));
            }
            if state.active_requests.len() >= GLOBAL_CONCURRENCY + QUEUE_CAPACITY + 64 {
                return Err(failure(ErrorCode::RateLimited));
            }
            if !control && self.waiting.load(Ordering::SeqCst) >= QUEUE_CAPACITY {
                return Err(failure(ErrorCode::RateLimited));
            }
            if !control {
                self.waiting.fetch_add(1, Ordering::SeqCst);
            }
            state
                .active_requests
                .insert(request_key, record.job_id.clone());
            state
                .active_jobs
                .insert(record.job_id.clone(), active.clone());
            state
                .connections
                .retain(|_, value| value.strong_count() > 0);
            request.connection_id.as_ref().map(|id| {
                match state.connections.get(id).and_then(Weak::upgrade) {
                    Some(semaphore) => semaphore,
                    None => {
                        let semaphore = Arc::new(Semaphore::new(CONNECTION_CONCURRENCY));
                        state
                            .connections
                            .insert(id.clone(), Arc::downgrade(&semaphore));
                        semaphore
                    }
                }
            })
        };
        let lease = JobLease {
            manager: self.clone(),
            active,
            global_permit: None,
            connection_permit: None,
            connection_semaphore,
            waiting: !control,
            persisted: !control,
            completed: AtomicBool::new(false),
            deadline_at_ms: now_ms().saturating_add(request.deadline_ms),
        };
        if !control {
            self.persist(&record)?;
        }
        Ok(lease)
    }
    pub async fn start(&self, lease: &mut JobLease) -> ServiceResult<()> {
        if lease.signal().is_cancelled() {
            return Err(failure(ErrorCode::Cancelled));
        }
        if now_ms() >= lease.deadline_at_ms {
            return Err(failure(ErrorCode::DeadlineExceeded));
        }
        if lease.waiting {
            let connection_semaphore = lease.connection_semaphore.take();
            let acquire = async {
                let connection = match connection_semaphore {
                    Some(semaphore) => Some(
                        semaphore
                            .acquire_owned()
                            .await
                            .map_err(|_| failure(ErrorCode::RateLimited))?,
                    ),
                    None => None,
                };
                let global = self
                    .global
                    .clone()
                    .acquire_owned()
                    .await
                    .map_err(|_| failure(ErrorCode::RateLimited))?;
                Ok::<_, super::contracts::ServiceError>((connection, global))
            };
            let signal = lease.signal();
            let permits = tokio::select! {
                _ = signal.cancelled() => return Err(failure(ErrorCode::Cancelled)),
                result = tokio::time::timeout(std::time::Duration::from_millis(lease.deadline_at_ms.saturating_sub(now_ms())), acquire) => result.map_err(|_| failure(ErrorCode::DeadlineExceeded))??,
            };
            lease.connection_permit = permits.0;
            lease.global_permit = Some(permits.1);
            self.waiting.fetch_sub(1, Ordering::SeqCst);
            lease.waiting = false;
        }
        {
            let mut record = lease
                .active
                .record
                .lock()
                .map_err(|_| failure(ErrorCode::ResourceLimit))?;
            record.status = JobStatus::Running;
            if lease.persisted {
                self.persist(&record)?;
            }
        }
        Ok(())
    }
    pub async fn begin(
        self: &Arc<Self>,
        request: &ServiceRequest,
        caller: &CallerContext,
        is_write: bool,
        control: bool,
    ) -> ServiceResult<JobLease> {
        let mut lease = self.queue(request, caller, is_write, control)?;
        self.start(&mut lease).await?;
        Ok(lease)
    }
    pub fn get(
        &self,
        job_id: &str,
        owner: &str,
        caller: &CallerContext,
    ) -> ServiceResult<JobRecord> {
        let record = self
            .store
            .read::<JobRecord>(job_id)?
            .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
        if record.owner_instance_id != owner
            || record.caller_id != caller.caller_id
            || record.caller_owner_instance_id != caller.owner_instance_id
        {
            return Err(failure(ErrorCode::PermissionDenied));
        }
        if record.expires_at_ms <= now_ms() {
            return Err(failure(ErrorCode::ResultExpired));
        }
        Ok(record)
    }
    pub fn cancel(
        &self,
        job_id: &str,
        owner: &str,
        caller: &CallerContext,
    ) -> ServiceResult<JobRecord> {
        let mut record = self.get(job_id, owner, caller)?;
        if owner != self.instance_id {
            return Err(failure(ErrorCode::CapabilityUnavailable));
        }
        let active = {
            self.state
                .lock()
                .map_err(|_| failure(ErrorCode::ResourceLimit))?
                .active_jobs
                .get(job_id)
                .cloned()
        };
        if let Some(active) = active {
            active.signal.cancel();
            let mut current = active
                .record
                .lock()
                .map_err(|_| failure(ErrorCode::ResourceLimit))?;
            current.cancel_requested = true;
            record = current.clone();
            self.persist(&record)?;
        }
        Ok(record)
    }
    pub fn active_count(&self) -> usize {
        self.state
            .lock()
            .map(|state| state.active_jobs.len())
            .unwrap_or(0)
    }
    pub fn cancel_active(&self) -> Vec<JobRecord> {
        let active: Vec<_> = self
            .state
            .lock()
            .map(|state| state.active_jobs.values().cloned().collect())
            .unwrap_or_default();
        active
            .into_iter()
            .filter_map(|job| {
                job.signal.cancel();
                let mut record = job.record.lock().ok()?;
                record.cancel_requested = true;
                Some(record.clone())
            })
            .collect()
    }
}

pub struct JobLease {
    manager: Arc<JobManager>,
    active: Arc<ActiveJob>,
    global_permit: Option<OwnedSemaphorePermit>,
    connection_permit: Option<OwnedSemaphorePermit>,
    connection_semaphore: Option<Arc<Semaphore>>,
    waiting: bool,
    persisted: bool,
    completed: AtomicBool,
    pub deadline_at_ms: u64,
}
impl JobLease {
    pub fn signal(&self) -> CancellationSignal {
        self.active.signal.clone()
    }
    pub fn id(&self) -> String {
        self.active
            .record
            .lock()
            .map(|record| record.job_id.clone())
            .unwrap_or_default()
    }
    pub fn mark_write_dispatched(&self) -> ServiceResult<()> {
        if self.signal().is_cancelled() {
            return Err(failure(ErrorCode::Cancelled));
        }
        if now_ms() >= self.deadline_at_ms {
            return Err(failure(ErrorCode::DeadlineExceeded));
        }
        let mut record = self
            .active
            .record
            .lock()
            .map_err(|_| failure(ErrorCode::ResourceLimit))?;
        record.is_write = true;
        record.write_dispatched = true;
        if let Err(error) = self.manager.persist(&record) {
            record.write_dispatched = false;
            return Err(error);
        }
        Ok(())
    }
    pub fn write_dispatched(&self) -> bool {
        self.active
            .record
            .lock()
            .map(|record| record.write_dispatched)
            .unwrap_or(true)
    }
    pub fn write_confirmed(&self) -> bool {
        self.active
            .record
            .lock()
            .map(|record| record.write_confirmed)
            .unwrap_or(false)
    }
    pub fn mark_write_confirmed(&self) -> ServiceResult<()> {
        let mut record = self
            .active
            .record
            .lock()
            .map_err(|_| failure(ErrorCode::ResourceLimit))?;
        record.write_confirmed = true;
        if self.persisted {
            self.manager.persist(&record)?;
        }
        Ok(())
    }
    pub fn finish(&self, mut response: ServiceResponse) -> ServiceResult<ServiceResponse> {
        let mut record = self
            .active
            .record
            .lock()
            .map_err(|_| failure(ErrorCode::ResourceLimit))?;
        response.job_id = if self.persisted {
            Some(record.job_id.clone())
        } else {
            None
        };
        if record.write_dispatched
            && response
                .error
                .as_ref()
                .is_some_and(|error| error.outcome == Outcome::Unknown)
        {
            response.status = JobStatus::OutcomeUnknown;
        }
        record.status = response.status;
        let mut persisted = response.clone();
        if let Some(data) = persisted.data.as_object_mut() {
            data.remove("challenge");
        }
        record.response = Some(persisted);
        if self.persisted {
            self.manager.persist(&record)?;
        }
        self.completed.store(true, Ordering::SeqCst);
        Ok(response)
    }
}
impl Drop for JobLease {
    fn drop(&mut self) {
        if self.waiting {
            self.manager.waiting.fetch_sub(1, Ordering::SeqCst);
        }
        if let Ok(mut record) = self.active.record.lock() {
            if !self.completed.load(Ordering::SeqCst) {
                interrupt_record(&mut record);
                if self.persisted {
                    let _ = self.manager.persist(&record);
                }
            }
            if let Ok(mut state) = self.manager.state.lock() {
                state
                    .active_requests
                    .remove(&(record.connection_id.clone(), record.request_id.clone()));
                state.active_jobs.remove(&record.job_id);
            }
        }
    }
}

fn interrupt_record(record: &mut JobRecord) {
    let mut error = failure(if record.write_dispatched && !record.write_confirmed {
        ErrorCode::OutcomeUnknown
    } else {
        ErrorCode::DriverExited
    });
    if record.write_confirmed {
        error.outcome = Outcome::Applied;
    } else if record.write_dispatched {
        error.outcome = Outcome::Unknown;
    }
    record.status = if error.outcome == Outcome::Unknown {
        JobStatus::OutcomeUnknown
    } else {
        JobStatus::Interrupted
    };
    record.response = Some(ServiceResponse {
        protocol_version: 1,
        request_id: record.request_id.clone(),
        connection_id: record.connection_id.clone(),
        status: record.status,
        job_id: Some(record.job_id.clone()),
        result_id: None,
        data: serde_json::Value::Null,
        page: Default::default(),
        limits: Default::default(),
        metrics: Default::default(),
        warnings: vec![],
        error: Some(error),
    });
}
