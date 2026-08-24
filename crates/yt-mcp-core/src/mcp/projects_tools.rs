use crate::mcp::{UserToken, YoutrackMCPServer};
use rmcp::handler::server::tool::Extension;
use rmcp::{ErrorData as McpError, model::CallToolResult, tool, tool_router};

#[tool_router(router = "youtrack_projects", vis = "pub")]
impl YoutrackMCPServer {
    #[tool(
        name = "youtrack_list_projects",
        description = "List YouTrack projects (id, shortName, name)",
        annotations(title = "List projects", read_only_hint = true, open_world_hint = true)
    )]
    pub async fn list_projects(
        &self,
        Extension(token): Extension<UserToken>,
    ) -> Result<CallToolResult, McpError> {
        let yt = self.as_user(&token)?;
        match yt.list_projects().await {
            Ok(projects) => Self::json_result(projects),
            Err(e) => Ok(Self::tool_error(e)),
        }
    }
}
