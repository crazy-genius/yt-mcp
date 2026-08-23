use crate::mcp::{
    AddCommentArgs, CreateIssueArgs, FindIssueArgs, MyIssuesArgs, SearchIssuesArgs,
    YoutrackMCPServer,
};
use rmcp::model::ContentBlock;
use rmcp::{ErrorData as McpError, model::CallToolResult, tool, tool_router};
use youtrack_client_wrapper::Issue;

#[tool_router(router = "youtrack_issues", vis = "pub")]
impl YoutrackMCPServer {
    #[tool(
        name = "youtrack_find_issue",
        description = "Get complete information about a YouTrack issue by its human-readable ID. Optional 'fields' selects returned fields (see youtrack://reference/fields)."
    )]
    pub async fn find_issue(
        &self,
        params: rmcp::handler::server::wrapper::Parameters<FindIssueArgs>,
    ) -> Result<CallToolResult, McpError> {
        let issue: Issue =
            self.service.find_issue(&params.0.id, params.0.fields).await.map_err(Self::map_err)?;
        Self::json_result(issue)
    }

    #[tool(
        name = "youtrack_search_issues",
        description = "Search YouTrack issues using YouTrack query language. Optional 'fields' selects returned fields (see youtrack://reference/fields)."
    )]
    pub async fn search_issues(
        &self,
        params: rmcp::handler::server::wrapper::Parameters<SearchIssuesArgs>,
    ) -> Result<CallToolResult, McpError> {
        let issues = self
            .service
            .search_issues(&params.0.query, params.0.fields)
            .await
            .map_err(Self::map_err)?;
        Self::json_result(issues)
    }

    #[tool(
        name = "youtrack_my_issues",
        description = "List open (unresolved) issues assigned to a given YouTrack login"
    )]
    pub async fn my_issues(
        &self,
        params: rmcp::handler::server::wrapper::Parameters<MyIssuesArgs>,
    ) -> Result<CallToolResult, McpError> {
        let issues = self.service.my_issues(&params.0.assignee).await.map_err(Self::map_err)?;
        Self::json_result(issues)
    }

    #[tool(
        name = "youtrack_create_issue",
        description = "DESTRUCTIVE: requires explicit user confirmation in the dialog before calling. Creates a new YouTrack issue in the given project (shortName)"
    )]
    pub async fn create_issue(
        &self,
        params: rmcp::handler::server::wrapper::Parameters<CreateIssueArgs>,
    ) -> Result<CallToolResult, McpError> {
        let args = params.0;
        let issue: Issue = self
            .service
            .create_issue(&args.project, &args.summary, args.description.as_deref())
            .await
            .map_err(Self::map_write_err)?;
        Self::json_result(issue)
    }

    #[tool(
        name = "youtrack_add_comment",
        description = "DESTRUCTIVE: requires explicit user confirmation in the dialog before calling. Adds a comment to the issue"
    )]
    pub async fn add_comment(
        &self,
        params: rmcp::handler::server::wrapper::Parameters<AddCommentArgs>,
    ) -> Result<CallToolResult, McpError> {
        let args = params.0;
        self.service.add_comment(&args.issue, &args.text).await.map_err(Self::map_write_err)?;
        Ok(CallToolResult::success(vec![ContentBlock::text(format!(
            "OK: comment added to {}",
            args.issue
        ))]))
    }
}
