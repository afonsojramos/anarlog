use std::sync::{Arc, Mutex};

use anlg_db_reactive::{LiveQueryRuntime, QueryEventSink, SubscriptionRegistration};
use serde_json::Value;
use tokio::sync::{OwnedSemaphorePermit, mpsc, watch};

use crate::{Reply, Result, RuntimeHandle, ServiceError, types::failure};

#[derive(Clone, Debug)]
pub struct WatchSnapshot {
    pub sequence: u64,
    pub rows: Arc<[Value]>,
}

#[derive(Clone)]
struct Sink {
    snapshots: watch::Sender<WatchSnapshot>,
    errors: mpsc::Sender<ServiceError>,
    terminal_error: Arc<Mutex<Option<ServiceError>>>,
}

impl QueryEventSink for Sink {
    fn send_result(&self, rows: Vec<Value>) -> std::result::Result<(), String> {
        self.snapshots.send_modify(|snapshot| {
            snapshot.sequence += 1;
            snapshot.rows = rows.into();
        });
        Ok(())
    }

    fn send_error(&self, error: String) -> std::result::Result<(), String> {
        let error = ServiceError::Failed(error.into());
        if self.errors.try_send(error.clone()).is_err() {
            *self.terminal_error.lock().map_err(|e| e.to_string())? = Some(error);
            self.snapshots
                .send_modify(|snapshot| snapshot.sequence += 1);
            return Err("Watch error queue overflow; subscription terminated".into());
        }
        self.snapshots
            .send_modify(|snapshot| snapshot.sequence += 1);
        Ok(())
    }
}

pub struct QueryWatch {
    pub registration: SubscriptionRegistration,
    pub snapshots: watch::Receiver<WatchSnapshot>,
    pub errors: mpsc::Receiver<ServiceError>,
    terminal_error: Arc<Mutex<Option<ServiceError>>>,
    runtime: Arc<LiveQueryRuntime<Sink>>,
    _slot: OwnedSemaphorePermit,
}

impl QueryWatch {
    pub fn terminal_error(&self) -> Option<ServiceError> {
        self.terminal_error
            .lock()
            .ok()
            .and_then(|error| error.clone())
    }

    pub async fn unsubscribe(self) -> Result<()> {
        self.runtime
            .unsubscribe(&self.registration.id)
            .await
            .map_err(failure)
    }
}

impl RuntimeHandle {
    pub fn watch_library(&self) -> Result<Reply<QueryWatch>> {
        self.watch_query(
            "SELECT id, title, updated_at FROM sessions WHERE deleted_at IS NULL ORDER BY updated_at DESC, id DESC LIMIT 1".into(),
            vec![],
        )
    }

    pub fn watch_query(&self, sql: String, params: Vec<Value>) -> Result<Reply<QueryWatch>> {
        self.submit(move |services| async move {
            let slot = services
                .watch_slots
                .try_acquire_owned()
                .map_err(|_| ServiceError::Busy)?;
            let (snapshots, receiver) = watch::channel(WatchSnapshot {
                sequence: 0,
                rows: Arc::from([]),
            });
            let (errors, error_receiver) = mpsc::channel(8);
            let terminal_error = Arc::new(Mutex::new(None));
            let runtime = Arc::new(LiveQueryRuntime::new(services.db));
            let registration = runtime
                .subscribe(
                    sql,
                    params,
                    Sink {
                        snapshots,
                        errors,
                        terminal_error: terminal_error.clone(),
                    },
                )
                .await
                .map_err(failure)?;
            Ok(QueryWatch {
                registration,
                snapshots: receiver,
                errors: error_receiver,
                terminal_error,
                runtime,
                _slot: slot,
            })
        })
    }
}
