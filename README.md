# dumber-chat-core

Portable Rust messaging library for dumber.chat. Provides phone-number login,
contact lookup, end-to-end encrypted text messaging and voice calls.

The library builds as an `rlib` and `cdylib`; Android uses the shared typed Kotlin binding in `bindings/kotlin`.

## Build and check

Requires Rust 1.96 or newer, a C/C++ toolchain, and CMake for native dependencies.
SQLite is bundled. From this repository:

```sh
cargo build --locked
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo fmt --all -- --check
```

## Use from Rust

Add a path dependency (or a Git dependency once you host this repository):

```toml
[dependencies]
dumber-chat-core = { path = "../chat-core" }
```

```rust,ignore
use dumber_chat_core::Client;

// Generate the store password once (at least 32 characters of random material)
// and persist it using the host platform's secure storage.
let client = Client::new("https://dumber.chat")?
    .with_crypto_store(private_store_directory, persistent_store_password);
client.request_code(phone_number)?;
// Ask the user to enter the sms code or retrieve it from the SMS app
let session = client.verify(phone_number, sms_code)?;
let conversation = client.open_chat(&session, recipient_phone_number)?;
let sent = client.send_text(&session, &conversation.id, "Hello!", unique_transaction_id)?;
let update = client.sync(&session)?;
```

## Encryption

All messages and calls are end-to-end encrypted, with encryption keys stored in private_store_directory.

## Message history

The dumber.chat server doesn't store persistent chat history, only a delivery queue. It's
up to the client application to store and back up messages.

## Devices

dumber.chat allows two separate sessions handling messaging and calls or a single session handling both. 
A new login will log out any existing sessions for that functionality.

## Android binding

Use the shared [Kotlin binding](bindings/kotlin/README.md).

## Encrypted groups

`create_group(&session, name, &[], operation_id)` creates a private,
non-federated encrypted group with only the creator. Add participants afterward
with `invite_member`; an initial list of phone numbers is also supported.

## Call signaling

`set_call_capability`, `call_capability`, `turn_config`, `send_call`, and
`acknowledge_calls` expose call support without giving a host raw Matrix events.
`SyncUpdate.calls` contains `CallEvent` values separately from chat messages.
`CallSignal` carries a `CallKind`, call/party IDs, timestamp, and bounded typed SDP
or candidate fields. The bridge exposes equivalent `set_call_capability`,
`call_capability`, `turn_config`, `send_call`, `acknowledge_calls` operations;
