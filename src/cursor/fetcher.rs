use super::description::CursorDescription;
use super::viewer::CursorViewer;
use crate::ejson::into_ejson_document;
use crate::mergebox::{Mergebox, Mergeboxes};
use crate::watcher::{Event, WatchSubscription, Watcher};
use anyhow::{anyhow, Context, Error};
use bson::{Document, Timestamp};
use futures_util::{StreamExt, TryStreamExt};
use mongodb::Database;
use serde_json::{Map, Value};
use std::mem::{replace, take};
use std::sync::Arc;
use tokio::sync::broadcast::error::TryRecvError;
use tokio::sync::broadcast::Receiver;
use tokio::sync::Mutex;
use tokio::time::{interval_at, Duration, Instant, Interval};

#[derive(Debug, Eq, PartialEq)]
enum FetchPhase {
    Querying,
    Replaying,
    Steady,
}

#[derive(Debug)]
struct FetchState {
    buffered_events: Vec<Event>,
    phase: FetchPhase,
}

impl Default for FetchState {
    fn default() -> Self {
        Self {
            buffered_events: Vec::default(),
            phase: FetchPhase::Steady,
        }
    }
}

impl FetchState {
    fn buffer(&mut self, event: Event) {
        self.buffered_events.push(event);
    }

    fn clear(&mut self) {
        self.buffered_events.clear();
    }

    fn finish_steady(&mut self) {
        self.phase = FetchPhase::Steady;
    }

    fn start_querying(&mut self) {
        self.buffered_events.clear();
        self.phase = FetchPhase::Querying;
    }

    fn start_replay(&mut self) {
        self.phase = FetchPhase::Replaying;
    }

    fn take_buffered(&mut self) -> Vec<Event> {
        take(&mut self.buffered_events)
    }
}

enum DrainOutcome {
    Lagged,
    Ready,
    Resync(Option<Timestamp>),
}

pub struct CursorFetcher {
    database: Database,
    description: CursorDescription,
    documents: Vec<Map<String, Value>>,
    fetch_state: FetchState,
    viewer: Option<CursorViewer>,
    watcher: Arc<Mutex<Watcher>>,
}

impl CursorFetcher {
    async fn drain_buffered_events(
        &mut self,
        receiver: &mut Receiver<Event>,
    ) -> Result<DrainOutcome, Error> {
        loop {
            match receiver.try_recv() {
                Ok(Event::Resync(boundary)) => {
                    self.fetch_state.clear();
                    return Ok(DrainOutcome::Resync(boundary));
                }
                Ok(event) => self.fetch_state.buffer(event),
                Err(TryRecvError::Closed) => {
                    return Err(anyhow!("change stream receiver closed"));
                }
                Err(TryRecvError::Empty) => return Ok(DrainOutcome::Ready),
                Err(TryRecvError::Lagged(_)) => {
                    self.fetch_state.clear();
                    return Ok(DrainOutcome::Lagged);
                }
            }
        }
    }

    async fn fetch_documents(
        &self,
        boundary: Option<Timestamp>,
    ) -> Result<Vec<Map<String, Value>>, Error> {
        let collection = self
            .database
            .collection::<Document>(&self.description.collection);

        if let Some(boundary) = boundary {
            let mut session = collection.client().start_session(None).await?;
            session.advance_operation_time(boundary);

            let mut cursor = collection
                .find_with_session(
                    Some(self.description.selector.clone()),
                    Some(self.description.as_find_options()),
                    &mut session,
                )
                .await?;

            cursor
                .stream(&mut session)
                .map(|maybe_document| maybe_document.map(into_ejson_document))
                .try_collect()
                .await
                .map_err(Into::into)
        } else {
            collection
                .find(
                    Some(self.description.selector.clone()),
                    Some(self.description.as_find_options()),
                )
                .await?
                .map(|maybe_document| maybe_document.map(into_ejson_document))
                .try_collect()
                .await
                .map_err(Into::into)
        }
    }

    pub async fn fetch_snapshot(
        &mut self,
        mergeboxes: &Arc<Mutex<Mergeboxes>>,
        boundary: Option<Timestamp>,
    ) -> Result<(), Error> {
        println!("\x1b[0;32mmongo\x1b[0m fetch({:?})", self.description);

        let mut documents = self.fetch_documents(boundary).await?;
        let mut mergeboxes = mergeboxes.lock().await;

        for document in &mut documents {
            let id = extract_id(document)?;
            mergeboxes
                .insert(
                    self.description.collection.clone(),
                    id.clone(),
                    document.clone(),
                )
                .await?;
            document.insert(String::from("_id"), id);
        }

        for mut document in replace(&mut self.documents, documents) {
            let id = extract_id(&mut document)?;
            mergeboxes
                .remove(self.description.collection.clone(), id.clone(), &document)
                .await?;
        }

        Ok(())
    }

    pub async fn handle_event(
        &mut self,
        event: Event,
        receiver: &mut Receiver<Event>,
        mergeboxes: &Arc<Mutex<Mergeboxes>>,
    ) -> Result<(), Error> {
        match event {
            Event::Resync(boundary) => self.refetch_and_replay(receiver, mergeboxes, boundary).await,
            event => {
                let refetch = process(
                    event,
                    &self.description,
                    &mut self.documents,
                    mergeboxes,
                    self.viewer.as_ref().unwrap(),
                )
                .await
                .context("CursorFetcher::handle_event")?;

                if refetch {
                    self.refetch_and_replay(receiver, mergeboxes, None)
                        .await
                        .context("CursorFetcher::handle_event (refetch)")?;
                }

                Ok(())
            }
        }
    }

    pub fn new(
        database: Database,
        description: CursorDescription,
        watcher: Arc<Mutex<Watcher>>,
    ) -> Self {
        let viewer = match CursorViewer::try_from(&description) {
            Ok(viewer) => Some(viewer),
            Err(error) => {
                println!("\x1b[0;32mmongo\x1b[0m \x1b[0;31m{error}\x1b[0m");
                None
            }
        };

        Self {
            database,
            description,
            documents: Vec::default(),
            fetch_state: FetchState::default(),
            viewer,
            watcher,
        }
    }

    pub async fn refetch_and_replay(
        &mut self,
        receiver: &mut Receiver<Event>,
        mergeboxes: &Arc<Mutex<Mergeboxes>>,
        mut boundary: Option<Timestamp>,
    ) -> Result<(), Error> {
        'retry: loop {
            self.fetch_state.start_querying();
            self.fetch_snapshot(mergeboxes, boundary.clone())
                .await
                .context("CursorFetcher::refetch_and_replay (fetch)")?;

            match self
                .drain_buffered_events(receiver)
                .await
                .context("CursorFetcher::refetch_and_replay (drain)")?
            {
                DrainOutcome::Lagged => {
                    boundary = None;
                    continue;
                }
                DrainOutcome::Ready => {}
                DrainOutcome::Resync(next_boundary) => {
                    boundary = next_boundary;
                    continue;
                }
            }

            self.fetch_state.start_replay();
            for event in self.fetch_state.take_buffered() {
                let refetch = process(
                    event,
                    &self.description,
                    &mut self.documents,
                    mergeboxes,
                    self.viewer.as_ref().unwrap(),
                )
                .await
                .context("CursorFetcher::refetch_and_replay (replay)")?;

                if refetch {
                    boundary = None;
                    continue 'retry;
                }
            }

            self.fetch_state.finish_steady();
            return Ok(());
        }
    }

    pub async fn register(&self, mergebox: &Arc<Mutex<Mergebox>>) -> Result<(), Error> {
        let mut mergebox = mergebox.lock().await;
        for mut document in self.documents.clone() {
            let id = extract_id(&mut document)?;
            mergebox
                .insert(self.description.collection.clone(), id, document)
                .await
                .context("CursorFetcher::register")?;
        }

        Ok(())
    }

    pub async fn watch(&self) -> Result<WatchSubscription, Interval> {
        if self.viewer.is_some() {
            let mut watcher = self.watcher.lock().await;
            match watcher.watch(self.description.collection.clone()).await {
                Ok(subscription) => Ok(subscription),
                Err(error) => {
                    println!("\x1b[0;32mmongo\x1b[0m \x1b[0;31m{error:?}\x1b[0m");

                    let interval = self.description.polling_interval_ms.unwrap_or(10_000);
                    let duration = Duration::from_millis(interval);
                    Err(interval_at(Instant::now() + duration, duration))
                }
            }
        } else {
            let interval = self.description.polling_interval_ms.unwrap_or(10_000);
            let duration = Duration::from_millis(interval);
            Err(interval_at(Instant::now() + duration, duration))
        }
    }

    pub async fn unregister(&self, mergebox: &Arc<Mutex<Mergebox>>) -> Result<(), Error> {
        let mut mergebox = mergebox.lock().await;
        for mut document in self.documents.clone() {
            let id = extract_id(&mut document)?;
            mergebox
                .remove(self.description.collection.clone(), id, &document)
                .await
                .context("CursorFetcher::unregister")?;
        }

        Ok(())
    }
}

fn extract_id(document: &mut Map<String, Value>) -> Result<Value, Error> {
    document
        .remove("_id")
        .ok_or_else(|| anyhow!("_id not found in {document:?}"))
}

async fn process(
    event: Event,
    description: &CursorDescription,
    documents: &mut Vec<Map<String, Value>>,
    mergeboxes: &Arc<Mutex<Mergeboxes>>,
    viewer: &CursorViewer,
) -> Result<bool, Error> {
    match event {
        Event::Clear => {
            let mut mergeboxes = mergeboxes.lock().await;
            for mut document in take(documents) {
                let id = extract_id(&mut document)?;
                viewer.projector.apply(&mut document);
                mergeboxes
                    .remove(description.collection.clone(), id, &document)
                    .await
                    .context("process -> Event::Clear")?;
            }
            Ok(false)
        }
        Event::Delete(document) => {
            let mut document = into_ejson_document(document);
            let id = extract_id(&mut document)?;
            let Some(index) = documents.iter().position(|x| x.get("_id") == Some(&id)) else {
                return Ok(false);
            };

            if description
                .limit()
                .is_some_and(|limit| limit == documents.len())
            {
                return Ok(true);
            }

            let mut document = documents.swap_remove(index);
            document.remove("_id");
            viewer.projector.apply(&mut document);
            mergeboxes
                .lock()
                .await
                .remove(description.collection.clone(), id, &document)
                .await
                .context("process -> Event::Delete")?;

            Ok(false)
        }
        Event::Insert(document) => {
            let mut document = into_ejson_document(document);
            if !viewer.matcher.matches(&document) {
                return Ok(false);
            }

            if let Some(index) = {
                let id = document.get("_id");
                documents.iter().position(|x| x.get("_id") == id)
            } {
                if description.limit().is_some() {
                    documents.remove(index);
                } else {
                    documents.swap_remove(index);
                }
            }

            if let Some(limit) = description.limit() {
                let index = documents
                    .binary_search_by(|x| viewer.sorter.cmp(x, &document))
                    .unwrap_or_else(|index| index);
                if index == limit {
                    return Ok(false);
                }

                documents.insert(index, document.clone());
            } else {
                documents.push(document.clone());
            }

            let id = extract_id(&mut document)?;
            viewer.projector.apply(&mut document);
            let mut mergeboxes = mergeboxes.lock().await;
            mergeboxes
                .insert(description.collection.clone(), id, document)
                .await
                .context("process -> Event::Insert")?;

            if let Some(limit) = description.limit() {
                if documents.len() > limit {
                    if let Some(mut document) = documents.pop() {
                        let id = extract_id(&mut document)?;
                        viewer.projector.apply(&mut document);
                        mergeboxes
                            .remove(description.collection.clone(), id, &document)
                            .await
                            .context("process -> Event::Insert")?;
                    }
                }
            }

            Ok(false)
        }
        Event::Resync(_) => Ok(true),
        Event::Update(document) => {
            let mut document = into_ejson_document(document);
            let is_matching = viewer.matcher.matches(&document);
            if is_matching {
                let index_before = {
                    let id = document.get("_id");
                    documents.iter().position(|x| x.get("_id") == id)
                };

                if let Some(limit) = description.limit() {
                    let index = documents
                        .binary_search_by(|x| viewer.sorter.cmp(x, &document))
                        .unwrap_or_else(|index| index);

                    if index == limit {
                        return Ok(false);
                    }

                    documents.insert(index, document.clone());
                } else {
                    documents.push(document.clone());
                }

                let id = extract_id(&mut document)?;
                viewer.projector.apply(&mut document);
                let mut mergeboxes = mergeboxes.lock().await;
                mergeboxes
                    .insert(description.collection.clone(), id.clone(), document)
                    .await
                    .context("process -> Event::Update")?;

                if let Some(index) = index_before {
                    let mut document = if description.limit().is_some() {
                        documents.remove(index)
                    } else {
                        documents.swap_remove(index)
                    };

                    document.remove("_id");
                    viewer.projector.apply(&mut document);
                    mergeboxes
                        .remove(description.collection.clone(), id, &document)
                        .await
                        .context("process -> Event::Update")?;
                }
            } else {
                let id = extract_id(&mut document)?;
                let Some(index) = documents.iter().position(|x| x.get("_id") == Some(&id)) else {
                    return Ok(false);
                };

                let mut document = match description.limit() {
                    Some(limit) => {
                        if limit == documents.len() {
                            return Ok(true);
                        };

                        documents.remove(index)
                    }
                    None => documents.swap_remove(index),
                };

                let id = extract_id(&mut document)?;
                viewer.projector.apply(&mut document);
                mergeboxes
                    .lock()
                    .await
                    .remove(description.collection.clone(), id, &document)
                    .await
                    .context("process -> Event::Update")?;
            }

            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{process, CursorDescription, CursorViewer, FetchPhase, FetchState};
    use crate::ddp::DDPMessage;
    use crate::mergebox::{Mergebox, Mergeboxes};
    use crate::watcher::Event;
    use anyhow::Error;
    use bson::doc;
    use serde::Deserialize;
    use serde_json::{json, Value};
    use std::sync::Arc;
    use tokio::sync::mpsc::channel;
    use tokio::sync::Mutex;
    use tokio::test;

    async fn simulate(
        description: Value,
        events: Vec<Event>,
        messages: Vec<DDPMessage>,
    ) -> Result<(), Error> {
        let description = CursorDescription::deserialize(description)?;
        let viewer = CursorViewer::try_from(&description)?;
        let (sender, mut receiver) = channel(64);
        let mergeboxes = Arc::new(Mutex::new({
            let mut mergeboxes = Mergeboxes::default();
            mergeboxes.insert_mergebox(1, &Arc::new(Mutex::new(Mergebox::new(sender))));
            mergeboxes
        }));

        let mut documents = Vec::new();
        for event in events {
            process(event, &description, &mut documents, &mergeboxes, &viewer).await?;
        }

        for message in messages {
            assert_eq!(receiver.try_recv(), Ok(message));
        }

        assert!(receiver.try_recv().is_err());
        assert!(documents.is_empty());

        Ok(())
    }

    macro_rules! simulate {
        ($name:ident, $description:expr, $events:expr, $messages:expr) => {
            #[test]
            async fn $name() -> Result<(), Error> {
                simulate($description, $events, $messages).await
            }
        };
    }

    macro_rules! json_doc {
        ($($json:tt)*) => {
            match json! {{ $($json)* }} {
                Value::Object(map) => map,
                _ => unreachable!(),
            }
        };
    }

    simulate!(
        scenario_1,
        json! {{"collectionName": "x", "selector": {}, "options": {}}},
        vec![],
        vec![]
    );

    simulate!(
        scenario_2,
        json! {{"collectionName": "x", "selector": {}, "options": {}}},
        vec![
            Event::Insert(doc! {"_id": 1}),
            Event::Insert(doc! {"_id": 2, "a": 3}),
            Event::Clear
        ],
        vec![
            DDPMessage::Added {
                collection: "x".to_owned(),
                id: json!(1),
                fields: None,
                cleared: None,
            },
            DDPMessage::Added {
                collection: "x".to_owned(),
                id: json!(2),
                fields: Some(json_doc! {"a": 3}),
                cleared: None,
            },
            DDPMessage::Removed {
                collection: "x".to_owned(),
                id: json!(1)
            },
            DDPMessage::Removed {
                collection: "x".to_owned(),
                id: json!(2)
            }
        ]
    );

    #[test]
    async fn fetch_state_tracks_query_replay_steady_transitions() {
        let mut fetch_state = FetchState::default();
        assert_eq!(fetch_state.phase, FetchPhase::Steady);

        fetch_state.start_querying();
        fetch_state.buffer(Event::Clear);
        assert_eq!(fetch_state.phase, FetchPhase::Querying);
        assert_eq!(fetch_state.buffered_events.len(), 1);

        fetch_state.start_replay();
        assert_eq!(fetch_state.phase, FetchPhase::Replaying);

        assert_eq!(fetch_state.take_buffered(), vec![Event::Clear]);
        fetch_state.finish_steady();
        assert_eq!(fetch_state.phase, FetchPhase::Steady);
    }

    #[test]
    async fn fetch_state_restart_clears_buffer() {
        let mut fetch_state = FetchState::default();
        fetch_state.start_querying();
        fetch_state.buffer(Event::Clear);
        fetch_state.start_querying();

        assert_eq!(fetch_state.phase, FetchPhase::Querying);
        assert!(fetch_state.buffered_events.is_empty());
    }

    #[test]
    async fn duplicate_insert_reuses_existing_document() -> Result<(), Error> {
        let description = CursorDescription::deserialize(
            json! {{"collectionName": "x", "selector": {}, "options": {}}},
        )?;
        let viewer = CursorViewer::try_from(&description)?;
        let (sender, mut receiver) = channel(64);
        let mergeboxes = Arc::new(Mutex::new({
            let mut mergeboxes = Mergeboxes::default();
            mergeboxes.insert_mergebox(1, &Arc::new(Mutex::new(Mergebox::new(sender))));
            mergeboxes
        }));

        let mut documents = vec![json_doc! {"_id": 1, "a": 1}];
        mergeboxes
            .lock()
            .await
            .insert("x".to_owned(), json!(1), json_doc! {"a": 1})
            .await?;
        assert_eq!(
            receiver.try_recv(),
            Ok(DDPMessage::Added {
                collection: "x".to_owned(),
                id: json!(1),
                fields: Some(json_doc! {"a": 1}),
                cleared: None,
            })
        );

        process(
            Event::Insert(doc! {"_id": 1, "a": 1}),
            &description,
            &mut documents,
            &mergeboxes,
            &viewer,
        )
        .await?;

        assert!(receiver.try_recv().is_err());
        assert_eq!(documents, vec![json_doc! {"_id": 1, "a": 1}]);

        Ok(())
    }
}
