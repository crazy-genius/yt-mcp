use crate::mcp::{ApplyCommandArgs, UserToken, YoutrackMCPServer};
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::ContentBlock;
use rmcp::{ErrorData as McpError, model::CallToolResult, tool, tool_router};

#[tool_router(router = "youtrack_commands", vis = "pub")]
impl YoutrackMCPServer {
    #[tool(
        name = "youtrack_apply_command",
        description = "Applies a YouTrack command (state/assignee/tag changes) to the issue, e.g. 'state Fixed'",
        annotations(
            title = "Apply command",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    pub async fn apply_command(
        &self,
        params: Parameters<ApplyCommandArgs>,
        Extension(token): Extension<UserToken>,
    ) -> Result<CallToolResult, McpError> {
        let args = params.0;
        let yt = self.as_user(&token)?;
        match yt.apply_command(&args.issue, &args.command, args.comment.as_deref()).await {
            Ok(()) => Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "OK: applied '{}' to {}",
                args.command, args.issue
            ))])),
            Err(e) => Ok(Self::tool_error(e)),
        }
    }
}
