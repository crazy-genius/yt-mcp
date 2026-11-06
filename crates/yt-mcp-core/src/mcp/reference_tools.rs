use crate::mcp::{ReferenceArgs, ReferenceTopic, YoutrackMCPServer};
use crate::references::{ARTICLE_FIELDS, ISSUE_FIELDS, QUERY_SYNTAX};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::ContentBlock;
use rmcp::{ErrorData as McpError, model::CallToolResult, tool, tool_router};

#[tool_router(router = "youtrack_reference", vis = "pub")]
impl YoutrackMCPServer {
    // Тот же текст, что отдают ресурсы `youtrack://reference/*`: не все MCP-клиенты
    // умеют читать ресурсы, а тулы видят все. Токен проверяет общий `call_tool`;
    // сам справочник вшит в бинарь, в YouTrack не ходит, поэтому `open_world_hint = false`.
    #[tool(
        name = "youtrack_reference",
        description = "Returns a built-in reference: 'query-syntax' (YouTrack search query language), 'issue-fields' / 'article-fields' (the `fields` selection syntax). Same text as resources youtrack://reference/issues/search/query-syntax, youtrack://reference/issues/fields and youtrack://reference/articles/fields.",
        annotations(
            title = "Read YouTrack reference",
            read_only_hint = true,
            open_world_hint = false
        )
    )]
    pub async fn reference(
        &self,
        params: Parameters<ReferenceArgs>,
    ) -> Result<CallToolResult, McpError> {
        let text = match params.0.topic {
            ReferenceTopic::QuerySyntax => QUERY_SYNTAX,
            ReferenceTopic::IssueFields => ISSUE_FIELDS,
            ReferenceTopic::ArticleFields => ARTICLE_FIELDS,
        };
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    }
}
