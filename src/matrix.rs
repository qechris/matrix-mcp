//! Matrix client management: login, session persistence, sync, and the
//! higher-level operations exposed by the MCP tools.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex as StdMutex},
};

use anyhow::{anyhow, Context, Result};
use matrix_sdk::{
    attachment::AttachmentConfig,
    authentication::matrix::MatrixSession,
    config::SyncSettings,
    media::{MediaFormat, MediaRequestParameters},
    room::{
        edit::EditedContent,
        reply::{EnforceThread, Reply},
        MessagesOptions,
    },
    ruma::{
        api::client::{
            profile::{AvatarUrl, DisplayName},
            receipt::create_receipt::v3::ReceiptType,
            room::{create_room, Visibility},
        },
        assign,
        events::{
            reaction::ReactionEventContent,
            receipt::ReceiptThread,
            relation::Annotation,
            room::{
                encryption::RoomEncryptionEventContent,
                message::{
                    MessageType, RoomMessageEventContentWithoutRelation, TextMessageEventContent,
                },
            },
            AnySyncMessageLikeEvent, AnySyncTimelineEvent, InitialStateEvent, SyncMessageLikeEvent,
        },
        EventId, OwnedEventId, OwnedUserId, RoomId, RoomOrAliasId, UserId,
    },
    store::RoomLoadSettings,
    Client, Room, RoomMemberships, SessionMeta, SessionTokens,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::RwLock;

/// How long a single `sync_once` is allowed to run before we give up.
const SYNC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// How long `login_sso` waits for the user to complete the browser flow
/// before giving up and tearing down the local callback server.
const SSO_LOGIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

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
    env_access_token: Option<String>,
    env_user_id: Option<String>,
    env_device_id: Option<String>,
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
    /// * `MATRIX_ACCESS_TOKEN`, `MATRIX_USER_ID`, `MATRIX_DEVICE_ID` - a
    ///   pre-obtained access token for optional auto-login, for homeservers
    ///   that require SSO/OAuth (which this server cannot drive interactively)
    /// * `MATRIX_DEVICE_NAME` - display name for this device (default `matrix-mcp`)
    /// * `MATRIX_SESSION_FILE`- where to persist the session (default under XDG state dir)
    /// * `MATRIX_STORE_PATH`  - directory for the SQLite crypto/state store
    ///   (default: a `store` directory next to the session file)
    pub fn from_env() -> Result<Self> {
        let non_empty = |k: &str| std::env::var(k).ok().filter(|s| !s.trim().is_empty());

        let default_homeserver = non_empty("MATRIX_HOMESERVER");
        let env_user = non_empty("MATRIX_USER");
        let env_password = non_empty("MATRIX_PASSWORD");
        let env_access_token = non_empty("MATRIX_ACCESS_TOKEN");
        let env_user_id = non_empty("MATRIX_USER_ID");
        let env_device_id = non_empty("MATRIX_DEVICE_ID");
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
            env_access_token,
            env_user_id,
            env_device_id,
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

        // The login POST above already validated the credentials, so a
        // best-effort initial sync is fine here.
        self.finish_login(client, &homeserver, false).await
    }

    /// Log in using a pre-obtained access token rather than a password. This
    /// is the only way to authenticate against homeservers that require
    /// SSO/OAuth for interactive login, since this server has no way to drive
    /// a browser through that flow. Get a token either from a homeserver
    /// admin API (which mints a fresh device, the safer option) or by copying
    /// one out of an already-logged-in client - but reusing another client's
    /// device id for encrypted rooms will cause the two clients' local crypto
    /// state to fight over the same device identity, so a token for a
    /// dedicated/fresh device is strongly preferred.
    pub async fn login_with_token(
        &self,
        homeserver: Option<&str>,
        user_id: &str,
        device_id: &str,
        access_token: &str,
    ) -> Result<LoginInfo> {
        let homeserver = homeserver
            .map(str::to_owned)
            .or_else(|| self.default_homeserver.clone())
            .ok_or_else(|| anyhow!("no homeserver provided and MATRIX_HOMESERVER is not set"))?;

        let client = self.build_client(&homeserver).await?;
        let user_id = Self::parse_user_id(user_id)?;
        let session = MatrixSession {
            meta: SessionMeta {
                user_id,
                device_id: device_id.into(),
            },
            tokens: SessionTokens {
                access_token: access_token.to_string(),
                refresh_token: None,
            },
        };
        client
            .matrix_auth()
            .restore_session(session, RoomLoadSettings::default())
            .await
            .context("failed to load the provided session")?;

        // Unlike a password login, restoring a session from a token makes no
        // network call, so nothing has verified the token yet - require the
        // initial sync to succeed so a bad/expired/wrong-device token is
        // reported as a login failure instead of silently accepted.
        self.finish_login(client, &homeserver, true).await
    }

    /// Log in via the homeserver's SSO flow: opens a local callback listener,
    /// gets the SSO URL from the homeserver, opens it in the user's default
    /// browser (best-effort - if that fails, the URL is still returned so it
    /// can be opened manually), and waits for the browser flow to complete.
    /// Returns the login result and the SSO URL that was used, if the
    /// homeserver returned one before the flow succeeded.
    pub async fn login_sso(
        &self,
        homeserver: Option<&str>,
        identity_provider_id: Option<&str>,
    ) -> Result<(LoginInfo, Option<String>)> {
        let homeserver = homeserver
            .map(str::to_owned)
            .or_else(|| self.default_homeserver.clone())
            .ok_or_else(|| anyhow!("no homeserver provided and MATRIX_HOMESERVER is not set"))?;
        let client = self.build_client(&homeserver).await?;

        let sso_url: Arc<StdMutex<Option<String>>> = Arc::new(StdMutex::new(None));
        let sso_url_writer = sso_url.clone();
        let mut builder = client
            .matrix_auth()
            .login_sso(move |url| {
                let sso_url_writer = sso_url_writer.clone();
                async move {
                    *sso_url_writer.lock().unwrap() = Some(url.clone());
                    // Spawned on the blocking pool: opening a browser shells out to the OS
                    // and shouldn't block an async worker thread while it does.
                    let open_url = url.clone();
                    match tokio::task::spawn_blocking(move || webbrowser::open(&open_url)).await {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => tracing::warn!(
                            "could not open a browser automatically ({e:#}); open this URL to \
                             finish SSO login: {url}"
                        ),
                        Err(e) => tracing::warn!(
                            "could not spawn a task to open a browser ({e:#}); open this URL to \
                             finish SSO login: {url}"
                        ),
                    }
                    Ok(())
                }
            })
            .initial_device_display_name(&self.device_name);
        if let Some(idp) = identity_provider_id {
            builder = builder.identity_provider_id(idp);
        }

        tokio::time::timeout(SSO_LOGIN_TIMEOUT, builder.send())
            .await
            .map_err(|_| {
                anyhow!(
                    "timed out after {}s waiting for the SSO browser flow to complete - run \
                     login_sso again",
                    SSO_LOGIN_TIMEOUT.as_secs()
                )
            })?
            .context("SSO login failed")?;

        let sso_url = sso_url.lock().unwrap().clone();
        let info = self.finish_login(client, &homeserver, false).await?;
        Ok((info, sso_url))
    }

    /// Persist the session, run an initial sync, and record the connected
    /// client. When `verify_sync` is true, a sync failure fails the whole
    /// call (used when nothing has otherwise validated the session yet).
    async fn finish_login(
        &self,
        client: Client,
        homeserver: &str,
        verify_sync: bool,
    ) -> Result<LoginInfo> {
        self.persist_session(&client, homeserver).await?;
        if verify_sync {
            client.sync_once(refresh_sync_settings()).await.context(
                "initial sync failed - check the token, user id, and device id are correct \
                     and the token hasn't been revoked",
            )?;
        } else {
            let _ = client.sync_once(refresh_sync_settings()).await;
        }

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

    /// If not already connected and credentials are present in the
    /// environment, perform an automatic login: password login takes
    /// priority if both `MATRIX_USER`/`MATRIX_PASSWORD` and the access-token
    /// variables are set.
    pub async fn maybe_login_from_env(&self) -> Result<()> {
        if self.client.read().await.is_some() {
            return Ok(());
        }
        if let (Some(user), Some(password)) = (self.env_user.clone(), self.env_password.clone()) {
            self.login(None, &user, &password).await?;
            return Ok(());
        }
        if let (Some(user_id), Some(device_id), Some(access_token)) = (
            self.env_user_id.clone(),
            self.env_device_id.clone(),
            self.env_access_token.clone(),
        ) {
            self.login_with_token(None, &user_id, &device_id, &access_token)
                .await?;
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

    /// Parse an event id, producing a friendly error on failure.
    fn parse_event_id(event_id: &str) -> Result<OwnedEventId> {
        EventId::parse(event_id).map_err(|e| anyhow!("invalid event id '{event_id}': {e}"))
    }

    /// Parse a room id and return the corresponding room, or a friendly error
    /// if it isn't known to this client.
    fn resolve_room(client: &Client, room_id: &str) -> Result<Room> {
        let parsed =
            RoomId::parse(room_id).map_err(|e| anyhow!("invalid room id '{room_id}': {e}"))?;
        client.get_room(&parsed).ok_or_else(|| {
            anyhow!("room {room_id} not found - are you joined? Try the `join_room` or `sync` tool")
        })
    }

    /// Parse a user id, producing a friendly error on failure.
    fn parse_user_id(user_id: &str) -> Result<OwnedUserId> {
        UserId::parse(user_id).map_err(|e| anyhow!("invalid user id '{user_id}': {e}"))
    }

    /// Log out of the current session: invalidate the access token server-side,
    /// drop the in-memory client, and delete the persisted session file so
    /// `try_restore()` doesn't try to reuse the now-invalid token on next start.
    pub async fn logout(&self) -> Result<()> {
        let client = self.connected_client().await?;
        client.logout().await.context("logout failed")?;
        *self.client.write().await = None;
        let _ = tokio::fs::remove_file(&self.session_path).await;
        Ok(())
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

    /// Send a text message (plain or markdown) to a room. When `reply_to_event_id`
    /// is set, the message is sent as a rich reply to that event.
    pub async fn send_message(
        &self,
        room_id: &str,
        body: &str,
        markdown: bool,
        reply_to_event_id: Option<&str>,
    ) -> Result<String> {
        let client = self.connected_client().await?;
        let room = Self::resolve_room(&client, room_id)?;

        let without_relation = if markdown {
            RoomMessageEventContentWithoutRelation::text_markdown(body)
        } else {
            RoomMessageEventContentWithoutRelation::text_plain(body)
        };
        let content = match reply_to_event_id {
            Some(target) => {
                let target = Self::parse_event_id(target)?;
                room.make_reply_event(
                    without_relation,
                    Reply {
                        event_id: target,
                        enforce_thread: EnforceThread::Unthreaded,
                    },
                )
                .await
                .context("failed to build reply")?
            }
            None => without_relation.with_relation(None),
        };
        let response = room.send(content).await.context("failed to send message")?;
        Ok(response.event_id.to_string())
    }

    /// Edit a previously-sent message (`m.replace`). Only the original sender
    /// can edit their own message.
    pub async fn edit_message(
        &self,
        room_id: &str,
        event_id: &str,
        body: &str,
        markdown: bool,
    ) -> Result<String> {
        let client = self.connected_client().await?;
        let room = Self::resolve_room(&client, room_id)?;
        let target = Self::parse_event_id(event_id)?;

        let new_content = if markdown {
            RoomMessageEventContentWithoutRelation::text_markdown(body)
        } else {
            RoomMessageEventContentWithoutRelation::text_plain(body)
        };
        let edit = room
            .make_edit_event(&target, EditedContent::RoomMessage(new_content))
            .await
            .context("failed to build edit")?;
        let response = room.send(edit).await.context("failed to send edit")?;
        Ok(response.event_id.to_string())
    }

    /// Redact (delete) an event - a message or a reaction - optionally with a
    /// reason. There is no separate "unreact" API: un-reacting means redacting
    /// the reaction event by its own event id.
    pub async fn redact_event(
        &self,
        room_id: &str,
        event_id: &str,
        reason: Option<&str>,
    ) -> Result<String> {
        let client = self.connected_client().await?;
        let room = Self::resolve_room(&client, room_id)?;
        let target = Self::parse_event_id(event_id)?;

        let response = room
            .redact(&target, reason, None)
            .await
            .context("failed to redact event")?;
        Ok(response.event_id.to_string())
    }

    /// React to a message with an emoji (`m.annotation`). Returns the new
    /// reaction's event id, which can later be passed to `redact_event` to
    /// remove the reaction.
    pub async fn send_reaction(
        &self,
        room_id: &str,
        event_id: &str,
        emoji: &str,
    ) -> Result<String> {
        let client = self.connected_client().await?;
        let room = Self::resolve_room(&client, room_id)?;
        let target = Self::parse_event_id(event_id)?;

        let content = ReactionEventContent::from(Annotation::new(target, emoji.to_string()));
        let response = room
            .send(content)
            .await
            .context("failed to send reaction")?;
        Ok(response.event_id.to_string())
    }

    /// Mark a room as read up to `event_id` (or the latest message if not
    /// given) with an unthreaded public read receipt.
    pub async fn mark_read(&self, room_id: &str, event_id: Option<&str>) -> Result<String> {
        let client = self.connected_client().await?;
        let room = Self::resolve_room(&client, room_id)?;

        let target = match event_id {
            Some(id) => Self::parse_event_id(id)?,
            None => {
                let mut options = MessagesOptions::backward();
                options.limit = 1u32.into();
                let latest = room
                    .messages(options)
                    .await
                    .context("failed to fetch latest message")?
                    .chunk
                    .into_iter()
                    .next()
                    .ok_or_else(|| anyhow!("room {room_id} has no messages to mark as read"))?;
                latest
                    .event_id()
                    .ok_or_else(|| anyhow!("latest event has no id"))?
            }
        };

        room.send_single_receipt(ReceiptType::Read, ReceiptThread::Unthreaded, target.clone())
            .await
            .context("failed to send read receipt")?;
        Ok(target.to_string())
    }

    /// List the members of a room with their display name, membership state,
    /// and power level.
    pub async fn get_room_members(&self, room_id: &str) -> Result<Value> {
        let client = self.connected_client().await?;
        let room = Self::resolve_room(&client, room_id)?;

        let members = room
            .members(RoomMemberships::empty())
            .await
            .context("failed to fetch room members")?;
        let members: Vec<Value> = members
            .iter()
            .map(|m| {
                json!({
                    "user_id": m.user_id().to_string(),
                    "name": m.name(),
                    "display_name": m.display_name(),
                    "membership": m.membership().as_str(),
                    "power_level": format!("{:?}", m.power_level()),
                })
            })
            .collect();
        Ok(json!({ "room_id": room_id, "count": members.len(), "members": members }))
    }

    /// Create a new room, optionally inviting users and enabling encryption.
    pub async fn create_room(
        &self,
        name: Option<&str>,
        topic: Option<&str>,
        invite: &[String],
        public: bool,
        encrypted: bool,
        is_direct: bool,
    ) -> Result<String> {
        let client = self.connected_client().await?;
        let invite = invite
            .iter()
            .map(|u| Self::parse_user_id(u))
            .collect::<Result<Vec<OwnedUserId>>>()?;

        let initial_state = if encrypted {
            vec![InitialStateEvent::with_empty_state_key(
                RoomEncryptionEventContent::with_recommended_defaults(),
            )
            .to_raw_any()]
        } else {
            vec![]
        };

        let request = assign!(create_room::v3::Request::new(), {
            name: name.map(str::to_owned),
            topic: topic.map(str::to_owned),
            invite,
            is_direct,
            visibility: if public { Visibility::Public } else { Visibility::Private },
            preset: Some(if public {
                create_room::v3::RoomPreset::PublicChat
            } else {
                create_room::v3::RoomPreset::PrivateChat
            }),
            initial_state,
        });

        let room = client
            .create_room(request)
            .await
            .context("failed to create room")?;
        Ok(room.room_id().to_string())
    }

    /// Invite a user to a room.
    pub async fn invite_user(&self, room_id: &str, user_id: &str) -> Result<()> {
        let client = self.connected_client().await?;
        let room = Self::resolve_room(&client, room_id)?;
        let user_id = Self::parse_user_id(user_id)?;
        room.invite_user_by_id(&user_id)
            .await
            .context("failed to invite user")?;
        Ok(())
    }

    /// Leave a room.
    pub async fn leave_room(&self, room_id: &str) -> Result<()> {
        let client = self.connected_client().await?;
        let room = Self::resolve_room(&client, room_id)?;
        room.leave().await.context("failed to leave room")?;
        Ok(())
    }

    /// Kick a member from a room, optionally with a reason.
    pub async fn kick_room_member(
        &self,
        room_id: &str,
        user_id: &str,
        reason: Option<&str>,
    ) -> Result<()> {
        let client = self.connected_client().await?;
        let room = Self::resolve_room(&client, room_id)?;
        let user_id = Self::parse_user_id(user_id)?;
        room.kick_user(&user_id, reason)
            .await
            .context("failed to kick member")?;
        Ok(())
    }

    /// Ban a member from a room, optionally with a reason.
    pub async fn ban_room_member(
        &self,
        room_id: &str,
        user_id: &str,
        reason: Option<&str>,
    ) -> Result<()> {
        let client = self.connected_client().await?;
        let room = Self::resolve_room(&client, room_id)?;
        let user_id = Self::parse_user_id(user_id)?;
        room.ban_user(&user_id, reason)
            .await
            .context("failed to ban member")?;
        Ok(())
    }

    /// Unban a previously-banned member from a room, optionally with a reason.
    pub async fn unban_room_member(
        &self,
        room_id: &str,
        user_id: &str,
        reason: Option<&str>,
    ) -> Result<()> {
        let client = self.connected_client().await?;
        let room = Self::resolve_room(&client, room_id)?;
        let user_id = Self::parse_user_id(user_id)?;
        room.unban_user(&user_id, reason)
            .await
            .context("failed to unban member")?;
        Ok(())
    }

    /// Update a room's name and/or topic. Only the fields provided are changed.
    pub async fn update_room(
        &self,
        room_id: &str,
        name: Option<&str>,
        topic: Option<&str>,
    ) -> Result<()> {
        let client = self.connected_client().await?;
        let room = Self::resolve_room(&client, room_id)?;
        // Independent state events - update concurrently rather than round-tripping twice.
        let set_name = async {
            if let Some(name) = name {
                room.set_name(name.to_string())
                    .await
                    .context("failed to set room name")?;
            }
            Ok::<_, anyhow::Error>(())
        };
        let set_topic = async {
            if let Some(topic) = topic {
                room.set_room_topic(topic)
                    .await
                    .context("failed to set room topic")?;
            }
            Ok::<_, anyhow::Error>(())
        };
        tokio::try_join!(set_name, set_topic)?;
        Ok(())
    }

    /// Get or create a direct-message room with a user.
    pub async fn create_dm(&self, user_id: &str) -> Result<String> {
        let client = self.connected_client().await?;
        let user_id = Self::parse_user_id(user_id)?;

        if let Some(existing) = client.get_dm_room(&user_id) {
            return Ok(existing.room_id().to_string());
        }
        let room = client
            .create_dm(&user_id)
            .await
            .context("failed to create DM room")?;
        // `create_dm` marks the room as a DM via an account-data write, but that
        // write isn't reflected in the local cache `get_dm_room` reads from until
        // the next sync - without this, an immediate second `create_dm` call for
        // the same user wouldn't find this room and would create a duplicate.
        let _ = client.sync_once(refresh_sync_settings()).await;
        Ok(room.room_id().to_string())
    }

    /// Look up a user's profile (display name and avatar). Defaults to the
    /// logged-in user when `user_id` is omitted.
    pub async fn get_profile(&self, user_id: Option<&str>) -> Result<Value> {
        let client = self.connected_client().await?;
        let user_id = match user_id {
            Some(id) => Self::parse_user_id(id)?,
            None => client
                .user_id()
                .ok_or_else(|| anyhow!("not logged in"))?
                .to_owned(),
        };

        let profile = client
            .account()
            .fetch_user_profile_of(&user_id)
            .await
            .context("failed to fetch profile")?;
        let display_name = profile
            .get_static::<DisplayName>()
            .context("failed to parse display name")?;
        let avatar_url = profile
            .get_static::<AvatarUrl>()
            .context("failed to parse avatar url")?;
        Ok(json!({
            "user_id": user_id.to_string(),
            "display_name": display_name,
            "avatar_url": avatar_url.map(|u| u.to_string()),
        }))
    }

    /// Upload a local file and send it to a room as an image, audio, video,
    /// or generic file attachment, chosen automatically from its MIME type.
    pub async fn send_file(
        &self,
        room_id: &str,
        path: &str,
        caption: Option<&str>,
    ) -> Result<String> {
        let client = self.connected_client().await?;
        let room = Self::resolve_room(&client, room_id)?;

        let data = tokio::fs::read(path)
            .await
            .with_context(|| format!("failed to read file '{path}'"))?;
        let content_type = mime_guess::from_path(path).first_or_octet_stream();
        let filename = std::path::Path::new(path)
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".to_string());

        let mut config = AttachmentConfig::new();
        if let Some(caption) = caption {
            config = config.caption(Some(TextMessageEventContent::plain(caption)));
        }

        let response = room
            .send_attachment(filename, &content_type, data, config)
            .await
            .context("failed to send attachment")?;
        Ok(response.event_id.to_string())
    }

    /// Download the media attached to a message event and save it to a local
    /// path.
    pub async fn download_media(
        &self,
        room_id: &str,
        event_id: &str,
        save_path: &str,
    ) -> Result<Value> {
        let client = self.connected_client().await?;
        let room = Self::resolve_room(&client, room_id)?;
        let target = Self::parse_event_id(event_id)?;

        let event = room
            .event(&target, None)
            .await
            .context("failed to fetch event")?;
        let deserialized: AnySyncTimelineEvent = event
            .raw()
            .deserialize()
            .context("failed to deserialize event")?;
        let AnySyncTimelineEvent::MessageLike(AnySyncMessageLikeEvent::RoomMessage(
            SyncMessageLikeEvent::Original(msg),
        )) = deserialized
        else {
            return Err(anyhow!("event {event_id} is not a message"));
        };

        let (source, mimetype, filename) = match msg.content.msgtype {
            MessageType::Image(c) => (
                c.source,
                c.info.and_then(|i| i.mimetype),
                c.filename.unwrap_or(c.body),
            ),
            MessageType::Video(c) => (
                c.source,
                c.info.and_then(|i| i.mimetype),
                c.filename.unwrap_or(c.body),
            ),
            MessageType::Audio(c) => (
                c.source,
                c.info.and_then(|i| i.mimetype),
                c.filename.unwrap_or(c.body),
            ),
            MessageType::File(c) => (
                c.source,
                c.info.and_then(|i| i.mimetype),
                c.filename.unwrap_or(c.body),
            ),
            _ => return Err(anyhow!("event {event_id} has no downloadable media")),
        };

        let request = MediaRequestParameters {
            source,
            format: MediaFormat::File,
        };
        let data = client
            .media()
            .get_media_content(&request, true)
            .await
            .context("failed to download media")?;
        let bytes_written = data.len();
        tokio::fs::write(save_path, data)
            .await
            .with_context(|| format!("failed to write '{save_path}'"))?;

        Ok(json!({
            "room_id": room_id,
            "event_id": event_id,
            "path": save_path,
            "filename": filename,
            "mime_type": mimetype,
            "bytes_written": bytes_written,
        }))
    }

    /// Read messages from a room, newest fetched, returned in chronological
    /// order. Without `before_token`, starts from the most recent message; to
    /// page further back, pass the `next_token` returned by the previous call.
    pub async fn read_messages(
        &self,
        room_id: &str,
        limit: u32,
        before_token: Option<&str>,
    ) -> Result<Value> {
        let client = self.connected_client().await?;
        let room = Self::resolve_room(&client, room_id)?;

        let mut options = MessagesOptions::backward();
        options.limit = limit.into();
        options.from = before_token.map(str::to_owned);
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
        Ok(json!({
            "room_id": room_id,
            "count": messages.len(),
            "messages": messages,
            "next_token": response.end,
        }))
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
