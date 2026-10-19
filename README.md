# dumber-chat-core

Portable Rust messaging library for dumber.chat. Provides phone-number login,
contact lookup, private one-to-one Matrix rooms, and end-to-end encrypted text
messaging using the Matrix Rust SDK. Builds as an `rlib` and `cdylib`; Android
uses the included Java/JNI JSON bridge.

## Build and check

Requires Rust 1.96 or newer, a C/C++ toolchain, and CMake for native dependencies.
SQLite is bundled. From this repository:

```sh
cargo build --locked
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo fmt --all -- --check
```

This repository can be cloned and built independently. In the dumber.chat
application checkout it also remains a member of the parent Cargo workspace,
so the existing Android build script continues to work.

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
let client = Client::new("https://nokings.chat")?
    .with_crypto_store(private_store_directory, persistent_store_password);
client.request_code(phone_number)?;
let session = client.verify(phone_number, sms_code)?;
let peer = client.lookup(&session, recipient_phone_number)?;
let room = client.open_chat(&session, &peer)?;
client.send_text(&session, &room, "Hello!", unique_transaction_id)?;
let update = client.sync(&session, None)?;
```

All calls block: run them on worker threads. Use a separate worker for long-poll
sync. Save the returned session securely and reuse it across launches. The host
stores and renders received events, deduplicating by `event_id`; the SDK persists
its sync cursor and device keys. Reuse transaction IDs when retrying sends.

The phone service supplies `/api/auth/request-code`, `/api/auth/verify`,
`/api/directory/lookup`, and `/api/directory/resolve`. It is a separate component;
Synapse alone does not supply those endpoints. Messages go directly to Matrix.
Use `resolve` to obtain phone numbers for synced identities, then match local
contacts; never use opaque Matrix IDs as conversation titles.

## JSON and Android bindings

`invoke` accepts JSON with `command`, `service`, `session`, and command arguments.
Messaging commands require `store_path` and `store_password`. Responses are
`{"ok":true,"data":...}` or `{"ok":false,"error":"..."}`.

Commands: `request_code`, `verify`, `lookup`, `resolve`, `open_chat`, `join`,
`send`, `sync`, `history`, `devices`, `export_keys`, `import_keys`, and `logout`.
See `src/lib.rs` for argument names and the typed API. `sync` also accepts
`retry_events`: objects containing `room_id` and the original encrypted `event`
for messages whose keys were not yet available.

The Android JNI entry point is `chat.dumber.app.NativeCore.invoke(String)`.
Cross-compile for `aarch64-linux-android` using Android NDK Clang (API 26+), setting
Cargo's target linker and the target-specific CC, CXX and AR variables. Package
`libdumber_chat_core.so` in the application's ARM64 `jniLibs` directory.

## Encryption and persistence

Sends require persistent encryption storage and fail rather than send plaintext.
New rooms start encrypted. Existing rooms are upgraded before sending when the
current user has permission. Earlier plaintext messages remain plaintext.

Each login creates a separate device. Never share one device session between
independent crypto stores. Preserve the crypto store and its password; a new
login does not automatically recover old history keys. Export password-encrypted
message keys before discarding a device and import them on the replacement.
Export passphrases must be at least 12 characters. Automatic server key backup
and interactive device verification are not implemented.

## Live integration examples

`examples/e2ee.rs` uses two reserved test-account sessions in a private JSON array
file and a persistent test crypto directory:

```sh
CHAT_TEST_SESSIONS=/private/test-sessions.json \
CHAT_TEST_STORE=/private/test-crypto \
cargo run --locked --example e2ee
```

It checks encrypted delivery, ciphertext on Synapse, encrypted key export/import,
and wrong-passphrase rejection. Optional `CHAT_EXPECT_MESSAGE` and
`CHAT_IMPORT_BACKUP` check browser interoperability. It targets nokings.chat and
must only be run with reserved test accounts. Revoke test sessions after testing.

`examples/smoke.rs` requires the application's separately deployed, private
SMS-development service (`CHAT_SERVICE`, default `http://127.0.0.1:18091`) and
`CHAT_TEST_STORE`. Both examples use fixed **test-only** crypto-store passwords;
never use those passwords or test sessions in production.
