use anyhow::{anyhow, Error};
use bson::{doc, Document, Timestamp};
use futures_util::FutureExt;
use mongodb::change_stream::event::{ChangeStreamEvent, OperationType, ResumeToken};
use mongodb::options::{ChangeStreamOptions, FullDocumentType};
use mongodb::{change_stream::ChangeStream, Database};
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::spawn;
use tokio::sync::broadcast::{channel, Receiver, Sender};
use tokio::sync::{oneshot, Mutex};
use tokio::time::{sleep, Duration};

const RECOVERY_RETRY_DELAY: Duration = Duration::from_millis(500);

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Clear,
    Delete(Document),
    Insert(Document),
    Resync(Option<Timestamp>),
    Update(Document),
}

pub struct WatchSubscription {
    pub receiver: Receiver<Event>,
    pub start_at_operation_time: Option<Timestamp>,
}

struct CollectionWatcher {
    checkpoint: Arc<Mutex<StreamCheckpointState>>,
    sender: Sender<Event>,
}

#[derive(Clone, Debug, PartialEq)]
enum StreamCheckpoint {
    OperationTime(Timestamp),
    ResumeAfter(ResumeToken),
    StartAfter(ResumeToken),
}

impl StreamCheckpoint {
    fn into_options(self, full_document: Option<FullDocumentType>) -> ChangeStreamOptions {
        let builder = ChangeStreamOptions::builder().full_document(full_document);
        match self {
            Self::OperationTime(timestamp) => builder
                .start_at_operation_time(Some(timestamp))
                .build(),
            Self::ResumeAfter(token) => builder.resume_after(Some(token)).build(),
            Self::StartAfter(token) => builder.start_after(Some(token)).build(),
        }
    }
}

#[derive(Debug, Default)]
struct StreamCheckpointState {
    checkpoint: Option<StreamCheckpoint>,
}

impl StreamCheckpointState {
    fn checkpoint(&self) -> Option<StreamCheckpoint> {
        self.checkpoint.clone()
    }

    fn mark_invalidated(&mut self, resume_token: Option<ResumeToken>) -> bool {
        let Some(resume_token) = resume_token else {
            return false;
        };

        self.checkpoint = Some(StreamCheckpoint::StartAfter(resume_token));
        true
    }

    fn set_checkpoint(&mut self, checkpoint: StreamCheckpoint) {
        self.checkpoint = Some(checkpoint);
    }

    fn update_resume_token(&mut self, resume_token: Option<ResumeToken>) {
        let Some(resume_token) = resume_token else {
            return;
        };

        self.checkpoint = Some(StreamCheckpoint::ResumeAfter(resume_token));
    }
}

enum BufferedChange {
    Event(Event),
    Invalidate,
    Ignore,
}

#[derive(Clone, Copy)]
enum OpenMode {
    Fresh,
    StoredCheckpoint,
}

#[derive(Clone, Copy)]
enum PendingResync {
    FreshBoundary,
    StoredCheckpoint,
}

struct OpenedStream {
    change_stream: ChangeStream<ChangeStreamEvent<Document>>,
    start_at_operation_time: Option<Timestamp>,
}

pub struct Watcher {
    change_streams: BTreeMap<String, Arc<CollectionWatcher>>,
    database: Database,
    full_document: Option<FullDocumentType>,
}

impl Watcher {
    pub fn new(database: Database, full_document: Option<FullDocumentType>) -> Self {
        Self {
            change_streams: BTreeMap::new(),
            database,
            full_document,
        }
    }

    async fn start(
        &self,
        collection: String,
        collection_watcher: Arc<CollectionWatcher>,
    ) -> Result<Timestamp, Error> {
        let database = self.database.clone();
        let full_document = self.full_document.clone();
        let (ready_sender, ready_receiver) = oneshot::channel();

        let task = async move {
            run_change_stream_task(
                database,
                collection,
                full_document,
                collection_watcher,
                ready_sender,
            )
            .await
        };

        spawn(task.then(|result| async move {
            if let Err(error) = &result {
                println!("\x1b[0;31m[[ERROR]] {error:?}\x1b[0m");
            }
            result
        }));

        await_ready(ready_receiver).await
    }

    pub async fn watch(&mut self, collection: String) -> Result<WatchSubscription, Error> {
        if let Some(collection_watcher) = self.change_streams.get(&collection) {
            return Ok(WatchSubscription {
                receiver: collection_watcher.sender.subscribe(),
                start_at_operation_time: None,
            });
        }

        let (sender, receiver) = channel(1024);
        let collection_watcher = Arc::new(CollectionWatcher {
            checkpoint: Arc::new(Mutex::new(StreamCheckpointState::default())),
            sender,
        });
        let start_at_operation_time = self
            .start(collection.clone(), collection_watcher.clone())
            .await?;
        self.change_streams.insert(collection, collection_watcher);

        Ok(WatchSubscription {
            receiver,
            start_at_operation_time: Some(start_at_operation_time),
        })
    }
}

async fn await_ready(
    ready_receiver: oneshot::Receiver<Result<Timestamp, Error>>,
) -> Result<Timestamp, Error> {
    match ready_receiver.await {
        Ok(result) => result,
        Err(_) => Err(anyhow!(
            "change stream startup task terminated before reporting readiness"
        )),
    }
}

async fn capture_operation_time(database: &Database, collection: &str) -> Result<Timestamp, Error> {
    let mut session = database
        .collection::<Document>(collection)
        .client()
        .start_session(None)
        .await?;
    database
        .run_command_with_session(doc! { "ping": 1 }, None, &mut session)
        .await?;

    session
        .operation_time()
        .ok_or_else(|| anyhow!("MongoDB session did not report an operation time"))
}

fn change_stream_pipeline() -> [Document; 2] {
    [
        doc! { "$match": { "operationType": { "$in": ["delete", "drop", "dropDatabase", "insert", "invalidate", "replace", "update"] } } },
        doc! { "$project": { "_id": 1, "documentKey": 1, "fullDocument": 1, "ns": 1, "operationType": 1 } },
    ]
}

fn classify_change_event(event: ChangeStreamEvent<Document>) -> BufferedChange {
    match event {
        ChangeStreamEvent {
            operation_type: OperationType::Delete,
            document_key: Some(document_key),
            ..
        } => BufferedChange::Event(Event::Delete(document_key)),
        ChangeStreamEvent {
            operation_type: OperationType::Drop | OperationType::DropDatabase,
            ..
        } => BufferedChange::Event(Event::Clear),
        ChangeStreamEvent {
            operation_type: OperationType::Insert,
            full_document: Some(full_document),
            ..
        } => BufferedChange::Event(Event::Insert(full_document)),
        ChangeStreamEvent {
            operation_type: OperationType::Invalidate,
            ..
        } => BufferedChange::Invalidate,
        ChangeStreamEvent {
            operation_type: OperationType::Replace | OperationType::Update,
            full_document: Some(full_document),
            ..
        } => BufferedChange::Event(Event::Update(full_document)),
        _ => BufferedChange::Ignore,
    }
}

async fn open_change_stream(
    database: &Database,
    collection: &str,
    checkpoint_state: &Arc<Mutex<StreamCheckpointState>>,
    full_document: Option<FullDocumentType>,
    open_mode: OpenMode,
) -> Result<OpenedStream, Error> {
    let checkpoint = match open_mode {
        OpenMode::Fresh => {
            let timestamp = capture_operation_time(database, collection).await?;
            let checkpoint = StreamCheckpoint::OperationTime(timestamp);
            checkpoint_state
                .lock()
                .await
                .set_checkpoint(checkpoint.clone());
            checkpoint
        }
        OpenMode::StoredCheckpoint => match checkpoint_state.lock().await.checkpoint() {
            Some(checkpoint) => checkpoint,
            None => {
                let timestamp = capture_operation_time(database, collection).await?;
                let checkpoint = StreamCheckpoint::OperationTime(timestamp);
                checkpoint_state
                    .lock()
                    .await
                    .set_checkpoint(checkpoint.clone());
                checkpoint
            }
        },
    };

    let start_at_operation_time = match &checkpoint {
        StreamCheckpoint::OperationTime(timestamp) => Some(*timestamp),
        StreamCheckpoint::ResumeAfter(_) | StreamCheckpoint::StartAfter(_) => None,
    };

    let change_stream = database
        .collection::<Document>(collection)
        .watch(change_stream_pipeline(), Some(checkpoint.into_options(full_document)))
        .await?;

    Ok(OpenedStream {
        change_stream,
        start_at_operation_time,
    })
}

fn requires_fresh_recovery(error: &Error) -> bool {
    let Some(error) = error.downcast_ref::<mongodb::error::Error>() else {
        return false;
    };

    requires_fresh_recovery_mongo(error)
}

fn requires_fresh_recovery_mongo(error: &mongodb::error::Error) -> bool {
    match error.kind.as_ref() {
        mongodb::error::ErrorKind::MissingResumeToken => true,
        mongodb::error::ErrorKind::Command(command_error) => {
            matches!(command_error.code, 237 | 260 | 280 | 286)
                || matches!(
                    command_error.code_name.as_str(),
                    "ChangeStreamFatalError" | "ChangeStreamHistoryLost" | "ResumeTokenException"
                )
                || command_error.message.contains("resume")
                || command_error.message.contains("Resume")
        }
        _ => false,
    }
}

async fn run_change_stream_task(
    database: Database,
    collection: String,
    full_document: Option<FullDocumentType>,
    collection_watcher: Arc<CollectionWatcher>,
    ready_sender: oneshot::Sender<Result<Timestamp, Error>>,
) -> Result<(), Error> {
    let mut next_open_mode = OpenMode::Fresh;
    let mut pending_resync = None;
    let mut ready_sender = Some(ready_sender);

    loop {
        let opened_stream = match open_change_stream(
            &database,
            &collection,
            &collection_watcher.checkpoint,
            full_document.clone(),
            next_open_mode,
        )
        .await
        {
            Ok(opened_stream) => opened_stream,
            Err(error) => {
                if let Some(ready_sender) = ready_sender.take() {
                    let _ = ready_sender.send(Err(anyhow!("{error:#}")));
                    return Err(error);
                }

                if matches!(next_open_mode, OpenMode::StoredCheckpoint)
                    && requires_fresh_recovery(&error)
                {
                    println!(
                        "\x1b[0;33mmongo\x1b[0m resume failed for {collection}; switching to refetch/replay recovery"
                    );
                    next_open_mode = OpenMode::Fresh;
                    pending_resync = Some(PendingResync::FreshBoundary);
                } else {
                    println!(
                        "\x1b[0;33mmongo\x1b[0m failed to reopen change stream for {collection}: {error:#}"
                    );
                }

                sleep(RECOVERY_RETRY_DELAY).await;
                continue;
            }
        };

        if let Some(ready_sender) = ready_sender.take() {
            let start_at_operation_time = opened_stream
                .start_at_operation_time
                .clone()
                .ok_or_else(|| anyhow!("change stream startup did not capture an operation time"))?;
            let _ = ready_sender.send(Ok(start_at_operation_time));
        }

        if let Some(pending_resync) = pending_resync.take() {
            let event = match pending_resync {
                PendingResync::FreshBoundary => {
                    Event::Resync(opened_stream.start_at_operation_time.clone())
                }
                PendingResync::StoredCheckpoint => Event::Resync(None),
            };
            let _ = collection_watcher.sender.send(event);
        }

        let mut change_stream = opened_stream.change_stream;

        loop {
            match change_stream.next_if_any().await {
                Ok(Some(change_event)) => {
                    collection_watcher
                        .checkpoint
                        .lock()
                        .await
                        .update_resume_token(change_stream.resume_token());

                    match classify_change_event(change_event) {
                        BufferedChange::Event(event) => {
                            let _ = collection_watcher.sender.send(event);
                        }
                        BufferedChange::Invalidate => {
                            let can_start_after = collection_watcher
                                .checkpoint
                                .lock()
                                .await
                                .mark_invalidated(change_stream.resume_token());

                            next_open_mode = if can_start_after {
                                OpenMode::StoredCheckpoint
                            } else {
                                OpenMode::Fresh
                            };
                            pending_resync = Some(if can_start_after {
                                PendingResync::StoredCheckpoint
                            } else {
                                PendingResync::FreshBoundary
                            });
                            break;
                        }
                        BufferedChange::Ignore => {}
                    }
                }
                Ok(None) => {
                    collection_watcher
                        .checkpoint
                        .lock()
                        .await
                        .update_resume_token(change_stream.resume_token());

                    if change_stream.is_alive() {
                        continue;
                    }

                    next_open_mode = OpenMode::StoredCheckpoint;
                    break;
                }
                Err(error) => {
                    collection_watcher
                        .checkpoint
                        .lock()
                        .await
                        .update_resume_token(change_stream.resume_token());

                    println!(
                        "\x1b[0;33mmongo\x1b[0m change stream for {collection} disconnected: {error:#}"
                    );

                    next_open_mode = OpenMode::StoredCheckpoint;
                    if requires_fresh_recovery_mongo(&error) {
                        next_open_mode = OpenMode::Fresh;
                        pending_resync = Some(PendingResync::FreshBoundary);
                    }
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{await_ready, StreamCheckpoint, StreamCheckpointState};
    use anyhow::{anyhow, Error};
    use bson::{doc, Timestamp};
    use mongodb::change_stream::event::{ChangeStreamEvent, ResumeToken};
    use tokio::sync::oneshot;

    fn sample_resume_token() -> ResumeToken {
        bson::from_document::<ChangeStreamEvent<bson::Document>>(doc! {
            "_id": { "_data": "825F45B7E3000000012B022C0100296E5A1004B7F5159C0F7646F4A7ED8D65B18B4D12463C5F6964006460F45D2B53D8C8A88BBA2E0004" },
            "operationType": "insert",
            "fullDocument": { "_id": 1 },
            "documentKey": { "_id": 1 },
            "ns": { "db": "test", "coll": "items" }
        })
        .unwrap()
        .id
    }

    #[tokio::test]
    async fn await_ready_returns_success() -> Result<(), Error> {
        let (sender, receiver) = oneshot::channel();
        let timestamp = Timestamp {
            time: 1,
            increment: 2,
        };
        sender.send(Ok(timestamp)).unwrap();

        assert_eq!(await_ready(receiver).await?, timestamp);
        Ok(())
    }

    #[tokio::test]
    async fn await_ready_returns_startup_error() {
        let (sender, receiver) = oneshot::channel();
        sender.send(Err(anyhow!("startup failed"))).unwrap();

        let error = await_ready(receiver).await.unwrap_err();
        assert_eq!(error.to_string(), "startup failed");
    }

    #[tokio::test]
    async fn await_ready_detects_dropped_signal() {
        let (sender, receiver) = oneshot::channel::<Result<Timestamp, Error>>();
        drop(sender);

        let error = await_ready(receiver).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "change stream startup task terminated before reporting readiness"
        );
    }

    #[test]
    fn checkpoint_state_promotes_operation_time_to_resume_token() {
        let mut checkpoint_state = StreamCheckpointState::default();
        checkpoint_state.set_checkpoint(StreamCheckpoint::OperationTime(Timestamp {
            time: 1,
            increment: 1,
        }));

        checkpoint_state.update_resume_token(Some(sample_resume_token()));

        assert!(matches!(
            checkpoint_state.checkpoint(),
            Some(StreamCheckpoint::ResumeAfter(_))
        ));
    }

    #[test]
    fn checkpoint_state_marks_invalidation_with_start_after() {
        let mut checkpoint_state = StreamCheckpointState::default();

        assert!(checkpoint_state.mark_invalidated(Some(sample_resume_token())));
        assert!(matches!(
            checkpoint_state.checkpoint(),
            Some(StreamCheckpoint::StartAfter(_))
        ));
    }
}
