use tokio::sync::RwLock;

#[tokio::main]
async fn main() {
    let lock = tokio::sync::RwLock::new(5);
    match *lock.read().await {
        x => {
            println!("Got {}, trying to write...", x);
            *lock.write().await = 6;
            println!("Done.");
        }
    }
}
