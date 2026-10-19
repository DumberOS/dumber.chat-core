//! Run against the SSH-only development service. Never uses real SMS.
use anyhow::{bail, Result};
use dumber_chat_core::Client;
use serde_json::Value;
use uuid::Uuid;

fn contains(sync: &Value, room: &str, body: &str) -> bool {
    sync["rooms"]["join"][room]["timeline"]["events"]
        .as_array()
        .is_some_and(|events| events.iter().any(|e| e["content"]["body"] == body))
}

fn main() -> Result<()> {
    let url = std::env::var("CHAT_SERVICE").unwrap_or("http://127.0.0.1:18091".into());
    let store = std::env::var("CHAT_TEST_STORE")?;
    let client =
        Client::new(&url)?.with_crypto_store(store, "private-test-store-password-1234567890");
    let a_phone = "+15555550101";
    let b_phone = "+15555550102";
    client.request_code(a_phone)?;
    let a = client.verify(a_phone, "123456")?;
    client.request_code(b_phone)?;
    let b = client.verify(b_phone, "123456")?;
    client.request_code(a_phone)?;
    let a2 = client.verify(a_phone, "123456")?;
    assert_eq!(a.user_id, a2.user_id, "repeat login must retain identity");
    assert_ne!(a.access_token, a2.access_token);
    assert_eq!(client.lookup(&a, b_phone)?, b.user_id);
    assert!(client.lookup(&a, "+15555550103").is_err());
    client.sync(&a, None)?;
    client.sync(&b, None)?;
    client.sync(&a2, None)?;
    let room = client.open_chat(&a, &b.user_id)?;
    client.join(&b, &room)?;
    client.sync(&b, None)?;
    client.sync(&a, None)?;
    assert_eq!(
        client.open_chat(&a, &b.user_id)?,
        room,
        "reuse the existing direct room"
    );
    let marker = format!("Rust smoke {}", Uuid::new_v4());
    let txn = Uuid::new_v4().to_string();
    let sent = client.send_text(&a, &room, &marker, &txn)?;
    assert_eq!(
        sent,
        client.send_text(&a, &room, &marker, &txn)?,
        "send retries are idempotent"
    );
    let received = client.sync(&b, None)?;
    if !contains(&received, &room, &marker) {
        bail!("Recipient did not receive message");
    }
    let reply = format!("Reply {marker}");
    client.send_text(&b, &room, &reply, &Uuid::new_v4().to_string())?;
    if !contains(&client.sync(&a, None)?, &room, &reply) {
        bail!("Sender did not receive reply");
    }
    client.logout(&a)?;
    assert!(
        client.lookup(&a, b_phone).is_err(),
        "logout must revoke directory access"
    );
    client.logout(&a2)?;
    client.logout(&b)?;
    println!("PASS: registration, repeat login, directory, room reuse, two-way text, idempotent send, logout");
    Ok(())
}
