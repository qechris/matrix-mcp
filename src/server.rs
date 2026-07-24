//! The MCP server: exposes Matrix operations as tools over stdio.

use std::sync::Arc;

use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, Content, ServerCapabilities, ServerInfo},
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
pub struct SendMessageArgs {
    #[schemars(description = "Target room id, e.g. !abc123:matrix.org.")]
    pub room_id: String,
    #[schemars(description = "Message body to send.")]
    pub body: String,
    #[schemars(description = "Render the body as Markdown (default false = plain text).")]
    pub markdown: Option<bool>,
    #[schemars(description = "Event id to send this message as a rich reply to.")]
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
    Ok(CallToolResult::success(vec![Content::text(text)]))
}

/// Convert an internal error into an MCP tool error.
fn err(e: anyhow::Error) -> ErrorData {
    ErrorData::internal_error(format!("{e:#}"), None)
}

#[tool_router]
impl MatrixServer {
    #[tool(
        description = "Log in to a Matrix homeserver with a username and password. \
        The session is saved to disk and reused on the next start."
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
        for homeservers that require SSO/OAuth, which this server cannot complete interactively."
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
        minutes for the browser flow to complete. This call blocks until you finish signing in."
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
        description = "Report the current login state: user id, device, homeserver, and joined room count."
    )]
    async fn whoami(&self) -> Result<CallToolResult, ErrorData> {
        json_result(self.matrix.whoami().await)
    }

    #[tool(
        description = "Log out of the current session, invalidating the access token and \
        clearing the saved session so the next start requires logging in again."
    )]
    async fn logout(&self) -> Result<CallToolResult, ErrorData> {
        self.matrix.logout().await.map_err(err)?;
        json_result(serde_json::json!({ "logged_out": true }))
    }

    #[tool(
        description = "Run a single sync against the homeserver to refresh the local room list and state."
    )]
    async fn sync(&self) -> Result<CallToolResult, ErrorData> {
        self.matrix.sync().await.map_err(err)?;
        json_result(serde_json::json!({ "synced": true }))
    }

    #[tool(
        description = "List the rooms the logged-in account has joined, with id, name, topic, and encryption state."
    )]
    async fn list_rooms(&self) -> Result<CallToolResult, ErrorData> {
        let rooms = self.matrix.list_rooms().await.map_err(err)?;
        json_result(rooms)
    }

    #[tool(
        description = "Send a text message to a room. Set markdown=true to format the body as \
        Markdown. Set reply_to_event_id to send as a rich reply to an existing message."
    )]
    async fn send_message(
        &self,
        Parameters(args): Parameters<SendMessageArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let event_id = self
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
        }))
    }

    #[tool(description = "Edit a previously-sent message (only the original sender can edit it).")]
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
        description = "Redact (delete) an event - a message, to remove it, or a reaction, to un-react."
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

    #[tool(description = "React to a message with an emoji.")]
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
        description = "Mark a room as read up to a given event (or the latest message if omitted)."
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
        description = "List the members of a room with their user id, display name, membership state, and power level."
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
        any that cannot be decrypted are flagged with unable_to_decrypt=true."
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

    #[tool(description = "Join a room by its id (!room:server) or alias (#room:server).")]
    async fn join_room(
        &self,
        Parameters(args): Parameters<JoinRoomArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let room_id = self.matrix.join_room(&args.room).await.map_err(err)?;
        json_result(serde_json::json!({ "joined": true, "room_id": room_id }))
    }

    #[tool(
        description = "Create a new room, optionally with a name, topic, invited users, \
        public visibility, encryption, or as a direct message."
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

    #[tool(description = "Invite a user to a room.")]
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

    #[tool(description = "Leave a room.")]
    async fn leave_room(
        &self,
        Parameters(args): Parameters<LeaveRoomArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.matrix.leave_room(&args.room_id).await.map_err(err)?;
        json_result(serde_json::json!({ "left": true, "room_id": args.room_id }))
    }

    #[tool(description = "Kick a member from a room, optionally with a reason.")]
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

    #[tool(description = "Ban a member from a room, optionally with a reason.")]
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

    #[tool(description = "Unban a previously-banned member from a room, optionally with a reason.")]
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
        description = "Update a room's name and/or topic. Only the fields provided are changed."
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
        description = "Get or create a direct-message room with a user. Reuses an existing DM if one already exists."
    )]
    async fn create_dm(
        &self,
        Parameters(args): Parameters<CreateDmArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let room_id = self.matrix.create_dm(&args.user_id).await.map_err(err)?;
        json_result(serde_json::json!({ "room_id": room_id, "user_id": args.user_id }))
    }

    #[tool(
        description = "Look up a user's profile (display name and avatar). Defaults to the logged-in user."
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
        or generic file attachment (chosen automatically from its MIME type)."
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
        description = "Download the media attached to a message event and save it to a local path."
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
