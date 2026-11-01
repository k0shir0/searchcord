//! A bounded, single-owner SQLite writer. Accepted operations outlive callers.
use crate::{Result, Store};
use serde_json::Value;
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::{Mutex, mpsc, oneshot};
type Operation = Box<dyn FnOnce(&mut Store) -> Result<Value> + Send>;
enum Message {
    Write(Operation, oneshot::Sender<Result<Value>>),
    Stop(oneshot::Sender<()>),
}
struct Inner {
    sender: mpsc::Sender<Message>,
    closed: AtomicBool,
    admission: Mutex<()>,
    worker: Mutex<Option<tokio::task::JoinHandle<()>>>,
    unobserved: Arc<StdMutex<Option<String>>>,
}
#[derive(Clone)]
pub struct Writer(Arc<Inner>);
impl Writer {
    pub async fn open(path: PathBuf) -> Result<Self> {
        let store = tokio::task::spawn_blocking(move || Store::open(path)).await??;
        let (sender, mut receiver) = mpsc::channel(128);
        let unobserved = Arc::new(StdMutex::new(None));
        let errors = unobserved.clone();
        let worker = tokio::task::spawn_blocking(move || {
            let mut store = store;
            while let Some(message) = receiver.blocking_recv() {
                match message {
                    Message::Write(operation, ack) => {
                        let result = operation(&mut store);
                        if let Err(Err(error)) = ack.send(result) {
                            *errors.lock().unwrap() = Some(error.to_string());
                        }
                    }
                    Message::Stop(ack) => {
                        drop(store);
                        let _ = ack.send(());
                        return;
                    }
                }
            }
        });
        Ok(Self(Arc::new(Inner {
            sender,
            closed: AtomicBool::new(false),
            admission: Mutex::new(()),
            worker: Mutex::new(Some(worker)),
            unobserved,
        })))
    }
    pub async fn write(
        &self,
        operation: impl FnOnce(&mut Store) -> Result<Value> + Send + 'static,
    ) -> Result<Value> {
        let (ack, reply) = oneshot::channel();
        {
            let _guard = self.0.admission.lock().await;
            if self.0.closed.load(Ordering::Acquire) {
                return Err("Collector writer is closed".into());
            }
            self.0
                .sender
                .send(Message::Write(Box::new(operation), ack))
                .await
                .map_err(|_| "Collector writer is closed")?;
        }
        reply
            .await
            .map_err(|_| "Collector writer stopped before acknowledging a write")?
    }
    pub async fn barrier(&self) -> Result<()> {
        self.write(|_| Ok(Value::Null)).await?;
        if let Some(error) = self.0.unobserved.lock().unwrap().take() {
            return Err(format!("An accepted write failed: {error}").into());
        }
        Ok(())
    }
    pub async fn shutdown(&self) -> Result<()> {
        let writer = self.clone();
        tokio::spawn(async move { writer.shutdown_inner().await }).await?
    }
    async fn shutdown_inner(&self) -> Result<()> {
        let mut worker = self.0.worker.lock().await;
        if let Some(task) = worker.take() {
            let _guard = self.0.admission.lock().await;
            self.0.closed.store(true, Ordering::Release);
            let (ack, reply) = oneshot::channel();
            self.0
                .sender
                .send(Message::Stop(ack))
                .await
                .map_err(|_| "Collector writer stopped")?;
            reply.await?;
            task.await?;
        }
        if let Some(error) = self.0.unobserved.lock().unwrap().take() {
            return Err(format!("An accepted write failed: {error}").into());
        }
        Ok(())
    }
}
