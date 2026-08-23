mod articles_tools;
mod commands_tools;
mod issues_tools;
mod projects_tools;

use crate::references::{ARTICLE_FIELDS, ISSUE_FIELDS, QUERY_SYNTAX};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, ListResourcesResult, PaginatedRequestParams,
    ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, Resource,
    ResourceContents, ServerCapabilities, ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler,
    handler::server::tool::{ToolCallContext, ToolRouter},
    model::{CallToolResult, ContentBlock},
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use youtrack_client_wrapper::{YoutrackClientBuilder, YoutrackService};

const RESOURCE_QUERY_SYNTAX: &str = "youtrack://reference/issues/search/query-syntax";
const RESOURCE_ISSUE_FIELDS: &str = "youtrack://reference/issues/fields";
const RESOURCE_ARTICLE_FIELDS: &str = "youtrack://reference/articles/fields";

#[derive(Clone)]
pub struct YoutrackMCPServer {
    pub(crate) tool_router: ToolRouter<Self>,
    service: std::sync::Arc<YoutrackService>,
}

impl YoutrackMCPServer {
    pub fn new(base_url: &str, token: &str) -> Self {
        let client = YoutrackClientBuilder::new()
            .with_base_url(base_url)
            .with_token(token)
            .build()
            .expect("youtrack client builds");

        let issues_tools = YoutrackMCPServer::youtrack_issues();
        let project_tools = YoutrackMCPServer::youtrack_projects();
        let articles_tools = YoutrackMCPServer::youtrack_articles();
        let commands_tools = YoutrackMCPServer::youtrack_commands();

        Self {
            tool_router: issues_tools + project_tools + articles_tools + commands_tools,
            service: std::sync::Arc::new(YoutrackService::new(client)),
        }
    }

    fn json_result(value: impl serde::Serialize) -> Result<CallToolResult, McpError> {
        let text = serde_json::to_string_pretty(&value)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    }

    fn map_err(e: youtrack_client_wrapper::YoutrackError) -> McpError {
        McpError::internal_error(format!("youtrack: {e}"), None)
    }

    fn map_write_err(e: youtrack_client_wrapper::WriteError) -> McpError {
        McpError::internal_error(format!("youtrack: {e}"), None)
    }
}

impl ServerHandler for YoutrackMCPServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_resources().enable_tools().build())
    }

    /// Диспетчер тулов. `impl ServerHandler` здесь написан руками, поэтому
    /// `#[tool_handler]` его не генерирует, а дефолт трейта отвечает
    /// `-32601 tools/call` — список тулов при этом отдаётся нормально
    /// (шлюз читает `tool_router.list_all()` напрямую), и наружу это
    /// выглядит как «сервер знает тулы, но не умеет их звать».
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        self.tool_router.call(ToolCallContext::new(self, request, context)).await
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult {
            resources: vec![
                Resource::new(RESOURCE_QUERY_SYNTAX, "issues-query-syntax"),
                Resource::new(RESOURCE_ISSUE_FIELDS, "issue-fields"),
                Resource::new(RESOURCE_ARTICLE_FIELDS, "article-fields"),
            ],
            next_cursor: None,
            meta: None,
            ..Default::default()
        })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        match request.uri.as_str() {
            RESOURCE_QUERY_SYNTAX => Ok(ReadResourceResult::new(vec![ResourceContents::text(
                QUERY_SYNTAX,
                &request.uri,
            )])
            .into()),
            RESOURCE_ISSUE_FIELDS => Ok(ReadResourceResult::new(vec![ResourceContents::text(
                ISSUE_FIELDS,
                &request.uri,
            )])
            .into()),
            RESOURCE_ARTICLE_FIELDS => Ok(ReadResourceResult::new(vec![ResourceContents::text(
                ARTICLE_FIELDS,
                &request.uri,
            )])
            .into()),
            _ => Err(McpError::resource_not_found(
                "resource not found",
                Some(json!({ "uri": request.uri })),
            )),
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FindIssueArgs {
    /// Human-readable issue id, e.g. "DEMO-123"
    pub id: String,
    /// Optional field selection (see resource youtrack://reference/fields).
    /// Omit for the sensible default. Nested selection allowed, e.g.
    /// "customFields(name,value(name,login,fullName))". Список не должен
    /// быть пустым — пустой массив вырождается в ответ с одним `$type`.
    #[serde(default)]
    pub fields: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchIssuesArgs {
    /// YouTrack query language string (see resource youtrack://reference/query-syntax)
    pub query: String,
    /// Optional field selection (see resource youtrack://reference/fields).
    /// Список не должен быть пустым — пустой массив вырождается в ответ
    /// с одним `$type`.
    #[serde(default)]
    pub fields: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MyIssuesArgs {
    /// YouTrack login of the assignee
    pub assignee: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ApplyCommandArgs {
    /// Human-readable issue id, e.g. "DEMO-123"
    pub issue: String,
    /// YouTrack command string, e.g. "state Fixed"
    pub command: String,
    /// Optional comment to attach along with the command
    #[serde(default)]
    pub comment: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateIssueArgs {
    /// Project shortName, e.g. "DEMO"
    pub project: String,
    /// Issue summary (title)
    pub summary: String,
    /// Optional issue description
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AddCommentArgs {
    /// Human-readable issue id, e.g. "DEMO-123"
    pub issue: String,
    /// Comment text
    pub text: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FindArticleArgs {
    /// Human-readable article id, e.g. "DEMO-A-4"
    pub id: String,
    /// Optional field selection (see resource youtrack://reference/articles).
    /// Omit for the sensible default (includes the full `content`).
    #[serde(default)]
    pub fields: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchArticlesArgs {
    /// Space-separated words; ALL of them must occur in title or body
    /// (case-insensitive substring match). Empty string lists all articles.
    /// NOT the YouTrack query language — the articles endpoint has none.
    pub query: String,
    /// Optional project shortName to narrow the scan, e.g. "DEMO"
    #[serde(default)]
    pub project: Option<String>,
    /// Page size, default 25. Ask for more only when you really need it.
    #[serde(default)]
    pub limit: Option<usize>,
    /// How many matches to skip — pass the `next_skip` value from the
    /// previous response to get the next page.
    #[serde(default)]
    pub skip: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateArticleArgs {
    /// Project shortName, e.g. "DEMO"
    pub project: String,
    /// Article title
    pub summary: String,
    /// Article body in YouTrack Markdown
    #[serde(default)]
    pub content: Option<String>,
    /// Optional parent article id (idReadable) to nest this article under
    #[serde(default)]
    pub parent: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateArticleArgs {
    /// Human-readable article id, e.g. "DEMO-A-4"
    pub id: String,
    /// New title; omit to keep the current one
    #[serde(default)]
    pub summary: Option<String>,
    /// New body — REPLACES the whole article text, so read it with
    /// youtrack_find_article first and send the full edited version
    #[serde(default)]
    pub content: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AddArticleCommentArgs {
    /// Human-readable article id, e.g. "DEMO-A-4"
    pub article: String,
    /// Comment text
    pub text: String,
}

/// Ожидаемое число youtrack_*-инструментов — один источник правды для тестов
/// этого модуля и шлюза.
#[cfg(test)]
pub(crate) const YOUTRACK_TOOL_COUNT: usize = 12;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn router_lists_issue_and_article_tools() {
        let server = YoutrackMCPServer::new("https://example.com", "t");
        let names: Vec<_> =
            server.tool_router.list_all().into_iter().map(|t| t.name.to_string()).collect();
        for expected in [
            "youtrack_find_issue",
            "youtrack_search_issues",
            "youtrack_my_issues",
            "youtrack_apply_command",
            "youtrack_create_issue",
            "youtrack_add_comment",
            "youtrack_list_projects",
            "youtrack_find_article",
            "youtrack_search_articles",
            "youtrack_create_article",
            "youtrack_update_article",
            "youtrack_add_article_comment",
        ] {
            assert!(names.contains(&expected.to_string()), "missing tool {expected}");
        }
        assert_eq!(names.len(), YOUTRACK_TOOL_COUNT);
    }
}
