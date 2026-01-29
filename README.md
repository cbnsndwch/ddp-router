# DDP Router

A high-performance Rust implementation of MeteorJS's real-time data synchronization layer. DDP Router acts as a proxy between web clients and a Meteor server, offloading the computationally expensive subscription management, oplog tailing, and Mergebox operations from Node.js to native Rust code.

## Table of Contents

- [Overview](#overview)
- [Architecture](#architecture)
- [Core Concepts](#core-concepts)
- [Can I Use This Outside Meteor?](#can-i-use-this-outside-meteor)
- [Installation](#installation)
- [Configuration](#configuration)
- [Quick Start with Meteor](#quick-start-with-meteor)
- [How It Works](#how-it-works)
- [Module Reference](#module-reference)
- [Supported Query Features](#supported-query-features)
- [Limitations and Known Issues](#limitations-and-known-issues)
- [License](#license)

---

## Overview

DDP Router is a WebSocket proxy that implements the [Distributed Data Protocol (DDP)](https://github.com/meteor/meteor/blob/devel/packages/ddp/DDP.md) used by Meteor. It intercepts subscription requests from clients and manages them directly against MongoDB, using:

- **MongoDB Change Streams** for real-time updates (when possible)
- **Polling fallback** for complex queries that can't be processed in real-time
- **Mergebox** for efficient document deduplication across multiple subscriptions
- **Native query matching, sorting, and projection** implemented in Rust

### Why Use DDP Router?

| Benefit                  | Description                                                                         |
| ------------------------ | ----------------------------------------------------------------------------------- |
| **Performance**          | Rust's memory efficiency and speed can handle thousands of concurrent subscriptions |
| **Offloading**           | Moves CPU-intensive subscription logic away from Node.js                            |
| **Scalability**          | Stateless design allows horizontal scaling                                          |
| **Cursor Deduplication** | Identical cursors across sessions share a single MongoDB Change Stream              |

---

## Architecture

```
┌────────────────────────────────────────────────────────────────────┐
│                           DDP Router                               │
├────────────────────────────────────────────────────────────────────┤
│                                                                    │
│  ┌──────────┐     ┌───────────┐     ┌────────────┐                 │
│  │ Session  │────►│ Mergebox  │────►│ DDP Client │                 │
│  │ Manager  │     └───────────┘     │ (Browser)  │                 │
│  └────┬─────┘                       └────────────┘                 │
│       │                                                            │
│       ▼                                                            │
│  ┌──────────────────────────────────────────────────────────────┐  │
│  │                    Subscriptions                             │  │
│  │  ┌───────────────────────────────────────────────────────┐   │  │
│  │  │                     Cursors                           │   │  │
│  │  │  ┌─────────┐  ┌───────────┐  ┌─────────┐  ┌────────┐  │   │  │
│  │  │  │ Matcher │  │ Projector │  │ Sorter  │  │Fetcher │  │   │  │
│  │  │  └─────────┘  └───────────┘  └─────────┘  └────────┘  │   │  │
│  │  └───────────────────────────────────────────────────────┘   │  │
│  └──────────────────────────────────────────────────────────────┘  │
│       │                                                            │
│       ▼                                                            │
│  ┌──────────┐     ┌────────────────────────────────────────┐       │
│  │ Watcher  │────►│ MongoDB (Change Streams / Polling)     │       │
│  └──────────┘     └────────────────────────────────────────┘       │
│                                                                    │
│       │                                                            │
│       ▼                                                            │
│  ┌──────────────┐                                                  │
│  │ Meteor Server│ (for methods, auth, unsupported publications)    │
│  └──────────────┘                                                  │
│                                                                    │
└────────────────────────────────────────────────────────────────────┘
```

---

## Core Concepts

### DDP (Distributed Data Protocol)

DDP is the WebSocket-based protocol Meteor uses for real-time data synchronization. It defines messages for:

- **Subscriptions**: `sub`, `unsub`, `nosub`, `ready`
- **Data changes**: `added`, `changed`, `removed`
- **RPC**: `method`, `result`, `updated`
- **Connection**: `connect`, `connected`, `ping`, `pong`

### Mergebox

The Mergebox is a critical component that:

1. **Tracks document state** across multiple overlapping subscriptions
2. **Deduplicates data** so each document field is only sent to the client once
3. **Computes deltas** when documents change (sending only `changed` messages, not full replacements)
4. **Reference counts** documents to know when to send `removed`

### Cursor & CursorDescription

A cursor represents a MongoDB query with:

- `collection`: The MongoDB collection name
- `selector`: Query filter document
- `projection`: Field inclusion/exclusion
- `sort`: Sort specification
- `limit`: Maximum documents to return
- `skip`: Documents to skip (limited support)

### Watcher

The Watcher manages MongoDB Change Streams:

- Shares a single change stream per collection across all cursors
- Broadcasts events (`insert`, `update`, `delete`, `drop`) to subscribed cursors
- Each cursor filters events locally using the compiled Matcher

---

## Can I Use This Outside Meteor?

### Short Answer: **Partially, with significant effort**

DDP Router is designed as a **proxy for Meteor applications** and relies on:

1. **A Meteor server** to handle:
    - Authentication and method calls
    - Low-level publications (`this.added`, `this.changed`, etc.)
    - The `__subscription__` method extraction pattern

2. **DDP protocol compliance** from both client and server

### Standalone Usage Scenarios

| Use Case                                  | Feasibility                                                                 |
| ----------------------------------------- | --------------------------------------------------------------------------- |
| **Replace Meteor entirely**               | ❌ Not possible without major rewrites                                      |
| **Custom DDP client + MongoDB**           | ⚠️ Possible, but you'd need to implement subscription registration yourself |
| **Extract components for other projects** | ✅ The following modules are standalone-capable:                            |

### Reusable Modules (No Meteor Dependency)

These modules implement MongoDB-compatible logic in pure Rust and can be extracted:

| Module         | Purpose                                    | Dependencies                   |
| -------------- | ------------------------------------------ | ------------------------------ |
| `matcher.rs`   | MongoDB query selector matching            | `bson`, `regex`, `serde_json`  |
| `projector.rs` | Field projection (include/exclude)         | `bson`, `serde_json`           |
| `sorter.rs`    | Document sorting with BSON type ordering   | `bson`, `serde_json`           |
| `lookup.rs`    | Nested field lookup (dot notation)         | `serde_json`                   |
| `ejson.rs`     | BSON ↔ EJSON conversion                    | `bson`, `serde_json`, `base64` |
| `watcher.rs`   | MongoDB Change Stream wrapper              | `mongodb`, `tokio`             |
| `mergebox.rs`  | Document deduplication & delta computation | `serde_json`, `tokio`          |

### Building a Standalone System

If you want to build a non-Meteor real-time sync system, you would need to:

```rust
// Pseudocode for standalone usage
use ddp_router::{matcher::DocumentMatcher, projector::Projector, sorter::Sorter, watcher::Watcher};

// 1. Connect to MongoDB
let database = mongodb::Client::with_uri_str("mongodb://...").await?.database("my-db");

// 2. Create a watcher for your collection
let watcher = Watcher::new(database.clone());
let mut receiver = watcher.watch("users".to_string()).await;

// 3. Compile your query components
let matcher = DocumentMatcher::compile(&doc! { "status": "active" })?;
let sorter = Sorter::compile(Some(&doc! { "createdAt": -1 }))?;
let projector = Projector::compile(Some(&doc! { "password": 0 }))?;

// 4. Process change events
while let Ok(event) = receiver.recv().await {
    match event {
        Event::Insert(doc) => {
            let ejson_doc = ejson::into_ejson_document(doc);
            if matcher.matches(&ejson_doc) {
                let mut output = ejson_doc.clone();
                projector.apply(&mut output);
                // Send to your clients...
            }
        }
        // Handle Update, Delete, etc.
    }
}
```

---

## Installation

### Prerequisites

- Rust 1.79.0+ (edition 2021)
- MongoDB 4.0+ (for Change Streams support)
- A Meteor application (for full functionality)

### Build from Source

```bash
git clone https://github.com/meteor/ddp-router.git
cd ddp-router

# Debug build
cargo build

# Release build (recommended for production)
cargo build --release
```

### Run

```bash
# Using cargo
cargo run --release

# Or run the binary directly
./target/release/ddp-router
```

---

## Configuration

DDP Router can be configured via `config.toml` or environment variables.

### Configuration File (`config.toml`)

```toml
# Meteor server WebSocket URL
meteor.url = "ws://127.0.0.1:3000/websocket"

# MongoDB connection string (must include database name)
mongo.url = "mongodb://localhost:27017/meteor"

# DDP Router listen address
router.url = "127.0.0.1:4000"
```

### Environment Variables

Environment variables use underscore (`_`) as separator:

```bash
export METEOR_URL="ws://127.0.0.1:3000/websocket"
export MONGO_URL="mongodb://localhost:27017/meteor"
export ROUTER_URL="127.0.0.1:4000"
```

---

## Quick Start with Meteor

### Step 1: Patch Your Meteor Application

Add this code **before** any `Meteor.publish` calls (e.g., in a file that loads early):

```typescript
import { Meteor } from 'meteor/meteor';
import { NpmModuleMongodb } from 'meteor/npm-mongo';

const { publish } = Meteor;

Meteor.publish = function publishWithDDPRouter(name, fn) {
    // Create a method that returns cursor descriptions
    Meteor.methods({
        async [`__subscription__${name}`]() {
            const context = { ...this, ready() {}, unblock() {} };
            const maybeCursorOrCursors = await fn.apply(context, arguments);

            const cursors = Array.isArray(maybeCursorOrCursors)
                ? maybeCursorOrCursors
                : maybeCursorOrCursors
                  ? [maybeCursorOrCursors]
                  : [];

            const cursorDescriptions = cursors.map((cursor) => {
                const cursorDescription = cursor._cursorDescription;
                if (!cursorDescription) {
                    console.error('Expected a cursor, got:', cursor);
                    throw new Error('CursorExpectedError');
                }
                return cursorDescription;
            });

            // Use BSON's EJSON to avoid Meteor's serialization
            return NpmModuleMongodb.BSON.EJSON.stringify(cursorDescriptions);
        },
    });

    return publish.apply(this, arguments);
};
```

### Step 2: Configure Meteor Environment

```bash
# Disable SockJS (use raw WebSocket)
export DISABLE_SOCKJS=true

# Point clients to DDP Router
export DDP_DEFAULT_CONNECTION_URL=127.0.0.1:4000
```

### Step 3: Handle ObjectId Fields (if needed)

If your collections use MongoDB `ObjectId` in the `_id` field:

```typescript
import { MongoID } from 'meteor/mongo-id';

const idParse = MongoID.idParse;
MongoID.idParse = (id) =>
    id?.$type === 'oid' ? idParse(id.$value) : idParse(id);
```

### Step 4: Start Everything

```bash
# Terminal 1: Start MongoDB
mongod

# Terminal 2: Start Meteor
meteor run

# Terminal 3: Start DDP Router
cargo run --release
```

---

## How It Works

### Subscription Flow

```
Client                    DDP Router                       Meteor Server
  │                            │                                 │
  │────── sub {id, name} ─────►│                                 │
  │                            │                                 │
  │                            │── method `__subscription__X` ──►│
  │                            │                                 │
  │                            │◄─── result {cursors} ────────── │
  │                            │ [Parse cursor descriptions]     │
  │                            │ [Execute MongoDB query]         │
  │                            │ [Start Change Stream/Polling]   │
  │                            │                                 │
  │◄──── added {doc1} ─────────│                                 │
  │◄──── added {doc2} ─────────│                                 │
  │◄──── ready {id} ───────────│                                 │
  │                            │                                 │
  │          ...               │                                 │
  │                            │                                 │
  │        [MongoDB Change]    │                                 │
  │◄──── changed {doc1} ───────│                                 │
  │                            │                                 │
  │          ...               │                                 │
  │                            │                                 │
  │───── unsub {id} ──────────►│                                 │
  │◄──── nosub {id} ───────────│                                 │
```

### Fallback Behavior

When DDP Router can't handle a subscription (unsupported query operators, low-level publications, errors), it transparently forwards the subscription to the Meteor server:

```
Client                    DDP Router                      Meteor Server
  │                            │                                 │
  │────── sub {id, name} ─────►│                                 │
  │                            │── method `__subscription__X` ──►│
  │                            │◄───── error (not found) ────────│
  │                            │                                 │
  │                            │──────── sub {id, name} ────────►│
  │                            │                                 │
  │                                                              │
  │◄──────────────── [proxied DDP messages] ─────────────────────│
```

---

## Module Reference

### `main.rs`

Entry point. Sets up the TCP listener, MongoDB connection, and spawns session handlers for each WebSocket connection.

### `session.rs`

Manages a single client-server session pair. Handles DDP message routing, subscription interception, and Mergebox updates.

### `ddp.rs`

Defines all DDP message types as a Rust enum with serde serialization. Implements the full DDP protocol specification.

### `subscriptions.rs`

Manages all active subscriptions across sessions. Handles cursor deduplication—identical queries share the same underlying cursor.

### `cursor/mod.rs`

Represents a single MongoDB query subscription. Contains:

- `description.rs`: Cursor configuration (collection, selector, options)
- `fetcher.rs`: Initial fetch and change processing
- `viewer.rs`: Combines Matcher + Projector + Sorter

### `mergebox.rs`

Implements Meteor's Mergebox algorithm:

- Tracks which fields from which documents are visible to the client
- Computes minimal `added`/`changed`/`removed` messages
- Reference-counts documents across multiple subscriptions

### `watcher.rs`

Wraps MongoDB Change Streams:

- One stream per collection (shared across all cursors)
- Broadcasts insert/update/delete/drop events

### `matcher.rs`

Compiles MongoDB query selectors into an efficient matcher:

- Supports: `$and`, `$or`, `$nor`, `$eq`, `$ne`, `$gt`, `$gte`, `$lt`, `$lte`, `$in`, `$nin`, `$exists`, `$type`, `$mod`, `$regex`, `$size`, `$all`, `$not`
- Does NOT support: `$elemMatch`, `$where`, `$bitsAllClear`, `$bitsAllSet`, `$bitsAnyClear`, `$bitsAnySet`

### `projector.rs`

Applies MongoDB field projections (include/exclude) to documents.

### `sorter.rs`

Implements MongoDB's sort algorithm with proper BSON type ordering.

### `lookup.rs`

Handles dot-notation field access (e.g., `user.profile.name`).

### `ejson.rs`

Converts BSON types to Meteor's EJSON format:

- `ObjectId` → `{$type: "oid", $value: "..."}`
- `Date` → `{$date: timestamp}`
- `Binary` → `{$binary: "base64..."}`
- etc.

### `inflights.rs`

Tracks in-flight method calls (used for subscription method interception).

### `settings.rs`

Configuration loading from TOML files and environment variables.

---

## Supported Query Features

### Query Operators

| Operator                     | Supported | Notes                               |
| ---------------------------- | --------- | ----------------------------------- |
| `$eq`                        | ✅        |                                     |
| `$ne`                        | ✅        |                                     |
| `$gt`, `$gte`, `$lt`, `$lte` | ✅        |                                     |
| `$in`, `$nin`                | ✅        |                                     |
| `$and`, `$or`, `$nor`        | ✅        |                                     |
| `$not`                       | ✅        |                                     |
| `$exists`                    | ✅        |                                     |
| `$type`                      | ✅        |                                     |
| `$mod`                       | ✅        |                                     |
| `$regex`                     | ✅        | Uses Rust `regex` crate (not PCRE2) |
| `$size`                      | ✅        |                                     |
| `$all`                       | ✅        |                                     |
| `$elemMatch`                 | ❌        |                                     |
| `$where`                     | ❌        | Cannot execute JavaScript           |
| `$bitsAllClear`              | ❌        |                                     |
| `$bitsAllSet`                | ❌        |                                     |
| `$bitsAnyClear`              | ❌        |                                     |
| `$bitsAnySet`                | ❌        |                                     |

### Cursor Options

| Option              | Supported | Notes                               |
| ------------------- | --------- | ----------------------------------- |
| `limit`             | ✅        | Requires `sort`                     |
| `sort`              | ✅        | No parallel dotted paths            |
| `projection`        | ✅        | Include/exclude only (no operators) |
| `skip`              | ⚠️        | Falls back to polling               |
| `disableOplog`      | ✅        | Forces polling mode                 |
| `pollingIntervalMs` | ✅        | Default: 10000ms                    |

---

## Limitations and Known Issues

### Functional Limitations

| Issue                      | Description                                                       | Workaround                                 |
| -------------------------- | ----------------------------------------------------------------- | ------------------------------------------ |
| **No session resumption**  | Connection errors close both client and server connections        | Client must reconnect                      |
| **Regex dialect**          | Uses Rust `regex`, not PCRE2                                      | Most patterns work, some edge cases differ |
| **Limited real-time**      | Falls back to polling for unsupported queries                     | Simplify queries or accept polling         |
| **Nondeterministic sync**  | Multiple cursors may cause `removed`+`added` instead of `changed` | Cosmetic issue only                        |
| **ObjectId serialization** | Uses EJSON format for `_id`                                       | Apply client-side patch (see Quick Start)  |

### Unsupported Meteor Features

- **Low-level publications** (`this.added`, `this.changed`, `this.removed`) → forwarded to Meteor
- **Publish composite** and similar packages → forwarded to Meteor
- **`added` before `ready`** ordering → behavior may differ slightly

---

## License

This project is licensed under the **Mozilla Public License 2.0 (MPL-2.0)**.

### Third-Party Licenses

We use [`cargo-about`](https://github.com/EmbarkStudios/cargo-about) and [`cargo-deny`](https://github.com/EmbarkStudios/cargo-deny) to manage third-party licenses.

```bash
# Generate HTML license summary
cargo about generate -o licenses.html about.hbs

# Verify all licenses are allowed
cargo deny check licenses
```

---

## Contributing

Contributions are welcome! Please ensure:

1. Code passes `cargo clippy` with no warnings
2. All tests pass: `cargo test`
3. New features include appropriate tests

---

## Acknowledgments

- The [Meteor Development Group](https://www.meteor.com/) for the original DDP protocol and Mergebox implementation
- The Rust community for excellent async and MongoDB libraries
