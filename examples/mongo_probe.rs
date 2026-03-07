use anyhow::Result;
use mongodb::bson::{doc, Document};
use mongodb::Client;
use tokio::time::{timeout, Duration};

#[tokio::main]
async fn main() -> Result<()> {
    let mongo_url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "mongodb://localhost:27017".to_owned());

    let client = Client::with_uri_str(&mongo_url).await?;
    let database = client.database("admin");
    let collection = client
        .database("ddp_router_probe")
        .collection::<Document>("items");

    println!("hello:start");
    match timeout(Duration::from_secs(5), database.run_command(doc! { "hello": 1 }, None)).await {
        Ok(Ok(response)) => println!("hello:ok {:?}", response),
        Ok(Err(error)) => println!("hello:err {error:#}"),
        Err(_) => println!("hello:timeout"),
    }

    println!("session:start");
    match timeout(Duration::from_secs(5), client.start_session(None)).await {
        Ok(Ok(mut session)) => {
            println!("session:ok");
            println!("session-ping:start");
            match timeout(
                Duration::from_secs(5),
                database.run_command_with_session(doc! { "ping": 1 }, None, &mut session),
            )
            .await
            {
                Ok(Ok(response)) => {
                    println!("session-ping:ok {:?}", response);
                    println!("session-operation-time:{:?}", session.operation_time());
                }
                Ok(Err(error)) => println!("session-ping:err {error:#}"),
                Err(_) => println!("session-ping:timeout"),
            }
        }
        Ok(Err(error)) => println!("session:err {error:#}"),
        Err(_) => println!("session:timeout"),
    }

    println!("watch:start");
    match timeout(Duration::from_secs(5), collection.watch([], None)).await {
        Ok(Ok(_)) => println!("watch:ok"),
        Ok(Err(error)) => println!("watch:err {error:#}"),
        Err(_) => println!("watch:timeout"),
    }

    Ok(())
}
