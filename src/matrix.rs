//! Matrix client management: login, session persistence, sync, and the
//! higher-level operations exposed by the MCP tools.

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use matrix_sdk::{
    authentication::matrix::MatrixSession,
    config::SyncSettings,
    room::MessagesOptions,
    ruma::{events::room::message::RoomMessageEventContent, RoomId, RoomOrAliasId},
    store::RoomLoadSettings,
    Client,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::RwLock;

/// How long a single `sync_once` is allowed to run before we give up.
const SYNC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Sync settings for an on-demand, one-shot refresh. The default
/// [`SyncSettings`] long-poll (30s) for new events; we instead use a zero
/// server-side timeout so the call returns the current state promptly while
/// still flushing outgoing E2EE requests (key uploads and room-key sharing).
fn refresh_sync_settings() -> SyncSettings {
    SyncSettings::default().timeout(std::time::Duration::from_secs(0))
}

/// On-disk representation of a logged-in session. We persist the homeserver
/// alongside the Matrix session so it can be rebuilt on the next startup.
#[derive(Serialize, Deserialize)]
struct PersistedSession {
    homeserver: String,
    session: MatrixSession,
}

/// Summary returned after a successful login.
pub struct LoginInfo {
    pub user_id: String,
    pub device_id: String,
    pub homeserver: String,
}

/// Owns the (optional) connected [`Client`] and the configuration needed to
/// build one. All tool handlers go through this type.
pub struct MatrixManager {
    default_homeserver: Option<String>,
    env_user: Option<String>,
    env_password: Option<String>,
    device_name: String,
    session_path: PathBuf,
    store_path: PathBuf,
    client: RwLock<Option<Client>>,
}

impl MatrixManager {
    /// Build a manager from environment variables:
    ///
    /// * `MATRIX_HOMESERVER`  - default homeserver URL (e.g. `https://matrix.org`)
    /// * `MATRIX_USER`        - username for optional auto-login
    /// * `MATRIX_PASSWORD`    - password for optional auto-login
    /// * `MATRIX_DEVICE_NAME` - display name for this device (default `matrix-mcp`)
    /// * `MATRIX_SESSION_FILE`- where to persist the session (default under XDG state dir)
    /// * `MATRIX_STORE_PATH`  - directory for the SQLite crypto/state store
    ///   (default: a `store` directory next to the session file)
    pub fn from_env() -> Result<Self> {
        let non_empty = |k: &str| std::env::var(k).ok().filter(|s| !s.trim().is_empty());

        let default_homeserver = non_empty("MATRIX_HOMESERVER");
        let env_user = non_empty("MATRIX_USER");
        let env_password = non_empty("MATRIX_PASSWORD");
        let device_name =
            non_empty("MATRIX_DEVICE_NAME").unwrap_or_else(|| "matrix-mcp".to_string());
        let session_path = non_empty("MATRIX_SESSION_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(default_session_path);
        let store_path = non_empty("MATRIX_STORE_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| default_store_path(&session_path));

        Ok(Self {
            default_homeserver,
            env_user,
            env_password,
            device_name,
            session_path,
            store_path,
            client: RwLock::new(None),
        })
    }

    /// Build a fresh client pointed at `homeserver`, backed by a persistent
    /// SQLite store so end-to-end encryption keys and room state survive
    /// restarts.
    async fn build_client(&self, homeserver: &str) -> Result<Client> {
        let _ = tokio::fs::create_dir_all(&self.store_path).await;
        Client::builder()
            .homeserver_url(homeserver)
            .sqlite_store(&self.store_path, None)
            .build()
            .await
            .with_context(|| format!("failed to build client for homeserver {homeserver}"))
    }

    /// Log in with a username and password, persist the session, and run an
    /// initial sync so the room list is populated.
    pub async fn login(
        &self,
        homeserver: Option<&str>,
        username: &str,
        password: &str,
    ) -> Result<LoginInfo> {
        let homeserver = homeserver
            .map(str::to_owned)
            .or_else(|| self.default_homeserver.clone())
            .ok_or_else(|| anyhow!("no homeserver provided and MATRIX_HOMESERVER is not set"))?;

        let client = self.build_client(&homeserver).await?;
        client
            .matrix_auth()
            .login_username(username, password)
            .initial_device_display_name(&self.device_name)
            .send()
            .await
            .context("login failed")?;

        self.persist_session(&client, &homeserver).await?;
        let _ = client.sync_once(refresh_sync_settings()).await;

        let info = LoginInfo {
            user_id: client
                .user_id()
                .map(ToString::to_string)
                .unwrap_or_default(),
            device_id: client
                .device_id()
                .map(ToString::to_string)
                .unwrap_or_default(),
            homeserver: client.homeserver().to_string(),
        };
        *self.client.write().await = Some(client);
        Ok(info)
    }

    /// Restore a previously saved session from disk, if one exists.
    /// Returns `Ok(true)` when a session was restored.
    pub async fn try_restore(&self) -> Result<bool> {
        if !self.session_path.exists() {
            return Ok(false);
        }
        let data = tokio::fs::read_to_string(&self.session_path)
            .await
            .with_context(|| format!("reading session file {}", self.session_path.display()))?;
        let persisted: PersistedSession =
            serde_json::from_str(&data).context("parsing saved session file")?;

        let client = self.build_client(&persisted.homeserver).await?;
        client
            .matrix_auth()
            .restore_session(persisted.session, RoomLoadSettings::default())
            .await
            .context("restoring saved session")?;
        let _ = client.sync_once(refresh_sync_settings()).await;
        *self.client.write().await = Some(client);
        Ok(true)
    }

    /// If not already connected and credentials are present in the environment,
    /// perform an automatic password login.
    pub async fn maybe_login_from_env(&self) -> Result<()> {
        if self.client.read().await.is_some() {
            return Ok(());
        }
        if let (Some(user), Some(password)) = (self.env_user.clone(), self.env_password.clone()) {
            self.login(None, &user, &password).await?;
        }
        Ok(())
    }

    /// Persist the current session (homeserver + access token) to disk.
    async fn persist_session(&self, client: &Client, homeserver: &str) -> Result<()> {
        let session = client
            .matrix_auth()
            .session()
            .ok_or_else(|| anyhow!("no session available to persist after login"))?;
        let persisted = PersistedSession {
            homeserver: homeserver.to_string(),
            session,
        };
        if let Some(parent) = self.session_path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        let data = serde_json::to_string_pretty(&persisted)?;
        tokio::fs::write(&self.session_path, data)
            .await
            .with_context(|| format!("writing session file {}", self.session_path.display()))?;
        Ok(())
    }

    /// Return the connected client or a helpful error if not logged in.
    async fn connected_client(&self) -> Result<Client> {
        self.client.read().await.clone().ok_or_else(|| {
            anyhow!(
                "not logged in - use the `login` tool or set the MATRIX_* environment variables"
            )
        })
    }

    /// Run a single sync against the server to refresh local state.
    pub async fn sync(&self) -> Result<()> {
        let client = self.connected_client().await?;
        tokio::time::timeout(SYNC_TIMEOUT, client.sync_once(refresh_sync_settings()))
            .await
            .context("sync timed out")?
            .context("sync failed")?;
        Ok(())
    }

    /// Report the current login state. Never errors so it can be used to probe
    /// connectivity.
    pub async fn whoami(&self) -> Value {
        match self.client.read().await.clone() {
            Some(client) => json!({
                "logged_in": true,
                "user_id": client.user_id().map(ToString::to_string),
                "device_id": client.device_id().map(ToString::to_string),
                "homeserver": client.homeserver().to_string(),
                "joined_rooms": client.joined_rooms().len(),
            }),
            None => json!({
                "logged_in": false,
                "default_homeserver": self.default_homeserver,
                "hint": "Use the `login` tool with username and password, or set MATRIX_USER/MATRIX_PASSWORD.",
            }),
        }
    }

    /// List the rooms the account has joined.
    pub async fn list_rooms(&self) -> Result<Value> {
        let client = self.connected_client().await?;
        // Best-effort refresh; ignore sync errors so we can still report
        // whatever state we already have.
        let _ = self.sync().await;

        let mut rooms = Vec::new();
        for room in client.joined_rooms() {
            let name = room
                .name()
                .or_else(|| room.cached_display_name().map(|d| d.to_string()));
            rooms.push(json!({
                "room_id": room.room_id().to_string(),
                "name": name,
                "topic": room.topic(),
                "encryption": format!("{:?}", room.encryption_state()),
            }));
        }
        Ok(json!({ "count": rooms.len(), "rooms": rooms }))
    }

    /// Send a text message (plain or markdown) to a room.
    pub async fn send_message(&self, room_id: &str, body: &str, markdown: bool) -> Result<String> {
        let client = self.connected_client().await?;
        let room_id =
            RoomId::parse(room_id).map_err(|e| anyhow!("invalid room id '{room_id}': {e}"))?;
        let room = client.get_room(&room_id).ok_or_else(|| {
            anyhow!("room {room_id} not found - are you joined? Try the `join_room` or `sync` tool")
        })?;

        let content = if markdown {
            RoomMessageEventContent::text_markdown(body)
        } else {
            RoomMessageEventContent::text_plain(body)
        };
        let response = room.send(content).await.context("failed to send message")?;
        Ok(response.event_id.to_string())
    }

    /// Read the most recent messages from a room (newest fetched, returned in
    /// chronological order).
    pub async fn read_messages(&self, room_id: &str, limit: u32) -> Result<Value> {
        let client = self.connected_client().await?;
        let parsed =
            RoomId::parse(room_id).map_err(|e| anyhow!("invalid room id '{room_id}': {e}"))?;
        let room = client.get_room(&parsed).ok_or_else(|| {
            anyhow!("room {room_id} not found - are you joined? Try the `join_room` or `sync` tool")
        })?;

        let mut options = MessagesOptions::backward();
        options.limit = limit.into();
        let response = room
            .messages(options)
            .await
            .context("failed to fetch messages")?;

        let mut messages = Vec::new();
        for event in &response.chunk {
            // With e2e-encryption enabled, `messages()` decrypts events in
            // place, so `raw()` already yields plaintext for events we have
            // keys for. Events we could not decrypt remain `m.room.encrypted`.
            let value: Value = match serde_json::from_str(event.raw().json().get()) {
                Ok(value) => value,
                Err(_) => continue,
            };
            let event_type = value.get("type").and_then(Value::as_str);
            let unable_to_decrypt = event_type == Some("m.room.encrypted");
            messages.push(json!({
                "type": event_type,
                "sender": value.get("sender").and_then(Value::as_str),
                "event_id": value.get("event_id").and_then(Value::as_str),
                "origin_server_ts": value.get("origin_server_ts").cloned().unwrap_or(Value::Null),
                "msgtype": value.pointer("/content/msgtype").and_then(Value::as_str),
                "body": value.pointer("/content/body").and_then(Value::as_str),
                "unable_to_decrypt": unable_to_decrypt,
            }));
        }
        // `backward` yields newest-first; reverse for chronological reading.
        messages.reverse();
        Ok(json!({ "room_id": room_id, "count": messages.len(), "messages": messages }))
    }

    /// Join a room by its id (`!room:server`) or alias (`#room:server`).
    pub async fn join_room(&self, room: &str) -> Result<String> {
        let client = self.connected_client().await?;
        let target = RoomOrAliasId::parse(room)
            .map_err(|e| anyhow!("invalid room id or alias '{room}': {e}"))?;
        let joined = client
            .join_room_by_id_or_alias(&target, &[])
            .await
            .context("failed to join room")?;
        Ok(joined.room_id().to_string())
    }
}

/// Default location for the persisted session file.
fn default_session_path() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_STATE_HOME") {
        if !dir.is_empty() {
            return PathBuf::from(dir).join("matrix-mcp").join("session.json");
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return PathBuf::from(home)
                .join(".local")
                .join("state")
                .join("matrix-mcp")
                .join("session.json");
        }
    }
    PathBuf::from("matrix-mcp-session.json")
}

/// Default location for the SQLite crypto/state store: a `store` directory
/// alongside the session file.
fn default_store_path(session_path: &std::path::Path) -> PathBuf {
    session_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("store")
}
