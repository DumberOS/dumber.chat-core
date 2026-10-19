//! Small synchronous Matrix text client. Run network calls off the UI thread.
//! Sessions and sync responses are serializable; the host owns secure persistence.
use anyhow::{bail, Context, Result};
use reqwest::{blocking::Client as Http, Method};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
mod encryption;

#[derive(Clone, Serialize, Deserialize)]
pub struct Session {
    pub homeserver: String,
    pub user_id: String,
    pub access_token: String,
    pub device_id: String,
}

pub struct Client {
    service: String,
    http: Http,
    crypto_store: Option<(PathBuf, String)>,
}

fn segment(value: &str) -> String {
    let mut url = reqwest::Url::parse("https://localhost/").unwrap();
    url.path_segments_mut().unwrap().push(value);
    url.path().trim_start_matches('/').to_owned()
}

impl Client {
    pub fn new(service: &str) -> Result<Self> {
        let url = reqwest::Url::parse(service)?;
        if url.scheme() != "https"
            && !(url.scheme() == "http"
                && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "10.0.2.2")))
        {
            bail!("Use HTTPS (HTTP is permitted only for local development)");
        }
        Ok(Self {
            service: service.trim_end_matches('/').to_owned(),
            crypto_store: None,
            http: Http::builder().timeout(Duration::from_secs(40)).build()?,
        })
    }

    /// Configure persistent, encrypted device storage. Keep its password across launches.
    pub fn with_crypto_store(mut self, path: impl AsRef<Path>, password: &str) -> Self {
        self.crypto_store = Some((path.as_ref().to_owned(), password.to_owned()));
        self
    }
    fn encrypted(&self, session: &Session) -> Result<Arc<encryption::EncryptedClient>> {
        let (path, password) = self
            .crypto_store
            .as_ref()
            .context("Persistent encryption storage is required; refusing plaintext messaging")?;
        encryption::client(session, path, password)
    }

    fn request(
        &self,
        method: Method,
        url: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> Result<Value> {
        let mut req = self.http.request(method, url);
        if let Some(token) = token {
            req = req.bearer_auth(token);
        }
        if let Some(body) = body {
            req = req.json(&body);
        }
        let response = req.send()?;
        let status = response.status();
        let data: Value = response.json().context("Invalid server response")?;
        if !status.is_success() {
            bail!(
                "{}: {}",
                status.as_u16(),
                data.get("detail")
                    .or(data.get("error"))
                    .unwrap_or(&json!("Request failed"))
            );
        }
        Ok(data)
    }

    fn matrix(
        &self,
        session: &Session,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value> {
        self.request(
            method,
            &format!(
                "{}/_matrix/client/v3{}",
                session.homeserver.trim_end_matches('/'),
                path
            ),
            Some(&session.access_token),
            body,
        )
    }

    pub fn request_code(&self, phone: &str) -> Result<Value> {
        self.request(
            Method::POST,
            &format!("{}/api/auth/request-code", self.service),
            None,
            Some(json!({"phone":phone})),
        )
    }

    pub fn verify(&self, phone: &str, code: &str) -> Result<Session> {
        Ok(serde_json::from_value(self.request(
            Method::POST,
            &format!("{}/api/auth/verify", self.service),
            None,
            Some(json!({"phone":phone,"code":code})),
        )?)?)
    }

    pub fn lookup(&self, session: &Session, phone: &str) -> Result<String> {
        let result = self.request(
            Method::POST,
            &format!("{}/api/directory/lookup", self.service),
            Some(&session.access_token),
            Some(json!({"phone":phone})),
        )?;
        Ok(result["user_id"]
            .as_str()
            .context("Missing user ID")?
            .to_owned())
    }

    pub fn resolve(&self, session: &Session, user_ids: &[String]) -> Result<Value> {
        self.request(
            Method::POST,
            &format!("{}/api/directory/resolve", self.service),
            Some(&session.access_token),
            Some(json!({"user_ids":user_ids})),
        )
    }

    pub fn open_chat(&self, session: &Session, user_id: &str) -> Result<String> {
        let crypto = self.encrypted(session)?;
        if user_id == session.user_id {
            bail!("Choose another person");
        }
        // Do not use initial /sync for discovery: Synapse can cache it briefly,
        // so it may omit a room created immediately before this request.
        let joined = self.matrix(session, Method::GET, "/joined_rooms", None)?;
        if let Some(rooms) = joined["joined_rooms"].as_array() {
            for id in rooms.iter().filter_map(Value::as_str) {
                let state = self.matrix(
                    session,
                    Method::GET,
                    &format!("/rooms/{}/state", segment(id)),
                    None,
                )?;
                let Some(events) = state.as_array() else {
                    continue;
                };
                let mut members = std::collections::BTreeMap::new();

                for event in events {
                    if event["type"] == "m.room.member" {
                        if let Some(key) = event["state_key"].as_str() {
                            members
                                .insert(key, event["content"]["membership"].as_str().unwrap_or(""));
                        }
                    }
                }
                let active: Vec<_> = members
                    .iter()
                    .filter(|(_, m)| **m == "join" || **m == "invite")
                    .map(|(u, _)| *u)
                    .collect();
                if active.len() == 2
                    && active.contains(&user_id)
                    && active.contains(&session.user_id.as_str())
                {
                    crypto.enable(id)?;
                    return Ok(id.to_owned());
                }
            }
        }
        let result = self.matrix(session, Method::POST, "/createRoom", Some(json!({
            "preset":"private_chat", "is_direct":true, "invite":[user_id],
            "creation_content":{"m.federate":false},
            "initial_state":[{"type":"m.room.encryption","state_key":"","content":{"algorithm":"m.megolm.v1.aes-sha2"}}, {"type":"m.room.history_visibility","state_key":"","content":{"history_visibility":"invited"}}]
        })))?;
        Ok(result["room_id"]
            .as_str()
            .context("Missing room ID")?
            .to_owned())
    }

    pub fn join(&self, session: &Session, room: &str) -> Result<Value> {
        self.matrix(
            session,
            Method::POST,
            &format!("/join/{}", segment(room)),
            Some(json!({})),
        )
    }

    /// Keep transaction_id unchanged when retrying a failed send to avoid duplicates.
    pub fn send_text(
        &self,
        session: &Session,
        room: &str,
        body: &str,
        transaction_id: &str,
    ) -> Result<Value> {
        if body.trim().is_empty() {
            bail!("Message is empty");
        }
        if transaction_id.is_empty() {
            bail!("Transaction ID is empty");
        }
        self.encrypted(session)?.send(room, body, transaction_id)
    }

    /// Sync through the Matrix SDK, decrypting messages locally. The SDK owns the sync cursor.
    pub fn sync(&self, session: &Session, _since: Option<&str>) -> Result<Value> {
        self.encrypted(session)?.sync(None)
    }
    pub fn history(&self, session: &Session, room: &str, from: &str) -> Result<Value> {
        self.encrypted(session)?.history(room, from)
    }

    pub fn logout(&self, session: &Session) -> Result<Value> {
        self.matrix(session, Method::POST, "/logout", Some(json!({})))
    }
}

/// Language-neutral JSON interface used by Android JNI and usable by other bindings.
pub fn invoke(input: &str) -> String {
    fn run(input: &str) -> Result<Value> {
        let args: Value = serde_json::from_str(input)?;
        let string = |key: &str| -> Result<&str> {
            args[key].as_str().with_context(|| format!("Missing {key}"))
        };
        let mut client = Client::new(string("service")?)?;
        if let (Some(path), Some(password)) =
            (args["store_path"].as_str(), args["store_password"].as_str())
        {
            client = client.with_crypto_store(path, password);
        }
        match string("command")? {
            "request_code" => return client.request_code(string("phone")?),
            "verify" => {
                return Ok(serde_json::to_value(
                    client.verify(string("phone")?, string("code")?)?,
                )?)
            }
            _ => {}
        }
        let session: Session = serde_json::from_value(args["session"].clone())?;
        match string("command")? {
            "lookup" => Ok(json!({"user_id":client.lookup(&session, string("phone")?)?})),
            "open_chat" => Ok(json!({"room_id":client.open_chat(&session, string("user_id")?)?})),
            "join" => client.join(&session, string("room_id")?),
            "send" => client.send_text(
                &session,
                string("room_id")?,
                string("body")?,
                string("transaction_id")?,
            ),
            "resolve" => client.resolve(
                &session,
                &serde_json::from_value::<Vec<String>>(args["user_ids"].clone())?,
            ),
            "sync" => client.encrypted(&session)?.sync(args.get("retry_events")),
            "devices" => client
                .encrypted(&session)?
                .devices(args["user_id"].as_str().unwrap_or(&session.user_id)),
            "export_keys" => client
                .encrypted(&session)?
                .export_keys(string("path")?.into(), string("passphrase")?),
            "import_keys" => client
                .encrypted(&session)?
                .import_keys(string("path")?.into(), string("passphrase")?),
            "history" => client.history(&session, string("room_id")?, string("from")?),
            "logout" => client.logout(&session),
            _ => bail!("Unknown command"),
        }
    }
    match run(input) {
        Ok(data) => json!({"ok":true,"data":data}).to_string(),
        Err(error) => json!({"ok":false,"error":error.to_string()}).to_string(),
    }
}

#[cfg(target_os = "android")]
mod android {
    use jni::{
        objects::{JClass, JString},
        sys::jstring,
        JNIEnv,
    };
    #[no_mangle]
    pub extern "system" fn Java_chat_dumber_app_NativeCore_invoke(
        mut env: JNIEnv,
        _: JClass,
        input: JString,
    ) -> jstring {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let input: String = env.get_string(&input)?.into();
            env.new_string(super::invoke(&input))
        }));
        match result {
            Ok(Ok(value)) => value.into_raw(),
            _ => {
                let _ = env.throw_new("java/lang/RuntimeException", "Native client failure");
                std::ptr::null_mut()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_nonlocal_cleartext() {
        assert!(Client::new("http://nokings.chat").is_err());
        assert!(Client::new("http://127.0.0.1:8080").is_ok());
    }
    #[test]
    fn malformed_json_returns_error() {
        assert_eq!(
            serde_json::from_str::<Value>(&invoke("nope")).unwrap()["ok"],
            false
        );
    }
    #[test]
    fn refuses_plaintext_when_store_is_missing() {
        let client = Client::new("https://nokings.chat").unwrap();
        let session = Session {
            homeserver: "https://nokings.chat".into(),
            user_id: "@test:nokings.chat".into(),
            device_id: "TEST".into(),
            access_token: "test".into(),
        };
        assert!(client
            .send_text(&session, "!room:nokings.chat", "secret", "txn")
            .unwrap_err()
            .to_string()
            .contains("refusing plaintext"));
    }

    #[test]
    fn encodes_path_segments() {
        assert_eq!(segment("a/b?#"), "a%2Fb%3F%23");
    }
}
