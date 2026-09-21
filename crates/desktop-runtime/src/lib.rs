#![forbid(unsafe_code)]

mod library;
mod types;
mod watch;

use std::{
    future::Future,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use anlg_db_core::{Db, DbOpenOptions, DbStorage};
use anlg_db_execute::DbExecutor;
use futures::future::BoxFuture;
use tokio::sync::{Semaphore, mpsc, oneshot};
pub use tokio_util::sync::CancellationToken;

pub use types::*;
pub use watch::{QueryWatch, WatchSnapshot};

pub const QUEUE_CAPACITY: usize = 64;
pub const MAX_WATCHES: usize = 32;
pub const MAX_DOCUMENT_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Profile {
    pub database: PathBuf,
}

#[derive(Clone)]
pub struct Services {
    pub db: Arc<Db>,
    pub executor: DbExecutor,
    watch_slots: Arc<Semaphore>,
}

type Work = Box<dyn FnOnce(Services) -> BoxFuture<'static, ()> + Send>;
pub type FlushParticipant = Box<dyn FnOnce() -> BoxFuture<'static, Result<()>> + Send>;

enum Command {
    Work(Work),
    Register(FlushParticipant, oneshot::Sender<Result<()>>),
    Shutdown(oneshot::Sender<Result<()>>),
}

pub struct Reply<T>(oneshot::Receiver<Result<T>>);

impl<T> Reply<T> {
    pub async fn receive(self) -> Result<T> {
        self.0.await.map_err(|_| ServiceError::Closed)?
    }
}

#[derive(Clone)]
pub struct RuntimeHandle {
    sender: mpsc::Sender<Command>,
    closing: Arc<AtomicBool>,
}

impl RuntimeHandle {
    pub fn start(profile: Profile) -> std::io::Result<(Self, Reply<()>)> {
        let (sender, receiver) = mpsc::channel(QUEUE_CAPACITY);
        let (ready_tx, ready_rx) = oneshot::channel();
        let handle = Self {
            sender,
            closing: Arc::new(AtomicBool::new(false)),
        };
        std::thread::Builder::new()
            .name("anarlog-runtime".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = ready_tx.send(Err(types::failure(error)));
                        return;
                    }
                };
                runtime.block_on(async move {
                    let services = async {
                        let db = Arc::new(
                            Db::open(DbOpenOptions {
                                storage: DbStorage::Local(&profile.database),
                                cloudsync_enabled: false,
                                journal_mode_wal: true,
                                foreign_keys: true,
                                max_connections: Some(4),
                            })
                            .await
                            .map_err(types::failure)?,
                        );
                        anlg_db_app::prepare_schema(&db)
                            .await
                            .map_err(types::failure)?;
                        Ok::<_, ServiceError>(Services {
                            executor: DbExecutor::new(db.clone()),
                            db,
                            watch_slots: Arc::new(Semaphore::new(MAX_WATCHES)),
                        })
                    }
                    .await;
                    match services {
                        Ok(services) => {
                            let _ = ready_tx.send(Ok(()));
                            run_queue(receiver, services).await;
                        }
                        Err(error) => {
                            let _ = ready_tx.send(Err(error));
                        }
                    }
                });
            })?;
        Ok((handle, Reply(ready_rx)))
    }

    /// Accepted work runs even when its reply is dropped. Use cancellation only for reads.
    pub fn submit<T, F, Fut>(&self, work: F) -> Result<Reply<T>>
    where
        T: Send + 'static,
        F: FnOnce(Services) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T>> + Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        self.enqueue(Command::Work(Box::new(move |services| {
            Box::pin(async move {
                let _ = tx.send(work(services).await);
            })
        })))?;
        Ok(Reply(rx))
    }

    pub fn read<T, F, Fut>(&self, cancellation: CancellationToken, work: F) -> Result<Reply<T>>
    where
        T: Send + 'static,
        F: FnOnce(Services) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T>> + Send + 'static,
    {
        self.submit(move |services| async move {
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => Err(ServiceError::Cancelled),
                result = work(services) => result,
            }
        })
    }

    pub fn register_flush(&self, participant: FlushParticipant) -> Result<Reply<()>> {
        let (tx, rx) = oneshot::channel();
        self.enqueue(Command::Register(participant, tx))?;
        Ok(Reply(rx))
    }

    pub async fn shutdown(&self) -> Result<()> {
        if self.closing.swap(true, Ordering::AcqRel) {
            return Err(ServiceError::Closed);
        }
        let (tx, rx) = oneshot::channel();
        self.sender
            .send(Command::Shutdown(tx))
            .await
            .map_err(|_| ServiceError::Closed)?;
        Reply(rx).receive().await
    }

    fn enqueue(&self, command: Command) -> Result<()> {
        if self.closing.load(Ordering::Acquire) {
            return Err(ServiceError::Closed);
        }
        self.sender.try_send(command).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => ServiceError::Busy,
            mpsc::error::TrySendError::Closed(_) => ServiceError::Closed,
        })
    }
}

async fn run_queue(mut receiver: mpsc::Receiver<Command>, services: Services) {
    let mut participants = Vec::new();
    let mut shutdown = None;
    while let Some(command) = receiver.recv().await {
        match command {
            Command::Work(work) => work(services.clone()).await,
            Command::Register(participant, reply) => {
                if participants.len() == QUEUE_CAPACITY {
                    let _ = reply.send(Err(ServiceError::Busy));
                } else {
                    participants.push(participant);
                    let _ = reply.send(Ok(()));
                }
            }
            Command::Shutdown(reply) => {
                receiver.close();
                shutdown = Some(reply);
            }
        }
    }
    let mut result = Ok(());
    for participant in participants {
        if let Err(error) = participant().await {
            result = Err(error);
        }
    }
    services.db.pool().close().await;
    if let Some(reply) = shutdown {
        let _ = reply.send(result);
    }
}
