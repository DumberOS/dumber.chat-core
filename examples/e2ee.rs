// Uses only the reserved SSH-only test accounts. Session file must be private.
use anyhow::{ensure, Result};
use dumber_chat_core::{Client, Session};
use serde_json::{json, Value};
use std::{path::PathBuf, time::Duration};
fn main() -> Result<()> {
    let file = std::env::var("CHAT_TEST_SESSIONS")?;
    let sessions: Vec<Session> = serde_json::from_slice(&std::fs::read(file)?)?;
    let root = PathBuf::from(std::env::var("CHAT_TEST_STORE")?);
    let password = "test-store-password-not-for-production-123456";
    let client = Client::new("https://nokings.chat")?.with_crypto_store(&root, password);
    let (a, b) = (&sessions[0], &sessions[1]);
    let initial = client.sync(a, None)?;
    if let Ok(body) = std::env::var("CHAT_EXPECT_MESSAGE") {
        ensure!(
            initial.to_string().contains(&body),
            "Existing native device did not decrypt browser message"
        );
    }
    client.sync(b, None)?;
    let room = client.open_chat(a, &b.user_id)?;
    client.join(b, &room)?;
    client.sync(b, None)?;
    client.sync(a, None)?;
    let marker = format!("Encrypted Rust test {}", uuid::Uuid::new_v4());
    let txn = uuid::Uuid::new_v4().to_string();
    let sent = client.send_text(a, &room, &marker, &txn)?;
    let id = sent["event_id"].as_str().unwrap();
    let raw: Value = reqwest::blocking::Client::new()
        .get(format!(
            "{}/_matrix/client/v3/rooms/{}/event/{}",
            a.homeserver, room, id
        ))
        .bearer_auth(&a.access_token)
        .send()?
        .error_for_status()?
        .json()?;
    ensure!(
        raw["type"] == "m.room.encrypted",
        "Server received plaintext"
    );
    ensure!(raw["content"].get("body").is_none(), "Body leaked");
    ensure!(!raw.to_string().contains(&marker), "Message leaked");
    let mut received = false;
    for _ in 0..6 {
        let sync = client.sync(b, None)?;
        if sync.to_string().contains(&marker) {
            received = true;
            break;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    ensure!(received, "Recipient did not decrypt encrypted message");
    let reply = format!("Reply {marker}");
    client.send_text(b, &room, &reply, &uuid::Uuid::new_v4().to_string())?;
    let mut received = false;
    for _ in 0..6 {
        if client.sync(a, None)?.to_string().contains(&reply) {
            received = true;
            break;
        }
    }
    ensure!(received, "Sender did not decrypt reply");
    std::fs::write(
        root.join("test-message.json"),
        json!({"room_id":room,"body":marker}).to_string(),
    )?;
    let backup = root.join("backup.txt");
    if let Ok(import) = std::env::var("CHAT_IMPORT_BACKUP") {
        let response: Value = serde_json::from_str(&dumber_chat_core::invoke(&json!({"command":"import_keys","service":"https://nokings.chat","session":a,"store_path":root,"store_password":password,"path":import,"passphrase":"backup-password-123"}).to_string()))?;
        ensure!(
            response["ok"] == true,
            "Browser key export failed to import in native SDK"
        );
    }
    let invoke = |command: &str, passphrase: &str| -> Result<Value> {
        let response:Value=serde_json::from_str(&dumber_chat_core::invoke(&json!({"command":command,"service":"https://nokings.chat","session":a,"store_path":root,"store_password":password,"path":backup,"passphrase":passphrase}).to_string()))?;
        Ok(response)
    };
    ensure!(
        invoke("export_keys", "backup-password-123")?["ok"] == true,
        "Export failed"
    );
    ensure!(
        !std::fs::read_to_string(&backup)?.contains("session_key"),
        "Export not encrypted"
    );
    ensure!(
        invoke("import_keys", "incorrect-passphrase")?["ok"] == false,
        "Wrong password accepted"
    );
    ensure!(
        invoke("import_keys", "backup-password-123")?["ok"] == true,
        "Import failed"
    );
    println!("PASS: SDK two-way encryption, ciphertext on Synapse, encrypted key export/import, wrong-password rejection");
    Ok(())
}
