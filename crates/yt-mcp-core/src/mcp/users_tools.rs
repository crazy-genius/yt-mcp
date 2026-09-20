use crate::mcp::{SearchUsersArgs, UserToken, YoutrackMCPServer};
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{ErrorData as McpError, model::CallToolResult, tool, tool_router};

#[tool_router(router = "youtrack_users", vis = "pub")]
impl YoutrackMCPServer {
    #[tool(
        name = "youtrack_whoami",
        description = "Who the current YouTrack token belongs to: login, full name, email. Call this when the user says 'my issues', 'assign it to me' or 'mention me' without naming a login.",
        annotations(title = "Who am I", read_only_hint = true, open_world_hint = true)
    )]
    pub async fn whoami(
        &self,
        Extension(token): Extension<UserToken>,
    ) -> Result<CallToolResult, McpError> {
        let yt = self.as_user(&token)?;
        match yt.whoami().await {
            Ok(user) => Self::json_result(user),
            Err(e) => Ok(Self::tool_error(e)),
        }
    }

    #[tool(
        name = "youtrack_search_users",
        description = "Find people by a substring of their login, full name or email, and get the 'login' needed for an @mention or for 'assignee <login>' in youtrack_apply_command. Paginated: the result carries 'matched', 'next_skip' and 'scan_truncated' (true = not every user was scanned, narrow the query). Banned accounts are never returned.",
        annotations(title = "Search users", read_only_hint = true, open_world_hint = true)
    )]
    pub async fn search_users(
        &self,
        params: Parameters<SearchUsersArgs>,
        Extension(token): Extension<UserToken>,
    ) -> Result<CallToolResult, McpError> {
        let args = params.0;
        let yt = self.as_user(&token)?;
        match yt.search_users(&args.query, args.limit, args.skip).await {
            Ok(page) => Self::json_result(page),
            Err(e) => Ok(Self::tool_error(e)),
        }
    }
}
