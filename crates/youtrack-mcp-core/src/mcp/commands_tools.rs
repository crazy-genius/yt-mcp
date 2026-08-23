use crate::mcp::{ApplyCommandArgs, YoutrackMCPServer};
use rmcp::model::ContentBlock;
use rmcp::{ErrorData as McpError, model::CallToolResult, tool, tool_router};

#[tool_router(router = "youtrack_commands", vis = "pub")]
impl YoutrackMCPServer {
    #[tool(
        name = "youtrack_apply_command",
        description = "DESTRUCTIVE: requires explicit user confirmation in the dialog before calling. Applies a YouTrack command (state/assignee/tag changes) to the issue, e.g. 'state Fixed'"
    )]
    pub async fn apply_command(
        &self,
        params: rmcp::handler::server::wrapper::Parameters<ApplyCommandArgs>,
    ) -> Result<CallToolResult, McpError> {
        let args = params.0;
        self.service
            .apply_command(&args.issue, &args.command, args.comment.as_deref())
            .await
            .map_err(Self::map_write_err)?;
        Ok(CallToolResult::success(vec![ContentBlock::text(format!(
            "OK: applied '{}' to {}",
            args.command, args.issue
        ))]))
    }
}
