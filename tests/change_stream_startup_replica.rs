use anyhow::{anyhow, Result};
use ddp_router::cursor::{Cursor, CursorDescription};
use ddp_router::ddp::DDPMessage;
use ddp_router::mergebox::Mergebox;
use ddp_router::watcher::Watcher;
use mongodb::bson::{doc, Document};
use mongodb::options::FullDocumentType;
use mongodb::Client;
use std::collections::BTreeSet;
use std::env;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc::channel;
use tokio::sync::Mutex;
use tokio::time::{sleep, timeout, Duration, Instant};

fn unique_database_name() -> String {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
    format!("ddp_router_startup_{suffix}")
}

#[tokio::test]
#[ignore = "requires DDP_ROUTER_TEST_MONGO_URL to point at a MongoDB replica set"]
async fn cursor_start_replays_writes_during_initial_fetch() -> Result<()> {
    let mongo_url = env::var("DDP_ROUTER_TEST_MONGO_URL").map_err(|_| {
        anyhow!("DDP_ROUTER_TEST_MONGO_URL must point at a MongoDB replica set for this test")
    })?;

    let client = Client::with_uri_str(mongo_url).await?;
    let database = client.database(&unique_database_name());
    let collection = database.collection::<Document>("items");

    let payload = "x".repeat(2048);
    for batch in 0..20 {
        let noise_documents: Vec<_> = (0..500)
            .map(|offset| {
                let index = batch * 500 + offset;
                doc! {
                    "_id": format!("noise-{index}"),
                    "kind": "noise",
                    "payload": &payload,
                }
            })
            .collect();
        collection.insert_many(noise_documents, None).await?;
    }

    collection
        .insert_one(doc! { "_id": "existing", "kind": "watched", "seq": -1 }, None)
        .await?;

    let watcher = Arc::new(Mutex::new(Watcher::new(
        database.clone(),
        Some(FullDocumentType::UpdateLookup),
    )));
    let description = CursorDescription {
        collection: "items".to_owned(),
        disable_oplog: false,
        limit: None,
        polling_interval_ms: None,
        projection: None,
        selector: doc! { "kind": "watched" },
        skip: None,
        sort: None,
        transform: None,
    };

    let mut cursor = Cursor::new(database.clone(), description, watcher);
    let (sender, mut receiver) = channel(128);
    let mergebox = Arc::new(Mutex::new(Mergebox::new(sender)));

    let inserter = {
        let collection = collection.clone();
        tokio::spawn(async move {
            sleep(Duration::from_millis(25)).await;
            for index in 0..20 {
                collection
                    .insert_one(
                        doc! {
                            "_id": format!("watched-{index}"),
                            "kind": "watched",
                            "seq": index as i32,
                        },
                        None,
                    )
                    .await?;
                sleep(Duration::from_millis(5)).await;
            }
            Ok::<_, mongodb::error::Error>(())
        })
    };

    timeout(Duration::from_secs(30), cursor.start(1, &mergebox))
        .await
        .map_err(|_| anyhow!("cursor.start timed out"))??;
    inserter.await??;

    let mut expected = BTreeSet::from([String::from("existing")]);
    expected.extend((0..20).map(|index| format!("watched-{index}")));

    let deadline = Instant::now() + Duration::from_secs(15);
    let mut seen = BTreeSet::new();
    while seen != expected {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }

        let message = match timeout(remaining, receiver.recv()).await {
            Ok(Some(message)) => message,
            Ok(None) => return Err(anyhow!("mergebox channel closed before all documents arrived")),
            Err(_) => break,
        };

        match message {
            DDPMessage::Added { id, .. } | DDPMessage::Changed { id, .. } => {
                if let Some(id) = id.as_str() {
                    if expected.contains(id) {
                        seen.insert(id.to_owned());
                    }
                }
            }
            DDPMessage::MovedBefore { .. }
            | DDPMessage::Ping { .. }
            | DDPMessage::Pong { .. }
            | DDPMessage::Ready { .. }
            | DDPMessage::Removed { .. }
            | DDPMessage::AddedBefore { .. }
            | DDPMessage::Connect { .. }
            | DDPMessage::Connected { .. }
            | DDPMessage::Failed { .. }
            | DDPMessage::Nosub { .. }
            | DDPMessage::Sub { .. }
            | DDPMessage::Unsub { .. }
            | DDPMessage::Method { .. }
            | DDPMessage::Result { .. }
            | DDPMessage::Updated { .. } => {}
        }
    }

    timeout(Duration::from_secs(30), cursor.stop(1, &mergebox))
        .await
        .map_err(|_| anyhow!("cursor.stop timed out"))??;
    let _ = timeout(
        Duration::from_secs(5),
        database.run_command(doc! { "dropDatabase": 1 }, None),
    )
    .await;

    assert_eq!(seen, expected);
    Ok(())
}
