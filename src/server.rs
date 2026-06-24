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
pub struct SendMessageArgs {
    #[schemars(description = "Target room id, e.g. !abc123:matrix.org.")]
    pub room_id: String,
    #[schemars(description = "Message body to send.")]
    pub body: String,
    #[schemars(description = "Render the body as Markdown (default false = plain text).")]
    pub markdown: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReadMessagesArgs {
    #[schemars(description = "Room id to read from, e.g. !abc123:matrix.org.")]
    pub room_id: String,
    #[schemars(description = "Maximum number of recent messages to return (default 20).")]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct JoinRoomArgs {
    #[schemars(description = "Room id (!room:server) or alias (#room:server) to join.")]
    pub room: String,
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
        description = "Report the current login state: user id, device, homeserver, and joined room count."
    )]
    async fn whoami(&self) -> Result<CallToolResult, ErrorData> {
        json_result(self.matrix.whoami().await)
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
        description = "Send a text message to a room. Set markdown=true to format the body as Markdown."
    )]
    async fn send_message(
        &self,
        Parameters(args): Parameters<SendMessageArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let event_id = self
            .matrix
            .send_message(&args.room_id, &args.body, args.markdown.unwrap_or(false))
            .await
            .map_err(err)?;
        json_result(serde_json::json!({
            "sent": true,
            "room_id": args.room_id,
            "event_id": event_id,
        }))
    }

    #[tool(
        description = "Read the most recent messages from a room, in chronological order. \
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
            .read_messages(&args.room_id, limit)
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
