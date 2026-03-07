# DDP Router Meteor Repro

This is a standard Meteor app rigged to use the Rust `ddp-router` as a proxy for DDP subscriptions. It acts as a clear reproduction and demo of the architecture for the Meteor core team.

## How to run:

1. **Start the Meteor Application (and local MongoDB)**
   Open a terminal in this directory (`examples/meteor-repro`) and run:
   ```powershell
   ./run.ps1
   ```
   *(Or manually set `DISABLE_SOCKJS=true` and `DDP_DEFAULT_CONNECTION_URL=http://127.0.0.1:4000` then run `meteor`)*

   This will spin up the Meteor app on port 3000 and its internal MongoDB replica set on port 3001.

2. **Start the DDP Router**
   In a separate terminal, from the root of the `ddp-router` repository, run:
   ```powershell
   ./examples/meteor-repro/run-router.ps1
   ```
   *(Or set `$env:MONGO_URL="mongodb://127.0.0.1:3001/meteor?replicaSet=meteor"` and run `cargo run --release`)*

   The router will start on port 4000 and connect to Meteor's MongoDB.

3. **Test the connection**
   Open [http://localhost:3000](http://localhost:3000) in your browser.
   The browser will retrieve the UI assets from the Meteor Node.js process (port 3000), but its DDP connection for real-time subscription data will point to `ws://127.0.0.1:4000/websocket` (handled by the Rust router).

## Architecture Details
* We patched `Meteor.publish` in `server/main.js` to return cursor descriptions so the Rust router can intercept and take over subscription handling.
* Subscriptions for the `links` collection bypass Node.js and are handled entirely via MongoDB Change Streams and polling directly inside the Rust DDP Router.
