use crate::mcp::YoutrackMCPServer;
use rmcp::{ErrorData as McpError, model::CallToolResult, tool, tool_router};

#[tool_router(router = "youtrack_projects", vis = "pub")]
impl YoutrackMCPServer {
    #[tool(
        name = "youtrack_list_projects",
        description = "List YouTrack projects (id, shortName, name)"
    )]
    pub async fn list_projects(&self) -> Result<CallToolResult, McpError> {
        let projects = self.service.list_projects().await.map_err(Self::map_write_err)?;
        Self::json_result(projects)
    }
}
