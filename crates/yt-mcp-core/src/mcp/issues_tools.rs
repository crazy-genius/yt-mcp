use crate::mcp::{
    CreateIssueArgs, FindIssueArgs, MyIssuesArgs, SearchIssuesArgs, UpdateIssueArgs, UserToken,
    YoutrackMCPServer,
};
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{ErrorData as McpError, model::CallToolResult, tool, tool_router};

#[tool_router(router = "youtrack_issues", vis = "pub")]
impl YoutrackMCPServer {
    #[tool(
        name = "youtrack_find_issue",
        description = "Get complete information about a YouTrack issue by its human-readable ID. Optional 'fields' selects returned fields (see youtrack_reference topic 'issue-fields' (resource youtrack://reference/issues/fields)).",
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
        description = "Search YouTrack issues using YouTrack query language. Optional 'fields' selects returned fields (see youtrack_reference topic 'issue-fields' (resource youtrack://reference/issues/fields)). Paginated with 'top' (default 50, capped at 100) and 'skip'; the response carries next_skip.",
        annotations(title = "Search issues", read_only_hint = true, open_world_hint = true)
    )]
    pub async fn search_issues(
        &self,
        params: Parameters<SearchIssuesArgs>,
        Extension(token): Extension<UserToken>,
    ) -> Result<CallToolResult, McpError> {
        let yt = self.as_user(&token)?;
        match yt.search_issues(&params.0.query, params.0.fields, params.0.top, params.0.skip).await
        {
            Ok(page) => Self::json_result(page),
            Err(e) => Ok(Self::tool_error(e)),
        }
    }

    #[tool(
        name = "youtrack_my_issues",
        description = "List open (unresolved) issues assigned to a given YouTrack login. Paginated with 'top' (default 50, capped at 100) and 'skip'. The login comes from youtrack_whoami.",
        annotations(title = "My open issues", read_only_hint = true, open_world_hint = true)
    )]
    pub async fn my_issues(
        &self,
        params: Parameters<MyIssuesArgs>,
        Extension(token): Extension<UserToken>,
    ) -> Result<CallToolResult, McpError> {
        let yt = self.as_user(&token)?;
        match yt.my_issues(&params.0.assignee, params.0.top, params.0.skip).await {
            Ok(page) => Self::json_result(page),
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

    #[tool(
        name = "youtrack_update_issue",
        description = "Changes the summary and/or replaces the whole description of an existing YouTrack issue. State, assignee and other fields change through youtrack_apply_command",
        annotations(
            title = "Edit issue summary/description",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    pub async fn update_issue(
        &self,
        params: Parameters<UpdateIssueArgs>,
        Extension(token): Extension<UserToken>,
    ) -> Result<CallToolResult, McpError> {
        let args = params.0;
        // Пустой вызов дошёл бы до YouTrack пустым POST и вернул бы карточку как
        // «успех» — модель решила бы, что правка сохранена.
        if args.summary.is_none() && args.description.is_none() {
            return Ok(CallToolResult::error(vec![rmcp::model::ContentBlock::text(
                "nothing to update: pass 'summary' and/or 'description'",
            )]));
        }
        let yt = self.as_user(&token)?;
        match yt.update_issue(&args.id, args.summary.as_deref(), args.description.as_deref()).await
        {
            Ok(issue) => Self::json_result(issue),
            Err(e) => Ok(Self::tool_error(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use yt_client::Youtrack;

    /// Пустая правка отказывается до сети: адрес YouTrack здесь недостижим, и
    /// ответ без обращения к нему доказывает, что запрос не ушёл.
    #[tokio::test]
    async fn an_update_with_nothing_to_change_is_refused_locally() {
        let server =
            YoutrackMCPServer::new(Arc::new(Youtrack::new("http://127.0.0.1:9").expect("builds")));
        let args = UpdateIssueArgs { id: "DEMO-1".into(), summary: None, description: None };
        let result = server
            .update_issue(Parameters(args), Extension(UserToken("t".into())))
            .await
            .expect("a tool error, not a protocol error");
        assert_eq!(result.is_error, Some(true));
        assert!(format!("{:?}", result.content).contains("nothing to update"));
    }
}
