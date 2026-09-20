use crate::mcp::{
    CreateIssueArgs, FindIssueArgs, MyIssuesArgs, SearchIssuesArgs, UserToken, YoutrackMCPServer,
};
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{ErrorData as McpError, model::CallToolResult, tool, tool_router};

#[tool_router(router = "youtrack_issues", vis = "pub")]
impl YoutrackMCPServer {
    #[tool(
        name = "youtrack_find_issue",
        description = "Get complete information about a YouTrack issue by its human-readable ID. Optional 'fields' selects returned fields (see youtrack://reference/fields).",
        annotations(title = "Find issue", read_only_hint = true, open_world_hint = true)
    )]
    pub async fn find_issue(
        &self,
        params: Parameters<FindIssueArgs>,
        Extension(token): Extension<UserToken>,
    ) -> Result<CallToolResult, McpError> {
        let yt = self.as_user(&token)?;
        match yt.find_issue(&params.0.id, params.0.fields).await {
            Ok(issue) => Self::json_result(issue),
            Err(e) => Ok(Self::tool_error(e)),
        }
    }

    #[tool(
        name = "youtrack_search_issues",
        description = "Search YouTrack issues using YouTrack query language. Optional 'fields' selects returned fields (see youtrack://reference/fields).",
        annotations(title = "Search issues", read_only_hint = true, open_world_hint = true)
    )]
    pub async fn search_issues(
        &self,
        params: Parameters<SearchIssuesArgs>,
        Extension(token): Extension<UserToken>,
    ) -> Result<CallToolResult, McpError> {
        let yt = self.as_user(&token)?;
        match yt.search_issues(&params.0.query, params.0.fields).await {
            Ok(issues) => Self::json_result(issues),
            Err(e) => Ok(Self::tool_error(e)),
        }
    }

    #[tool(
        name = "youtrack_my_issues",
        description = "List open (unresolved) issues assigned to a given YouTrack login. If the user said 'my issues' without naming a login, get it from youtrack_whoami first.",
        annotations(title = "My open issues", read_only_hint = true, open_world_hint = true)
    )]
    pub async fn my_issues(
        &self,
        params: Parameters<MyIssuesArgs>,
        Extension(token): Extension<UserToken>,
    ) -> Result<CallToolResult, McpError> {
        let yt = self.as_user(&token)?;
        match yt.my_issues(&params.0.assignee).await {
            Ok(issues) => Self::json_result(issues),
            Err(e) => Ok(Self::tool_error(e)),
        }
    }

    #[tool(
        name = "youtrack_create_issue",
        description = "Creates a new YouTrack issue in the given project (shortName)",
        annotations(
            title = "Create issue",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    pub async fn create_issue(
        &self,
        params: Parameters<CreateIssueArgs>,
        Extension(token): Extension<UserToken>,
    ) -> Result<CallToolResult, McpError> {
        let args = params.0;
        let yt = self.as_user(&token)?;
        match yt.create_issue(&args.project, &args.summary, args.description.as_deref()).await {
            Ok(issue) => Self::json_result(issue),
            Err(e) => Ok(Self::tool_error(e)),
        }
    }
}
