//! Matrix SDK owns device keys, Olm sessions, Megolm rotation and persistent crypto state.
use crate::Session;
use anyhow::{bail, Context, Result};
use matrix_sdk::{
    authentication::matrix::MatrixSession,
    config::{SyncSettings, SyncToken},
    deserialized_responses::{TimelineEvent, TimelineEventKind},
    room::MessagesOptions,
    ruma::{
        events::room::{encryption::RoomEncryptionEventContent, message::RoomMessageEventContent},
        RoomId, UserId,
    },
    sync::State,
    Client as MatrixClient,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("Matrix runtime")
    })
}
static CLIENTS: OnceLock<Mutex<HashMap<PathBuf, Arc<EncryptedClient>>>> = OnceLock::new();

pub(crate) struct EncryptedClient {
    client: MatrixClient,
    sync_lock: tokio::sync::Mutex<()>,
    pending: Mutex<Vec<Value>>,
}

pub(crate) fn client(
    session: &Session,
    root: &Path,
    password: &str,
) -> Result<Arc<EncryptedClient>> {
    if password.len() < 32 {
        bail!("A persistent crypto-store password of at least 32 characters is required");
    }
    let name = format!(
        "{:x}",
        Sha256::digest(format!(
            "{}\0{}\0{}",
            session.homeserver, session.user_id, session.device_id
        ))
    );
    let path = root.join(name);
    let mut clients = CLIENTS.get_or_init(Default::default).lock().unwrap();
    if let Some(client) = clients.get(&path) {
        return Ok(client.clone());
    }
    std::fs::create_dir_all(&path)?;
    let sdk = runtime().block_on(async {
        // Use Mozilla roots consistently, including Android without a Java TLS bridge.
        let roots =
            rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
        let http = reqwest_sdk::Client::builder()
            .tls_backend_preconfigured(tls)
            .build()?;
        let sdk = MatrixClient::builder()
            .http_client(http)
            .homeserver_url(&session.homeserver)
            .sqlite_store(&path, Some(password))
            .build()
            .await?;
        let login: MatrixSession = serde_json::from_value(serde_json::to_value(session)?)?;
        sdk.matrix_auth()
            .restore_session(login, Default::default())
            .await?;
        anyhow::Ok(sdk)
    })?;
    let client = Arc::new(EncryptedClient {
        client: sdk,
        sync_lock: Default::default(),
        pending: Default::default(),
    });
    runtime().block_on(client.refresh(true, 0))?;
    clients.insert(path, client.clone());
    Ok(client)
}

fn event_json(event: &TimelineEvent) -> Result<Value> {
    let mut value: Value = serde_json::from_str(event.raw().json().get())?;
    match &event.kind {
        TimelineEventKind::Decrypted(_) => {
            value["e2ee"] = json!(true);
        }
        TimelineEventKind::UnableToDecrypt { .. } => {
            let encrypted = value.clone();
            value["type"] = json!("m.room.message");
            value["content"] = json!({"msgtype":"m.text", "body":"Encrypted message — key not available on this device. Restore your message keys or wait for the sender's key."});
            value["encrypted_event"] = encrypted;
            value["unable_to_decrypt"] = json!(true);
        }
        _ => {}
    }
    Ok(value)
}

impl EncryptedClient {
    async fn refresh(&self, initial: bool, timeout: u64) -> Result<()> {
        let _guard = self.sync_lock.lock().await;
        let mut settings = SyncSettings::default().timeout(Duration::from_secs(timeout));
        if initial {
            settings = settings.token(SyncToken::NoToken).full_state(true);
        }
        let sync = self.client.sync_once(settings).await?;
        let mut result =
            json!({"next_batch":sync.next_batch,"rooms":{"join":{},"invite":{},"leave":{}}});
        for (id, update) in sync.rooms.joined {
            let state = match update.state {
                State::Before(events) | State::After(events) => events,
            };
            let events: Vec<_> = update
                .timeline
                .events
                .iter()
                .map(event_json)
                .collect::<Result<_>>()?;
            result["rooms"]["join"][id.as_str()] = json!({"state":{"events":state},"timeline":{"events":events,"limited":update.timeline.limited,"prev_batch":update.timeline.prev_batch}});
        }
        for (id, update) in sync.rooms.invited {
            result["rooms"]["invite"][id.as_str()] = serde_json::to_value(update)?;
        }
        for id in sync.rooms.left.keys() {
            result["rooms"]["leave"][id.as_str()] = json!({});
        }
        self.pending.lock().unwrap().push(result);
        Ok(())
    }

    async fn room(&self, id: &str) -> Result<matrix_sdk::Room> {
        let id = RoomId::parse(id)?;
        if self.client.get_room(&id).is_none() {
            self.refresh(false, 0).await?;
        }
        self.client
            .get_room(&id)
            .context("Room not synced yet; please try again")
    }

    async fn ensure_encrypted(&self, id: &str) -> Result<matrix_sdk::Room> {
        let room = self.room(id).await?;
        if !room.latest_encryption_state().await?.is_encrypted() {
            // Do not send any message until the SDK has observed the encryption state.
            room.send_state_event(RoomEncryptionEventContent::with_recommended_defaults()).await
                .context("Cannot enable encryption in this room. Ask its creator to update the app and send first.")?;
            self.refresh(false, 0).await?;
        }
        if !room.encryption_state().is_encrypted() {
            bail!("Encryption is not ready; no message was sent");
        }
        Ok(room)
    }

    pub fn send(&self, id: &str, body: &str, txn: &str) -> Result<Value> {
        runtime().block_on(async {
            let room = self.ensure_encrypted(id).await?;
            let txn: matrix_sdk::ruma::OwnedTransactionId = txn.into();
            let response = room
                .send(RoomMessageEventContent::text_plain(body))
                .with_transaction_id(txn)
                .await?;
            Ok(json!({"event_id":response.response.event_id,"e2ee":true}))
        })
    }

    pub fn sync(&self, retry: Option<&Value>) -> Result<Value> {
        runtime().block_on(async {
            if self.pending.lock().unwrap().is_empty() {
                self.refresh(false, 20).await?;
            }
            let pending = std::mem::take(&mut *self.pending.lock().unwrap());
            let mut out = json!({"next_batch":"", "rooms":{"join":{},"invite":{},"leave":{}}});
            for batch in pending {
                out["next_batch"] = batch["next_batch"].clone();
                for membership in ["join", "invite", "leave"] {
                    if let Some(rooms) = batch["rooms"][membership].as_object() {
                        for (id, update) in rooms {
                            for other in ["join", "invite", "leave"] {
                                if other != membership {
                                    out["rooms"][other].as_object_mut().unwrap().remove(id);
                                }
                            }
                            if membership == "join" && out["rooms"]["join"][id].is_object() {
                                for part in ["state", "timeline"] {
                                    let existing = &mut out["rooms"]["join"][id][part]["events"];
                                    if let (Some(old), Some(new)) =
                                        (existing.as_array_mut(), update[part]["events"].as_array())
                                    {
                                        old.extend(new.iter().cloned());
                                    }
                                }
                                if update["timeline"]["limited"] == true {
                                    out["rooms"]["join"][id]["timeline"]["limited"] = json!(true);
                                    out["rooms"]["join"][id]["timeline"]["prev_batch"] =
                                        update["timeline"]["prev_batch"].clone();
                                }
                            } else {
                                out["rooms"][membership][id] = update.clone();
                            }
                        }
                    }
                }
            }
            if let Some(retries) = retry.and_then(Value::as_array) {
                for retry in retries {
                    if let (Some(id), Some(raw)) = (retry["room_id"].as_str(), retry.get("event")) {
                        if let Ok(room) = self.room(id).await {
                            if room.state() != matrix_sdk::RoomState::Joined {
                                continue;
                            }
                            if let Ok(event) = room
                                .decrypt_event(&serde_json::from_value(raw.clone())?, None)
                                .await
                            {
                                let event = event_json(&event)?;
                                if event["e2ee"] == true {
                                    if !out["rooms"]["join"][id].is_object() {
                                        out["rooms"]["join"][id] =
                                            json!({"timeline":{"events":[]}});
                                    }
                                    out["rooms"]["join"][id]["timeline"]["events"]
                                        .as_array_mut()
                                        .unwrap()
                                        .push(event);
                                }
                            }
                        }
                    }
                }
            }
            Ok(out)
        })
    }

    pub fn history(&self, id: &str, from: &str) -> Result<Value> {
        runtime().block_on(async {
            let mut options = MessagesOptions::backward();
            options.from = Some(from.to_owned());
            options.limit = 50u32.into();
            let response = self.room(id).await?.messages(options).await?;
            let events: Vec<_> = response
                .chunk
                .iter()
                .map(event_json)
                .collect::<Result<_>>()?;
            Ok(json!({"chunk":events,"end":response.end,"start":response.start}))
        })
    }

    pub fn enable(&self, id: &str) -> Result<()> {
        runtime().block_on(self.ensure_encrypted(id))?;
        Ok(())
    }

    pub fn export_keys(&self, path: PathBuf, passphrase: &str) -> Result<Value> {
        if passphrase.len() < 12 {
            bail!("Use a backup passphrase of at least 12 characters");
        }
        runtime().block_on(
            self.client
                .encryption()
                .export_room_keys(path, passphrase, |_| true),
        )?;
        Ok(json!({"status":"exported"}))
    }
    pub fn import_keys(&self, path: PathBuf, passphrase: &str) -> Result<Value> {
        let result =
            runtime().block_on(self.client.encryption().import_room_keys(path, passphrase))?;
        Ok(json!({"imported":result.imported_count,"total":result.total_count}))
    }
    pub fn devices(&self, user: &str) -> Result<Value> {
        runtime().block_on(async {
            let user = UserId::parse(user)?;
            let devices = self.client.encryption().get_user_devices(&user).await?;
            let devices: Vec<_> = devices.devices().map(|device| json!({
                "device_id":device.device_id(),"name":device.display_name(),"verified":device.is_verified(),
                "fingerprint":device.get_key(matrix_sdk::ruma::DeviceKeyAlgorithm::Ed25519).map(|k| k.to_base64())
            })).collect();
            Ok(json!({"user_id":user,"device_id":self.client.device_id(),"fingerprint":self.client.encryption().ed25519_key().await,"devices":devices}))
        })
    }
}
