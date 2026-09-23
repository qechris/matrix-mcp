//! The MCP server: exposes Matrix operations as tools over stdio.

use std::sync::Arc;

use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ContentBlock, ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router, ErrorData, ServerHandler,
};
use serde::Deserialize;
use serde_json::Value;

use crate::matrix::MatrixManager;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct LoginArgs {
    #[schemars(
        description = "Homeserver URL, e.g. https://matrix.org. Optional if MATRIX_HOMESERVER is set."
    )]
    pub homeserver: Option<String>,
    #[schemars(
        description = "Matrix username (the localpart, e.g. `alice`, or a full @alice:server)."
    )]
    pub username: String,
    #[schemars(description = "Account password.")]
    pub password: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct LoginWithTokenArgs {
    #[schemars(
        description = "Homeserver URL, e.g. https://matrix.org. Optional if MATRIX_HOMESERVER is set."
    )]
    pub homeserver: Option<String>,
    #[schemars(description = "Full user id, e.g. @alice:matrix.org.")]
    pub user_id: String,
    #[schemars(
        description = "Device id the access token was issued for. Prefer a token for a fresh/dedicated \
        device (e.g. minted via a homeserver admin API) over one copied from an already-logged-in \
        client, since two clients sharing one device id will conflict over encryption state."
    )]
    pub device_id: String,
    #[schemars(
        description = "A pre-obtained access token. Use this for homeservers that require SSO/OAuth, \
        which this server cannot complete interactively - get a token from your homeserver's admin \
        API or from an existing client's settings (e.g. Element web: Settings > Help & About > Advanced)."
    )]
    pub access_token: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct LoginSsoArgs {
    #[schemars(
        description = "Homeserver URL, e.g. https://matrix.org. Optional if MATRIX_HOMESERVER is set."
    )]
    pub homeserver: Option<String>,
    #[schemars(
        description = "Identity provider id to use directly (e.g. \"oidc-microsoft\"), skipping the \
        provider picker. Optional; omit to let the homeserver show its normal SSO provider selection."
    )]
    pub identity_provider_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct EnableKeyBackupArgs {
    #[schemars(
        description = "Optional passphrase to protect the backup with, in addition to the \
        generated recovery key. Either can then be used to restore."
    )]
    pub passphrase: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RestoreKeyBackupArgs {
    #[schemars(
        description = "The account's recovery key (also called a security key, e.g. \
        \"EsTx xxxx xxxx ...\") or security passphrase. In Element: Settings > Encryption \
        (or Security & Privacy). This is not the account password."
    )]
    pub recovery_key: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DownloadRoomKeysArgs {
    #[schemars(description = "Room id to fetch historical keys for, e.g. !abc123:matrix.org.")]
    pub room_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SendMessageArgs {
    #[schemars(description = "Target room id, e.g. !abc123:matrix.org.")]
    pub room_id: String,
    #[schemars(description = "Message body to send.")]
    pub body: String,
    #[schemars(description = "Render the body as Markdown (default false = plain text).")]
    pub markdown: Option<bool>,
    #[schemars(
        description = "Event id to send this message as a rich reply to. Replying to a message \
        that is part of a thread sends the reply into that thread."
    )]
    pub reply_to_event_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct EditMessageArgs {
    #[schemars(description = "Room id containing the message, e.g. !abc123:matrix.org.")]
    pub room_id: String,
    #[schemars(description = "Event id of the message to edit (must be your own message).")]
    pub event_id: String,
    #[schemars(description = "New message body.")]
    pub body: String,
    #[schemars(description = "Render the body as Markdown (default false = plain text).")]
    pub markdown: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RedactEventArgs {
    #[schemars(description = "Room id containing the event, e.g. !abc123:matrix.org.")]
    pub room_id: String,
    #[schemars(
        description = "Event id to redact - a message (deletes it) or a reaction (un-reacts)."
    )]
    pub event_id: String,
    #[schemars(description = "Optional reason for the redaction.")]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SendReactionArgs {
    #[schemars(description = "Room id containing the message, e.g. !abc123:matrix.org.")]
    pub room_id: String,
    #[schemars(description = "Event id of the message to react to.")]
    pub event_id: String,
    #[schemars(description = "The reaction, typically a single emoji, e.g. \"\u{1F44D}\".")]
    pub emoji: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct MarkReadArgs {
    #[schemars(description = "Room id to mark as read, e.g. !abc123:matrix.org.")]
    pub room_id: String,
    #[schemars(
        description = "Event id to mark as read up to. Defaults to the room's latest message."
    )]
    pub event_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetRoomMembersArgs {
    #[schemars(description = "Room id to list members for, e.g. !abc123:matrix.org.")]
    pub room_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReadMessagesArgs {
    #[schemars(description = "Room id to read from, e.g. !abc123:matrix.org.")]
    pub room_id: String,
    #[schemars(description = "Maximum number of recent messages to return (default 20).")]
    pub limit: Option<u32>,
    #[schemars(
        description = "Continuation token from a previous call's next_token, to page further back in history."
    )]
    pub before_token: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReadThreadArgs {
    #[schemars(description = "Room id containing the thread, e.g. !abc123:matrix.org.")]
    pub room_id: String,
    #[schemars(
        description = "Event id of the thread root, or of any threaded reply in it (the \
        thread_root field on a message from read_messages)."
    )]
    pub event_id: String,
    #[schemars(description = "Maximum number of replies to return (default 50).")]
    pub limit: Option<u32>,
    #[schemars(
        description = "Continuation token from a previous call's next_token, to page further \
        through a long thread. Pass the thread_root event id alongside it."
    )]
    pub from_token: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct JoinRoomArgs {
    #[schemars(description = "Room id (!room:server) or alias (#room:server) to join.")]
    pub room: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CreateRoomArgs {
    #[schemars(description = "Room name.")]
    pub name: Option<String>,
    #[schemars(description = "Room topic.")]
    pub topic: Option<String>,
    #[schemars(description = "User ids to invite, e.g. [\"@bob:matrix.org\"].")]
    pub invite: Option<Vec<String>>,
    #[schemars(
        description = "Make the room publicly joinable and listed (default false = private/invite-only)."
    )]
    pub public: Option<bool>,
    #[schemars(description = "Enable end-to-end encryption for the room (default false).")]
    pub encrypted: Option<bool>,
    #[schemars(description = "Mark this as a direct-message room (default false).")]
    pub is_direct: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct InviteUserArgs {
    #[schemars(description = "Room id to invite into, e.g. !abc123:matrix.org.")]
    pub room_id: String,
    #[schemars(description = "User id to invite, e.g. @bob:matrix.org.")]
    pub user_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct LeaveRoomArgs {
    #[schemars(description = "Room id to leave, e.g. !abc123:matrix.org.")]
    pub room_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RoomMemberActionArgs {
    #[schemars(description = "Room id, e.g. !abc123:matrix.org.")]
    pub room_id: String,
    #[schemars(description = "Target user id, e.g. @bob:matrix.org.")]
    pub user_id: String,
    #[schemars(description = "Optional reason.")]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct UpdateRoomArgs {
    #[schemars(description = "Room id to update, e.g. !abc123:matrix.org.")]
    pub room_id: String,
    #[schemars(description = "New room name.")]
    pub name: Option<String>,
    #[schemars(description = "New room topic.")]
    pub topic: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CreateDmArgs {
    #[schemars(
        description = "User id to start or find a direct message room with, e.g. @bob:matrix.org."
    )]
    pub user_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetProfileArgs {
    #[schemars(
        description = "User id to look up, e.g. @bob:matrix.org. Defaults to the logged-in user."
    )]
    pub user_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SendFileArgs {
    #[schemars(description = "Target room id, e.g. !abc123:matrix.org.")]
    pub room_id: String,
    #[schemars(
        description = "Path to a local file to upload and send. Its MIME type is guessed from the extension."
    )]
    pub path: String,
    #[schemars(description = "Optional caption to send alongside the file.")]
    pub caption: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DownloadMediaArgs {
    #[schemars(description = "Room id containing the media message, e.g. !abc123:matrix.org.")]
    pub room_id: String,
    #[schemars(description = "Event id of the image/audio/video/file message.")]
    pub event_id: String,
    #[schemars(description = "Local path to save the downloaded media to.")]
    pub save_path: String,
}

/// The MCP server handler. Holds a shared [`MatrixManager`].
#[derive(Clone)]
pub struct MatrixServer {
    matrix: Arc<MatrixManager>,
    tool_router: ToolRouter<Self>,
}

impl MatrixServer {
    pub fn new(matrix: Arc<MatrixManager>) -> Self {
        Self {
            matrix,
            tool_router: Self::tool_router(),
        }
    }
}

/// Wrap a serde_json value as a pretty-printed text result.
fn json_result(value: Value) -> Result<CallToolResult, ErrorData> {
    let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
    Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
}

/// Convert an internal error into an MCP tool error.
fn err(e: anyhow::Error) -> ErrorData {
    ErrorData::internal_error(format!("{e:#}"), None)
}

#[tool_router]
impl MatrixServer {
    #[tool(
        description = "Log in to a Matrix homeserver with a username and password. \
        The session is saved to disk and reused on the next start.",
        annotations(read_only_hint = false, destructive_hint = false)
    )]
    async fn login(
        &self,
        Parameters(args): Parameters<LoginArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let info = self
            .matrix
            .login(args.homeserver.as_deref(), &args.username, &args.password)
            .await
            .map_err(err)?;
        json_result(serde_json::json!({
            "logged_in": true,
            "user_id": info.user_id,
            "device_id": info.device_id,
            "homeserver": info.homeserver,
        }))
    }

    #[tool(
        description = "Log in using a pre-obtained access token instead of a password. Use this \
        for homeservers that require SSO/OAuth, which this server cannot complete interactively.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn login_with_token(
        &self,
        Parameters(args): Parameters<LoginWithTokenArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let info = self
            .matrix
            .login_with_token(
                args.homeserver.as_deref(),
                &args.user_id,
                &args.device_id,
                &args.access_token,
            )
            .await
            .map_err(err)?;
        json_result(serde_json::json!({
            "logged_in": true,
            "user_id": info.user_id,
            "device_id": info.device_id,
            "homeserver": info.homeserver,
        }))
    }

    #[tool(
        description = "Log in via the homeserver's SSO flow, for homeservers that require SSO/OAuth. \
        Opens a local callback listener, gets the SSO URL, opens it in the default browser \
        (best-effort - if that fails, open the returned sso_url manually), and waits up to 5 \
        minutes for the browser flow to complete. This call blocks until you finish signing in.",
        annotations(read_only_hint = false, destructive_hint = false)
    )]
    async fn login_sso(
        &self,
        Parameters(args): Parameters<LoginSsoArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let (info, sso_url) = self
            .matrix
            .login_sso(
                args.homeserver.as_deref(),
                args.identity_provider_id.as_deref(),
            )
            .await
            .map_err(err)?;
        json_result(serde_json::json!({
            "logged_in": true,
            "user_id": info.user_id,
            "device_id": info.device_id,
            "homeserver": info.homeserver,
            "sso_url": sso_url,
        }))
    }

    #[tool(
        description = "Report the current login state: user id, device, homeserver, and joined room count.",
        annotations(read_only_hint = true)
    )]
    async fn whoami(&self) -> Result<CallToolResult, ErrorData> {
        json_result(self.matrix.whoami().await)
    }

    #[tool(
        description = "Log out of the current session, invalidating the access token and \
        deleting the saved session and local encryption store, so the next start requires \
        logging in again. Run this before uninstalling to leave nothing behind.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true
        )
    )]
    async fn logout(&self) -> Result<CallToolResult, ErrorData> {
        self.matrix.logout().await.map_err(err)?;
        json_result(serde_json::json!({ "logged_out": true }))
    }

    #[tool(
        description = "Run a single sync against the homeserver to refresh the local room list and state.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn sync(&self) -> Result<CallToolResult, ErrorData> {
        self.matrix.sync().await.map_err(err)?;
        json_result(serde_json::json!({ "synced": true }))
    }

    #[tool(
        description = "Set up a server-side key backup for this account and upload this device's \
        room keys, returning a newly generated recovery key. Use this when the account has no \
        backup yet. The recovery key is shown once and cannot be recovered - save it.",
        annotations(read_only_hint = false, destructive_hint = false)
    )]
    async fn enable_key_backup(
        &self,
        Parameters(args): Parameters<EnableKeyBackupArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let result = self
            .matrix
            .enable_key_backup(args.passphrase.as_deref())
            .await
            .map_err(err)?;
        json_result(result)
    }

    #[tool(
        description = "Unlock the server-side key backup with a recovery key, so encrypted \
        messages sent before this device existed can be decrypted. Needed once per device; \
        after this, reading a room automatically pulls its historical keys.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn restore_key_backup(
        &self,
        Parameters(args): Parameters<RestoreKeyBackupArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let result = self
            .matrix
            .restore_key_backup(&args.recovery_key)
            .await
            .map_err(err)?;
        json_result(result)
    }

    #[tool(
        description = "Download a room's historical message keys from the server-side key backup. \
        Requires `restore_key_backup` to have been run first. `read_messages` does this \
        automatically when it hits undecryptable messages, so this is mainly for forcing a refresh.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn download_room_keys(
        &self,
        Parameters(args): Parameters<DownloadRoomKeysArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let result = self
            .matrix
            .download_room_keys(&args.room_id)
            .await
            .map_err(err)?;
        json_result(result)
    }

    #[tool(
        description = "Start verifying this device against your other, already-verified session \
        (e.g. Element). Sends the verification request that session is waiting for and returns \
        emoji to compare. Once verified, this device is gossiped the keys to read history - no \
        recovery key needed. This call blocks until the emoji are ready; then compare them and \
        call confirm_device_verification (or cancel_device_verification if they differ).",
        annotations(read_only_hint = false, destructive_hint = false)
    )]
    async fn start_device_verification(&self) -> Result<CallToolResult, ErrorData> {
        let result = self.matrix.start_device_verification().await.map_err(err)?;
        json_result(result)
    }

    #[tool(
        description = "Continue an in-progress device verification: after you've accepted the \
        request in your other session, this fetches the emoji to compare. Returns status \
        \"pending\" if the other session hasn't accepted yet - just call it again.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn continue_device_verification(&self) -> Result<CallToolResult, ErrorData> {
        let result = self
            .matrix
            .continue_device_verification()
            .await
            .map_err(err)?;
        json_result(result)
    }

    #[tool(
        description = "Confirm the emoji from start_device_verification match your other session, \
        completing verification. Afterwards this device receives the cross-signing secrets and \
        key-backup key automatically and can read encrypted history.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn confirm_device_verification(&self) -> Result<CallToolResult, ErrorData> {
        let result = self
            .matrix
            .confirm_device_verification()
            .await
            .map_err(err)?;
        json_result(result)
    }

    #[tool(
        description = "Abort an in-progress device verification (e.g. the emoji did not match).",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn cancel_device_verification(&self) -> Result<CallToolResult, ErrorData> {
        let result = self
            .matrix
            .cancel_device_verification()
            .await
            .map_err(err)?;
        json_result(result)
    }

    #[tool(
        description = "List the rooms the logged-in account has joined, with id, name, topic, and encryption state.",
        annotations(read_only_hint = true)
    )]
    async fn list_rooms(&self) -> Result<CallToolResult, ErrorData> {
        let rooms = self.matrix.list_rooms().await.map_err(err)?;
        json_result(rooms)
    }

    #[tool(
        description = "Send a text message to a room. Set markdown=true to format the body as \
        Markdown. Set reply_to_event_id to send as a rich reply to an existing message; if that \
        message is part of a thread, the reply is sent into that thread, and the response's \
        thread_root says which one.",
        annotations(read_only_hint = false, destructive_hint = false)
    )]
    async fn send_message(
        &self,
        Parameters(args): Parameters<SendMessageArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let (event_id, thread_root) = self
            .matrix
            .send_message(
                &args.room_id,
                &args.body,
                args.markdown.unwrap_or(false),
                args.reply_to_event_id.as_deref(),
            )
            .await
            .map_err(err)?;
        json_result(serde_json::json!({
            "sent": true,
            "room_id": args.room_id,
            "event_id": event_id,
            "thread_root": thread_root,
        }))
    }

    #[tool(
        description = "Edit a previously-sent message (only the original sender can edit it).",
        annotations(read_only_hint = false, destructive_hint = false)
    )]
    async fn edit_message(
        &self,
        Parameters(args): Parameters<EditMessageArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let event_id = self
            .matrix
            .edit_message(
                &args.room_id,
                &args.event_id,
                &args.body,
                args.markdown.unwrap_or(false),
            )
            .await
            .map_err(err)?;
        json_result(serde_json::json!({
            "edited": true,
            "room_id": args.room_id,
            "event_id": event_id,
        }))
    }

    #[tool(
        description = "Redact (delete) an event - a message, to remove it, or a reaction, to un-react.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true
        )
    )]
    async fn redact_event(
        &self,
        Parameters(args): Parameters<RedactEventArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let redaction_event_id = self
            .matrix
            .redact_event(&args.room_id, &args.event_id, args.reason.as_deref())
            .await
            .map_err(err)?;
        json_result(serde_json::json!({
            "redacted": true,
            "room_id": args.room_id,
            "event_id": args.event_id,
            "redaction_event_id": redaction_event_id,
        }))
    }

    #[tool(
        description = "React to a message with an emoji.",
        annotations(read_only_hint = false, destructive_hint = false)
    )]
    async fn send_reaction(
        &self,
        Parameters(args): Parameters<SendReactionArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let event_id = self
            .matrix
            .send_reaction(&args.room_id, &args.event_id, &args.emoji)
            .await
            .map_err(err)?;
        json_result(serde_json::json!({
            "reacted": true,
            "room_id": args.room_id,
            "event_id": event_id,
        }))
    }

    #[tool(
        description = "Mark a room as read up to a given event (or the latest message if omitted).",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn mark_read(
        &self,
        Parameters(args): Parameters<MarkReadArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let event_id = self
            .matrix
            .mark_read(&args.room_id, args.event_id.as_deref())
            .await
            .map_err(err)?;
        json_result(serde_json::json!({
            "marked_read": true,
            "room_id": args.room_id,
            "event_id": event_id,
        }))
    }

    #[tool(
        description = "List the members of a room with their user id, display name, membership state, and power level.",
        annotations(read_only_hint = true)
    )]
    async fn get_room_members(
        &self,
        Parameters(args): Parameters<GetRoomMembersArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let members = self
            .matrix
            .get_room_members(&args.room_id)
            .await
            .map_err(err)?;
        json_result(members)
    }

    #[tool(
        description = "Read messages from a room, in chronological order. Without before_token, \
        starts from the most recent message; pass the response's next_token as before_token to \
        page further back in history (next_token is null once the start of the room is reached). \
        End-to-end encrypted messages are decrypted automatically when the keys are available; \
        any that cannot be decrypted are flagged with unable_to_decrypt=true.",
        annotations(read_only_hint = true)
    )]
    async fn read_messages(
        &self,
        Parameters(args): Parameters<ReadMessagesArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let limit = args.limit.unwrap_or(20).clamp(1, 100);
        let messages = self
            .matrix
            .read_messages(&args.room_id, limit, args.before_token.as_deref())
            .await
            .map_err(err)?;
        json_result(messages)
    }

    #[tool(
        description = "Read a single conversation thread: the thread root message followed by \
        its replies, oldest first. Threaded replies are interleaved with everything else in \
        read_messages output, so use this to read one thread as a conversation. Accepts the \
        root's event id or that of any reply in the thread. Pass the response's next_token as \
        from_token to page through a long thread. Rich replies (send_message's \
        reply_to_event_id) are not threads - read those with read_messages.",
        annotations(read_only_hint = true)
    )]
    async fn read_thread(
        &self,
        Parameters(args): Parameters<ReadThreadArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let limit = args.limit.unwrap_or(50).clamp(1, 100);
        let thread = self
            .matrix
            .read_thread(
                &args.room_id,
                &args.event_id,
                limit,
                args.from_token.as_deref(),
            )
            .await
            .map_err(err)?;
        json_result(thread)
    }

    #[tool(
        description = "Join a room by its id (!room:server) or alias (#room:server).",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn join_room(
        &self,
        Parameters(args): Parameters<JoinRoomArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let room_id = self.matrix.join_room(&args.room).await.map_err(err)?;
        json_result(serde_json::json!({ "joined": true, "room_id": room_id }))
    }

    #[tool(
        description = "Create a new room, optionally with a name, topic, invited users, \
        public visibility, encryption, or as a direct message.",
        annotations(read_only_hint = false, destructive_hint = false)
    )]
    async fn create_room(
        &self,
        Parameters(args): Parameters<CreateRoomArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let room_id = self
            .matrix
            .create_room(
                args.name.as_deref(),
                args.topic.as_deref(),
                args.invite.as_deref().unwrap_or_default(),
                args.public.unwrap_or(false),
                args.encrypted.unwrap_or(false),
                args.is_direct.unwrap_or(false),
            )
            .await
            .map_err(err)?;
        json_result(serde_json::json!({ "created": true, "room_id": room_id }))
    }

    #[tool(
        description = "Invite a user to a room.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn invite_user(
        &self,
        Parameters(args): Parameters<InviteUserArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.matrix
            .invite_user(&args.room_id, &args.user_id)
            .await
            .map_err(err)?;
        json_result(serde_json::json!({
            "invited": true,
            "room_id": args.room_id,
            "user_id": args.user_id,
        }))
    }

    #[tool(
        description = "Leave a room.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true
        )
    )]
    async fn leave_room(
        &self,
        Parameters(args): Parameters<LeaveRoomArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.matrix.leave_room(&args.room_id).await.map_err(err)?;
        json_result(serde_json::json!({ "left": true, "room_id": args.room_id }))
    }

    #[tool(
        description = "Kick a member from a room, optionally with a reason.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true
        )
    )]
    async fn kick_room_member(
        &self,
        Parameters(args): Parameters<RoomMemberActionArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.matrix
            .kick_room_member(&args.room_id, &args.user_id, args.reason.as_deref())
            .await
            .map_err(err)?;
        json_result(serde_json::json!({
            "kicked": true,
            "room_id": args.room_id,
            "user_id": args.user_id,
        }))
    }

    #[tool(
        description = "Ban a member from a room, optionally with a reason.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true
        )
    )]
    async fn ban_room_member(
        &self,
        Parameters(args): Parameters<RoomMemberActionArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.matrix
            .ban_room_member(&args.room_id, &args.user_id, args.reason.as_deref())
            .await
            .map_err(err)?;
        json_result(serde_json::json!({
            "banned": true,
            "room_id": args.room_id,
            "user_id": args.user_id,
        }))
    }

    #[tool(
        description = "Unban a previously-banned member from a room, optionally with a reason.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn unban_room_member(
        &self,
        Parameters(args): Parameters<RoomMemberActionArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.matrix
            .unban_room_member(&args.room_id, &args.user_id, args.reason.as_deref())
            .await
            .map_err(err)?;
        json_result(serde_json::json!({
            "unbanned": true,
            "room_id": args.room_id,
            "user_id": args.user_id,
        }))
    }

    #[tool(
        description = "Update a room's name and/or topic. Only the fields provided are changed.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn update_room(
        &self,
        Parameters(args): Parameters<UpdateRoomArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.matrix
            .update_room(&args.room_id, args.name.as_deref(), args.topic.as_deref())
            .await
            .map_err(err)?;
        json_result(serde_json::json!({ "updated": true, "room_id": args.room_id }))
    }

    #[tool(
        description = "Get or create a direct-message room with a user. Reuses an existing DM if one already exists.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn create_dm(
        &self,
        Parameters(args): Parameters<CreateDmArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let room_id = self.matrix.create_dm(&args.user_id).await.map_err(err)?;
        json_result(serde_json::json!({ "room_id": room_id, "user_id": args.user_id }))
    }

    #[tool(
        description = "Look up a user's profile (display name and avatar). Defaults to the logged-in user.",
        annotations(read_only_hint = true)
    )]
    async fn get_profile(
        &self,
        Parameters(args): Parameters<GetProfileArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let profile = self
            .matrix
            .get_profile(args.user_id.as_deref())
            .await
            .map_err(err)?;
        json_result(profile)
    }

    #[tool(
        description = "Upload a local file and send it to a room as an image, audio, video, \
        or generic file attachment (chosen automatically from its MIME type).",
        annotations(read_only_hint = false, destructive_hint = false)
    )]
    async fn send_file(
        &self,
        Parameters(args): Parameters<SendFileArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let event_id = self
            .matrix
            .send_file(&args.room_id, &args.path, args.caption.as_deref())
            .await
            .map_err(err)?;
        json_result(serde_json::json!({
            "sent": true,
            "room_id": args.room_id,
            "event_id": event_id,
        }))
    }

    #[tool(
        description = "Download the media attached to a message event and save it to a local path.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true
        )
    )]
    async fn download_media(
        &self,
        Parameters(args): Parameters<DownloadMediaArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let result = self
            .matrix
            .download_media(&args.room_id, &args.event_id, &args.save_path)
            .await
            .map_err(err)?;
        json_result(result)
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for MatrixServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(
                "Matrix MCP server. Tools let you log in to a Matrix homeserver, list joined \
                 rooms, read and send messages, and join rooms. Start with `whoami` to check the \
                 login state, then `login` if needed. End-to-end encryption is supported: \
                 messages are encrypted and decrypted automatically using a persistent key store. \
                 Decrypting older history may require running `sync` so the device receives room \
                 keys; messages with no available key are flagged unable_to_decrypt.",
            );
        info.server_info.name = env!("CARGO_PKG_NAME").to_string();
        info.server_info.version = env!("CARGO_PKG_VERSION").to_string();
        info.server_info.title = Some("Matrix MCP".to_string());
        info.server_info.description =
            Some("A Model Context Protocol server for the Matrix chat protocol.".to_string());
        info
    }
}
