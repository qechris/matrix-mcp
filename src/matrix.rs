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
    deserialized_responses::TimelineEvent,
    encryption::{
        backups::BackupState,
        verification::{SasState, SasVerification, VerificationRequest, VerificationRequestState},
        CryptoStoreError, OlmError,
    },
    media::{MediaFormat, MediaRequestParameters},
    room::{
        edit::EditedContent,
        reply::{EnforceThread, Reply},
        IncludeRelations, MessagesOptions, RelationsOptions,
    },
    ruma::{
        api::{
            client::{
                profile::{AvatarUrl, DisplayName},
                receipt::create_receipt::v3::ReceiptType,
                room::{create_room, Visibility},
            },
            error::ErrorKind,
            Direction,
        },
        assign,
        events::{
            reaction::ReactionEventContent,
            receipt::ReceiptThread,
            relation::{Annotation, RelationType},
            room::{
                encryption::RoomEncryptionEventContent,
                message::{
                    AddMentions, MessageType, Relation, RoomMessageEventContentWithoutRelation,
                    TextMessageEventContent,
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

/// How long to wait for pending room keys to finish uploading to the key
/// backup before giving up and letting a later sync retry.
const BACKUP_UPLOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Per-call budget for a verification step to make progress before returning
/// "still pending" so the caller can poll again. Kept well under a typical MCP
/// client's ~60s request timeout, since a single call can't block on slow
/// human interaction on the other device. Overridable with
/// `MATRIX_VERIFICATION_TIMEOUT` (seconds).
const DEFAULT_VERIFICATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(35);

/// After confirming, how long to keep syncing for the other device to gossip
/// over the cross-signing secrets and backup key. Added to the confirm step's
/// own budget, still leaving margin under the client request timeout.
const SECRET_GOSSIP_BUDGET: std::time::Duration = std::time::Duration::from_secs(15);

/// Server-side long-poll for each sync while driving verification, so incoming
/// `m.key.verification.*` to-device events are picked up promptly.
const VERIFICATION_SYNC_POLL: std::time::Duration = std::time::Duration::from_secs(2);

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
    verification_timeout: std::time::Duration,
    client: RwLock<Option<Client>>,
    /// The in-flight verification request, set by `start_device_verification`
    /// and advanced by `continue_device_verification` until emoji are ready.
    pending_verification: RwLock<Option<VerificationRequest>>,
    /// The SAS awaiting emoji confirmation, once the request reaches it.
    pending_sas: RwLock<Option<SasVerification>>,
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
    /// * `MATRIX_VERIFICATION_TIMEOUT` - seconds each interactive
    ///   device-verification poll waits for the other device (default 35)
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
        let verification_timeout = non_empty("MATRIX_VERIFICATION_TIMEOUT")
            .and_then(|s| s.parse::<u64>().ok())
            .map(std::time::Duration::from_secs)
            .unwrap_or(DEFAULT_VERIFICATION_TIMEOUT);

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
            verification_timeout,
            client: RwLock::new(None),
            pending_verification: RwLock::new(None),
            pending_sas: RwLock::new(None),
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

        // A password login always mints a new device, so whatever is on disk
        // belongs to some other device and can never be reused.
        self.discard_state_for_new_device().await?;
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

        self.ensure_logged_out().await?;
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

        // Unlike a password login, the device is known up front, so a store
        // that already belongs to it is kept (along with its keys). Restoring
        // makes no network call, so if the store belongs to a different
        // device - state left behind by an earlier login or install - it is
        // safe to discard it and try once more.
        let mut client = self.build_client(&homeserver).await?;
        let mut restored = client
            .matrix_auth()
            .restore_session(session.clone(), RoomLoadSettings::default())
            .await;
        if restored.as_ref().err().is_some_and(is_mismatched_account) {
            tracing::warn!(
                "local state belongs to a different device; discarding it before logging in"
            );
            drop(client);
            self.clear_local_state().await;
            client = self.build_client(&homeserver).await?;
            restored = client
                .matrix_auth()
                .restore_session(session, RoomLoadSettings::default())
                .await;
        }
        restored.context("failed to load the provided session")?;

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
        // Like a password login, an SSO login always mints a new device.
        self.discard_state_for_new_device().await?;
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
        if let Err(e) = client
            .matrix_auth()
            .restore_session(persisted.session, RoomLoadSettings::default())
            .await
        {
            if is_mismatched_account(&e) {
                // The session file and the store belong to different devices,
                // so neither can be trusted. Start clean rather than leave
                // state behind that would also break the next login.
                tracing::warn!(
                    "saved session and local store belong to different devices; discarding both"
                );
                drop(client);
                self.clear_local_state().await;
                return Ok(false);
            }
            return Err(e).context("restoring saved session");
        }

        // Restoring makes no network call, so a session whose device was
        // removed or logged out elsewhere - common after uninstalling and
        // reinstalling - would otherwise be reported as logged in with every
        // tool failing. Confirm the token still works. Only a definite
        // "unknown token" discards the session: any other failure (e.g. the
        // homeserver being unreachable) keeps it, so a transient network
        // problem never logs anyone out.
        if let Err(e) = client.whoami().await {
            if matches!(e.client_api_error_kind(), Some(ErrorKind::UnknownToken(_))) {
                tracing::warn!(
                    "saved session's access token was revoked; discarding it - log in again"
                );
                drop(client);
                self.clear_local_state().await;
                return Ok(false);
            }
            tracing::warn!("could not verify the saved session, keeping it: {e:#}");
        }

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
    /// drop the in-memory client, and delete the persisted session file and
    /// store. The server deletes the device on logout, so its crypto state is
    /// dead weight - and left behind, it would make the next login fail.
    pub async fn logout(&self) -> Result<()> {
        let client = self.connected_client().await?;
        if let Err(e) = client.logout().await {
            // A revoked token means the device is already gone server-side
            // (e.g. removed from another client), which is what logout was
            // for - finish the local cleanup rather than leave a dead session
            // that can neither log out nor be replaced by a new login.
            if !matches!(e.client_api_error_kind(), Some(ErrorKind::UnknownToken(_))) {
                return Err(e).context("logout failed");
            }
            tracing::warn!("access token was already revoked; clearing the local session");
        }
        *self.client.write().await = None;
        // An in-progress verification belongs to the device being logged out.
        *self.pending_verification.write().await = None;
        *self.pending_sas.write().await = None;
        drop(client);
        self.clear_local_state().await;
        Ok(())
    }

    /// Fail if a session is live. Every login creates or adopts a device and
    /// needs the local store to itself, so switching accounts goes through
    /// `logout` first rather than silently replacing a working session.
    async fn ensure_logged_out(&self) -> Result<()> {
        if let Some(client) = self.client.read().await.as_ref() {
            let who = client
                .user_id()
                .map(ToString::to_string)
                .unwrap_or_else(|| "an existing account".to_string());
            return Err(anyhow!(
                "already logged in as {who} - use the `logout` tool first to switch accounts"
            ));
        }
        Ok(())
    }

    /// Prepare for a login that will mint a brand-new device (password or
    /// SSO). With no live session, anything still on disk was left by an
    /// earlier device - a previous login, or an install that was removed
    /// without logging out - and its store would make this login fail with
    /// "the account in the store doesn't match".
    async fn discard_state_for_new_device(&self) -> Result<()> {
        self.ensure_logged_out().await?;
        self.clear_local_state().await;
        Ok(())
    }

    /// Delete the persisted session file and the SQLite store.
    ///
    /// Only the files matrix-sdk itself creates are removed from the store
    /// directory (which is then removed if it is left empty), never the whole
    /// tree: `MATRIX_STORE_PATH` is user-configurable, and a blanket recursive
    /// delete on a misconfigured path could take unrelated files with it.
    async fn clear_local_state(&self) {
        if let Err(e) = tokio::fs::remove_file(&self.session_path).await {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    "could not remove session file {}: {e}",
                    self.session_path.display()
                );
            }
        }

        let mut entries = match tokio::fs::read_dir(&self.store_path).await {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => {
                tracing::warn!("could not read store {}: {e}", self.store_path.display());
                return;
            }
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            // matrix-sdk-{state,crypto,event-cache,media}.sqlite3, plus their
            // -wal / -shm / -journal companions.
            if name.starts_with("matrix-sdk-") && name.contains(".sqlite3") {
                if let Err(e) = tokio::fs::remove_file(entry.path()).await {
                    tracing::warn!("could not remove {}: {e}", entry.path().display());
                }
            }
        }
        // Succeeds only if nothing else lives there.
        let _ = tokio::fs::remove_dir(&self.store_path).await;
    }

    /// Run one `sync_once` with the given settings, bounded by [`SYNC_TIMEOUT`].
    async fn sync_once_with(client: &Client, settings: SyncSettings) -> Result<()> {
        tokio::time::timeout(SYNC_TIMEOUT, client.sync_once(settings))
            .await
            .context("sync timed out")?
            .context("sync failed")?;
        Ok(())
    }

    /// Run a single sync against the server to refresh local state.
    pub async fn sync(&self) -> Result<()> {
        let client = self.connected_client().await?;
        Self::sync_once_with(&client, refresh_sync_settings()).await?;
        Self::flush_key_backup(&client).await;
        Ok(())
    }

    /// Push any room keys this device holds but hasn't backed up yet to the
    /// server-side key backup. The SDK only triggers this in the background,
    /// which a short-lived process can exit before completing - leaving keys
    /// out of the backup and messages unreadable on the account's next device.
    /// Best-effort: backups may not be set up, and failing to upload must not
    /// fail the caller's actual operation.
    async fn flush_key_backup(client: &Client) {
        let backups = client.encryption().backups();
        if backups.state() != BackupState::Enabled {
            return;
        }
        match tokio::time::timeout(BACKUP_UPLOAD_TIMEOUT, backups.wait_for_steady_state()).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!("could not upload room keys to the key backup: {e:?}"),
            Err(_) => tracing::warn!(
                "timed out uploading room keys to the key backup; they will be retried on the \
                 next sync"
            ),
        }
    }

    /// Set up a server-side key backup for this account and upload the room
    /// keys this device holds, returning the newly generated recovery key.
    /// That key is shown exactly once and cannot be recovered afterwards - it
    /// is what future devices pass to `restore_key_backup`.
    pub async fn enable_key_backup(&self, passphrase: Option<&str>) -> Result<Value> {
        let client = self.connected_client().await?;
        let recovery = client.encryption().recovery();

        // The SDK happily creates a *second* secret store when this device
        // already has backups enabled, which silently invalidates the recovery
        // key the account was previously given. Refuse instead - rotating a
        // recovery key someone may have saved is not something to do by
        // accident.
        if client
            .encryption()
            .backups()
            .exists_on_server()
            .await
            .unwrap_or(false)
        {
            return Err(anyhow!(
                "this account already has a key backup - use `restore_key_backup` with its \
                 existing recovery key. Creating a new one would invalidate the old key and \
                 strand the keys already backed up under it."
            ));
        }

        let mut enable = recovery.enable().wait_for_backups_to_upload();
        if let Some(passphrase) = passphrase {
            enable = enable.with_passphrase(passphrase);
        }
        let recovery_key = enable.await.context(
            "failed to enable key backup - if a backup already exists on the account, use \
             `restore_key_backup` with its existing recovery key instead",
        )?;

        Ok(json!({
            "enabled": true,
            "recovery_key": recovery_key,
            "backup_state": format!("{:?}", client.encryption().backups().state()),
            "warning": "Store this recovery key somewhere safe now - it is not recoverable and \
                        will not be shown again. Without it, messages in encrypted rooms cannot \
                        be read on a new device.",
        }))
    }

    /// Unlock the server-side key backup with a recovery key (also called a
    /// security key or passphrase), importing the cross-signing and backup
    /// decryption secrets from secret storage. This is what makes messages
    /// sent *before* this device existed decryptable: their room keys live in
    /// the backup, encrypted with this key.
    pub async fn restore_key_backup(&self, recovery_key: &str) -> Result<Value> {
        let client = self.connected_client().await?;
        client
            .encryption()
            .recovery()
            .recover(recovery_key)
            .await
            .context(
                "failed to unlock the key backup - check the recovery key is correct, and that \
                 key backup is actually set up on this account",
            )?;
        // Backups are resumed during sync, so settle the state before reporting it.
        let _ = client.sync_once(refresh_sync_settings()).await;

        let backup_state = client.encryption().backups().state();
        Ok(json!({
            "restored": true,
            "recovery_state": format!("{:?}", client.encryption().recovery().state()),
            "backup_state": format!("{backup_state:?}"),
            // `recover` succeeds even when it finds no usable backup version, so
            // say plainly whether historical keys can actually be fetched now.
            "key_backup_usable": backup_state == BackupState::Enabled,
        }))
    }

    /// Download this room's historical megolm keys from the server-side key
    /// backup, so previously undecryptable messages can be read. Requires the
    /// backup to have been unlocked first (see `restore_key_backup`).
    pub async fn download_room_keys(&self, room_id: &str) -> Result<Value> {
        let client = self.connected_client().await?;
        let room = Self::resolve_room(&client, room_id)?;
        let backups = client.encryption().backups();
        let backup_state = backups.state();

        backups
            .download_room_keys_for_room(room.room_id())
            .await
            .context("failed to download room keys from the key backup")?;

        Ok(json!({
            "room_id": room_id,
            "backup_state": format!("{backup_state:?}"),
            // The SDK reports success even when no backup key is available and
            // nothing was fetched, so don't claim more than we know.
            "key_backup_usable": backup_state == BackupState::Enabled,
            "hint": if backup_state == BackupState::Enabled {
                "Keys downloaded; re-read the room to pick up newly decryptable messages."
            } else {
                "No usable key backup - run `restore_key_backup` with your recovery key first."
            },
        }))
    }

    /// Run one sync tuned for verification: a short server-side long-poll so
    /// incoming `m.key.verification.*` to-device events arrive promptly, and
    /// which also flushes our own queued verification messages.
    async fn verification_sync(client: &Client) -> Result<()> {
        Self::sync_once_with(
            client,
            SyncSettings::default().timeout(VERIFICATION_SYNC_POLL),
        )
        .await
    }

    /// Whether this device has been cross-signed by the account owner, i.e.
    /// verified by another session. `None` if the device can't be read.
    /// (`is_verified` alone is true for one's own device by self-trust, so it's
    /// the wrong signal - use cross-signing.)
    async fn own_device_cross_signed(client: &Client) -> Option<bool> {
        match client.encryption().get_own_device().await {
            Ok(Some(device)) => Some(device.is_cross_signed_by_owner()),
            _ => None,
        }
    }

    /// Begin verifying this device against the user's other, already-verified
    /// session (e.g. Element). Sends the verification request that session is
    /// waiting for and briefly drives sync toward the emoji.
    ///
    /// Because a single tool call can't block on slow interaction in the other
    /// app (the MCP client caps request time), this returns as soon as either
    /// the emoji are ready OR the step budget elapses with the request still
    /// pending. In the pending case, accept the request in the other session,
    /// then call `continue_device_verification` to fetch the emoji.
    ///
    /// Verifying this way is what lets this device read history: once verified,
    /// the other device gossips over the cross-signing secrets and the
    /// key-backup key automatically, with no recovery key to type.
    pub async fn start_device_verification(&self) -> Result<Value> {
        let client = self.connected_client().await?;
        let user_id = client
            .user_id()
            .ok_or_else(|| anyhow!("not logged in"))?
            .to_owned();

        // Make sure the E2EE background tasks (which register the handlers that
        // auto-import gossiped secrets and enable the backup) are running.
        client
            .encryption()
            .wait_for_e2ee_initialization_tasks()
            .await;

        // Clear any leftover flow from a previous attempt before starting a new
        // one; two concurrent requests make the other device cancel.
        self.clear_pending_verification().await;

        // Our own cross-signing identity is needed to request self-verification;
        // force a /keys/query if it isn't in the local store yet.
        let identity = match client
            .encryption()
            .get_user_identity(&user_id)
            .await
            .context("failed to look up own identity")?
        {
            Some(identity) => identity,
            None => client
                .encryption()
                .request_user_identity(&user_id)
                .await
                .context("failed to fetch own identity")?
                .ok_or_else(|| {
                    anyhow!(
                        "no cross-signing identity for this account - set up secure backup / \
                         cross-signing in another client (e.g. Element) first"
                    )
                })?,
        };

        // Sending the request also targets our other devices; the verified one
        // (Element) accepts it.
        let request = identity
            .request_verification()
            .await
            .context("failed to send verification request")?;
        *self.pending_verification.write().await = Some(request);

        self.advance_verification(&client, true).await
    }

    /// Resume an in-progress verification started with
    /// `start_device_verification`: drive sync toward the emoji and return them
    /// once the other device has accepted and keys are exchanged. Call this
    /// repeatedly until it reports the emoji (or a cancellation).
    pub async fn continue_device_verification(&self) -> Result<Value> {
        let client = self.connected_client().await?;
        if self.pending_verification.read().await.is_none()
            && self.pending_sas.read().await.is_none()
        {
            return Err(anyhow!(
                "no verification in progress - run `start_device_verification` first"
            ));
        }
        self.advance_verification(&client, false).await
    }

    /// Drive the pending verification for up to one step budget: move the
    /// request to Ready, start the SAS, and wait for keys to be exchanged.
    /// Returns the emoji if they become ready within the budget, otherwise a
    /// "pending" status so the caller can poll again.
    async fn advance_verification(&self, client: &Client, just_started: bool) -> Result<Value> {
        let request = self.pending_verification.read().await.clone();
        let mut sas = self.pending_sas.read().await.clone();

        let outcome = tokio::time::timeout(self.verification_timeout, async {
            loop {
                // Obtain the SAS once the request is ready / has transitioned.
                if sas.is_none() {
                    if let Some(request) = &request {
                        match request.state() {
                            VerificationRequestState::Transitioned { verification } => {
                                sas = verification.sas();
                            }
                            VerificationRequestState::Cancelled(info) => {
                                return Err(anyhow!("the other device cancelled: {info:?}"));
                            }
                            VerificationRequestState::Done => {
                                return Err(anyhow!(
                                    "verification finished before emoji were shown"
                                ));
                            }
                            _ if request.is_ready() => {
                                sas = request
                                    .start_sas()
                                    .await
                                    .context("failed to start emoji verification")?;
                            }
                            _ => {}
                        }
                    }
                }

                // Once we have a SAS, wait for the emoji.
                if let Some(sas) = &sas {
                    match sas.state() {
                        SasState::KeysExchanged { .. } | SasState::Done { .. } => {
                            return Ok(true);
                        }
                        SasState::Cancelled(info) => {
                            return Err(anyhow!("verification was cancelled: {info:?}"));
                        }
                        _ => {}
                    }
                }

                Self::verification_sync(client).await?;
            }
        })
        .await;

        // Persist whatever progress we made so a follow-up call can resume.
        *self.pending_sas.write().await = sas.clone();

        match outcome {
            // Cancelled / hard error: tear the flow down so a retry starts clean.
            Ok(Err(e)) => {
                self.clear_pending_verification().await;
                Err(e)
            }
            // Emoji ready.
            Ok(Ok(_)) => {
                let sas = sas.expect("SAS present when emoji are ready");
                let emoji = sas.emoji().map(|emojis| {
                    emojis
                        .iter()
                        .map(|e| json!({ "symbol": e.symbol, "description": e.description }))
                        .collect::<Vec<_>>()
                });
                let decimals = sas.decimals().map(|(a, b, c)| [a, b, c]);
                Ok(json!({
                    "status": "awaiting_confirmation",
                    "emoji": emoji,
                    "decimals": decimals,
                    "instructions": "Compare these against what your other session shows. If they \
                        match, call `confirm_device_verification`; if not, call \
                        `cancel_device_verification`.",
                }))
            }
            // Budget elapsed, still waiting on the other device.
            Err(_) => Ok(json!({
                "status": "pending",
                "instructions": if just_started {
                    "Accept the verification request in your other session (e.g. Element), then \
                     call `continue_device_verification` to fetch the emoji."
                } else {
                    "Still waiting for the other session to accept - accept the request there, \
                     then call `continue_device_verification` again."
                },
            })),
        }
    }

    /// Confirm that the emoji from the start/continue step match the other
    /// device, completing the verification. Afterwards, drives sync so the
    /// other device can gossip over the cross-signing secrets and key-backup
    /// key, and reports whether this device is now verified and can read
    /// history.
    pub async fn confirm_device_verification(&self) -> Result<Value> {
        let client = self.connected_client().await?;
        let sas = self.pending_sas.read().await.clone().ok_or_else(|| {
            anyhow!(
                "no emoji to confirm yet - run `start_device_verification` (and \
                 `continue_device_verification`) until it returns emoji first"
            )
        })?;

        sas.confirm()
            .await
            .context("failed to confirm verification")?;

        let result = tokio::time::timeout(self.verification_timeout, async {
            loop {
                if sas.is_done() {
                    return Ok(());
                }
                if sas.is_cancelled() {
                    return Err(anyhow!("the other device cancelled before completing"));
                }
                Self::verification_sync(&client).await?;
            }
        })
        .await
        .map_err(|_| anyhow!("timed out waiting for the other device to finish verifying"));
        // The flow is over (or wedged) either way; drop the stored state.
        self.clear_pending_verification().await;
        result??;

        // The cross-signing secrets and backup key arrive over the next couple
        // of syncs via secret gossiping; pump sync until they land (bounded).
        // The loop's own result is the completeness we then report - no need to
        // re-query the status afterwards.
        let cross_signing_complete = tokio::time::timeout(SECRET_GOSSIP_BUDGET, async {
            loop {
                if client
                    .encryption()
                    .cross_signing_status()
                    .await
                    .is_some_and(|s| s.is_complete())
                {
                    return true;
                }
                if Self::verification_sync(&client).await.is_err() {
                    return false;
                }
            }
        })
        .await
        .unwrap_or(false);

        let device_verified = Self::own_device_cross_signed(&client)
            .await
            .unwrap_or(false);
        let backup_state = client.encryption().backups().state();

        Ok(json!({
            "verified": device_verified,
            "cross_signing_complete": cross_signing_complete,
            "backup_state": format!("{backup_state:?}"),
            "hint": if backup_state == BackupState::Enabled {
                "This device is verified and the key backup is unlocked - reading a room now \
                 pulls its history automatically."
            } else {
                "This device is verified. If the account has a key backup, its history keys \
                 should follow shortly; retry a read, or check `whoami`."
            },
        }))
    }

    /// Abort the in-progress device verification (e.g. the emoji didn't match).
    pub async fn cancel_device_verification(&self) -> Result<Value> {
        let sas = self.pending_sas.read().await.clone();
        let request = self.pending_verification.read().await.clone();
        let had_flow = sas.is_some() || request.is_some();
        if let Some(sas) = sas {
            let _ = sas.cancel().await;
        } else if let Some(request) = request {
            let _ = request.cancel().await;
        }
        self.clear_pending_verification().await;
        Ok(json!({
            "cancelled": had_flow,
            "note": if had_flow { Value::Null } else { json!("no verification was in progress") },
        }))
    }

    /// Drop any stored verification request/SAS.
    async fn clear_pending_verification(&self) {
        *self.pending_sas.write().await = None;
        *self.pending_verification.write().await = None;
    }

    /// Report the current login state. Never errors so it can be used to probe
    /// connectivity.
    pub async fn whoami(&self) -> Value {
        match self.client.read().await.clone() {
            Some(client) => {
                // Whether another session has cross-signed this device; an
                // unverified device can't be gossiped historical keys.
                let device_verified = Self::own_device_cross_signed(&client).await;
                json!({
                    "logged_in": true,
                    "user_id": client.user_id().map(ToString::to_string),
                    "device_id": client.device_id().map(ToString::to_string),
                    "homeserver": client.homeserver().to_string(),
                    "joined_rooms": client.joined_rooms().len(),
                    "device_verified": device_verified,
                    // Whether historical (pre-this-device) encrypted messages can be
                    // decrypted, which is the usual reason reads come back empty.
                    "key_backup": format!("{:?}", client.encryption().backups().state()),
                })
            }
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
    ) -> Result<(String, Option<String>)> {
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
                        // Forward the target's thread, if it has one: a reply
                        // to a message inside a thread belongs in that thread,
                        // not loose in the room timeline. Replying to anything
                        // unthreaded - including a thread's own root - stays a
                        // plain rich reply, so this never starts a new thread.
                        enforce_thread: EnforceThread::MaybeThreaded,
                        // Notify the author we are replying to, as the spec
                        // expects. The SDK downgrades this to `No` by itself
                        // when replying to our own message, since outgoing
                        // messages cannot self-notify.
                        add_mentions: AddMentions::Yes,
                    },
                )
                .await
                .context("failed to build reply")?
            }
            None => without_relation.with_relation(None),
        };
        // Whether the thread was forwarded is only knowable from the built
        // content, so report it rather than making callers re-read the event.
        let thread_root = match &content.relates_to {
            Some(Relation::Thread(thread)) => Some(thread.event_id.to_string()),
            _ => None,
        };
        let response = room.send(content).await.context("failed to send message")?;
        Ok((response.response.event_id.to_string(), thread_root))
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
        Ok(response.response.event_id.to_string())
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
        Ok(response.response.event_id.to_string())
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
                    .to_owned()
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

        // `MessagesOptions` isn't `Clone`, and a retry has to request the exact
        // same page, so build it from scratch each time.
        let build_options = || {
            let mut options = MessagesOptions::backward();
            options.limit = limit.into();
            options.from = before_token.map(str::to_owned);
            options
        };

        let mut response = room
            .messages(build_options())
            .await
            .context("failed to fetch messages")?;

        let mut recovered_from_backup = false;
        if Self::recover_room_keys(&client, &room, &response.chunk).await {
            if let Ok(retried) = room.messages(build_options()).await {
                recovered_from_backup = true;
                response = retried;
            }
        }

        let mut messages = Self::format_events(&response.chunk);
        // `backward` yields newest-first; reverse for chronological reading.
        messages.reverse();
        let undecryptable = Self::count_undecryptable(&messages);
        Ok(json!({
            "room_id": room_id,
            "count": messages.len(),
            "messages": messages,
            "next_token": response.end,
            "undecryptable_count": undecryptable,
            "recovered_keys_from_backup": recovered_from_backup,
            "hint": Self::decryption_hint(undecryptable),
        }))
    }

    /// Read a single thread: the root message followed by its replies, oldest
    /// first. Threaded replies are ordinary events carrying an `m.thread`
    /// relation to the root, so this is the relations API rather than the room
    /// timeline - thread replies do not appear in `read_messages` output in
    /// their conversational order.
    pub async fn read_thread(
        &self,
        room_id: &str,
        event_id: &str,
        limit: u32,
        from_token: Option<&str>,
    ) -> Result<Value> {
        let client = self.connected_client().await?;
        let room = Self::resolve_room(&client, room_id)?;
        let requested = Self::parse_event_id(event_id)?;

        // The root only belongs on the first page; later pages continue from a
        // token and would otherwise repeat it. Fetching it also lets us accept
        // the id of any message *inside* the thread: callers reading
        // `read_messages` output see replies, not the root, so redirect to the
        // root they relate to instead of returning a confusingly empty thread.
        let (root_id, root_event) = match from_token {
            Some(_) => (requested, None),
            None => {
                let event = room.event(&requested, None).await.with_context(|| {
                    format!("failed to fetch event {requested} - is it a message in this room?")
                })?;
                match Self::thread_root_of(&event) {
                    Some(root_id) => {
                        let root = room
                            .event(&root_id, None)
                            .await
                            .with_context(|| format!("failed to fetch thread root {root_id}"))?;
                        (root_id, Some(root))
                    }
                    None => (requested, Some(event)),
                }
            }
        };

        let build_options = || RelationsOptions {
            from: from_token.map(str::to_owned),
            // Forward from the root reads the thread in conversation order,
            // and pages onward through it via `next_batch_token`.
            dir: Direction::Forward,
            limit: Some(limit.into()),
            include_relations: IncludeRelations::RelationsOfType(RelationType::Thread),
            recurse: false,
        };

        let mut response = room
            .relations(root_id.clone(), build_options())
            .await
            .context("failed to fetch thread replies")?;

        let mut recovered_from_backup = false;
        if Self::recover_room_keys(&client, &room, &response.chunk).await {
            if let Ok(retried) = room.relations(root_id.clone(), build_options()).await {
                recovered_from_backup = true;
                response = retried;
            }
        }

        let mut root_message = root_event.as_ref().and_then(Self::format_event);
        // The root was fetched before any keys were imported, so if the replies
        // triggered a recovery, decode it again against the fresh keys.
        if recovered_from_backup
            && root_message
                .as_ref()
                .is_some_and(|m| m["unable_to_decrypt"] == Value::Bool(true))
        {
            if let Ok(root) = room.event(&root_id, None).await {
                root_message = Self::format_event(&root).or(root_message);
            }
        }

        let reply_count = response.chunk.len();
        let messages: Vec<Value> = root_message
            .into_iter()
            .chain(Self::format_events(&response.chunk))
            .collect();
        let undecryptable = Self::count_undecryptable(&messages);

        Ok(json!({
            "room_id": room_id,
            "thread_root": root_id.to_string(),
            "count": messages.len(),
            "reply_count": reply_count,
            "messages": messages,
            "next_token": response.next_batch_token,
            "undecryptable_count": undecryptable,
            "recovered_keys_from_backup": recovered_from_backup,
            "hint": Self::decryption_hint(undecryptable).or_else(|| {
                (reply_count == 0 && from_token.is_none()).then_some(
                    "This message has no threaded replies. Replies made with `send_message`'s \
                     reply_to_event_id are rich replies, not threads, and stay in the room \
                     timeline - read those with `read_messages`.",
                )
            }),
        }))
    }

    /// Render a batch of timeline events into the message shape both read
    /// tools return, skipping any whose raw JSON won't parse.
    fn format_events(chunk: &[TimelineEvent]) -> Vec<Value> {
        chunk.iter().filter_map(Self::format_event).collect()
    }

    /// Render one timeline event as a message object.
    fn format_event(event: &TimelineEvent) -> Option<Value> {
        // With e2e-encryption enabled, the SDK decrypts events in place, so
        // `raw()` already yields plaintext for events we have keys for.
        let value: Value = serde_json::from_str(event.raw().json().get()).ok()?;
        let event_type = value.get("type").and_then(Value::as_str);
        // Reuse the parsed `value` rather than re-parsing via
        // `is_undecryptable`: an event left as raw `m.room.encrypted` (or
        // flagged by the SDK) is one we couldn't decrypt.
        let unable_to_decrypt = event.kind.is_utd() || event_type == Some("m.room.encrypted");
        Some(json!({
            "type": event_type,
            "sender": value.get("sender").and_then(Value::as_str),
            "event_id": value.get("event_id").and_then(Value::as_str),
            "origin_server_ts": value.get("origin_server_ts").cloned().unwrap_or(Value::Null),
            "msgtype": value.pointer("/content/msgtype").and_then(Value::as_str),
            "body": value.pointer("/content/body").and_then(Value::as_str),
            "unable_to_decrypt": unable_to_decrypt,
            // Non-null on a threaded reply, naming the thread it belongs to -
            // the id to pass to `read_thread`.
            "thread_root": Self::thread_root_in(&value).map(|id| id.to_string()),
        }))
    }

    fn count_undecryptable(messages: &[Value]) -> usize {
        messages
            .iter()
            .filter(|m| m["unable_to_decrypt"] == Value::Bool(true))
            .count()
    }

    fn decryption_hint(undecryptable: usize) -> Option<&'static str> {
        (undecryptable > 0).then_some(
            "Some messages could not be decrypted. If they predate this device, unlock the \
             server-side key backup with `restore_key_backup` (needs your recovery key).",
        )
    }

    /// The thread this event is a reply in, or `None` if it isn't a threaded
    /// reply. A thread relation is sent unencrypted so servers can bundle it,
    /// so this reads correctly even for events we couldn't decrypt.
    fn thread_root_of(event: &TimelineEvent) -> Option<OwnedEventId> {
        Self::thread_root_in(&serde_json::from_str(event.raw().json().get()).ok()?)
    }

    /// `thread_root_of` against already-parsed event JSON.
    fn thread_root_in(value: &Value) -> Option<OwnedEventId> {
        let relation = value.pointer("/content/m.relates_to")?;
        if relation.get("rel_type").and_then(Value::as_str) != Some("m.thread") {
            return None;
        }
        EventId::parse(relation.get("event_id")?.as_str()?).ok()
    }

    /// If any event in `chunk` couldn't be decrypted, and the server-side key
    /// backup is unlocked, import this room's keys from it. Messages sent
    /// before this device existed are encrypted with keys it never received,
    /// and those keys live in the backup.
    ///
    /// Returns whether keys were imported, in which case re-requesting the same
    /// page decodes it again against them - the read APIs decrypt on every call
    /// and cache nothing, so the second pass sees the fresh keys.
    async fn recover_room_keys(client: &Client, room: &Room, chunk: &[TimelineEvent]) -> bool {
        if !chunk.iter().any(Self::is_undecryptable)
            || client.encryption().backups().state() != BackupState::Enabled
        {
            return false;
        }
        client
            .encryption()
            .backups()
            .download_room_keys_for_room(room.room_id())
            .await
            .is_ok()
    }

    /// Whether an event could not be decrypted. Prefers the SDK's own flag, but
    /// also catches events left as raw `m.room.encrypted` when decryption was
    /// never attempted (e.g. no crypto store), which the flag alone misses.
    fn is_undecryptable(event: &TimelineEvent) -> bool {
        event.kind.is_utd()
            || serde_json::from_str::<Value>(event.raw().json().get())
                .ok()
                .and_then(|v| v.get("type").and_then(Value::as_str).map(str::to_owned))
                .as_deref()
                == Some("m.room.encrypted")
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

/// Whether `e` means the crypto store holds a different device's account than
/// the session being loaded - i.e. the store was left behind by another
/// device and cannot be used for this one.
fn is_mismatched_account(e: &matrix_sdk::Error) -> bool {
    // Restoring a session surfaces it wrapped in an OlmError; other paths
    // report the store error directly.
    let store_error = match e {
        matrix_sdk::Error::CryptoStoreError(inner) => Some(&**inner),
        matrix_sdk::Error::OlmError(inner) => match &**inner {
            OlmError::Store(inner) => Some(inner),
            _ => None,
        },
        _ => None,
    };
    matches!(
        store_error,
        Some(CryptoStoreError::MismatchedAccount { .. })
    )
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
