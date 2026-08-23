use crate::mcp::{
    AddArticleCommentArgs, CreateArticleArgs, FindArticleArgs, SearchArticlesArgs,
    UpdateArticleArgs, YoutrackMCPServer,
};
use rmcp::model::ContentBlock;
use rmcp::{ErrorData as McpError, model::CallToolResult, tool, tool_router};
use youtrack_client_wrapper::Article;

#[tool_router(router = "youtrack_articles", vis = "pub")]
impl YoutrackMCPServer {
    #[tool(
        name = "youtrack_find_article",
        description = "Read one Knowledge Base article by its human-readable ID, including the full text (content) and its parent/child articles. Optional 'fields' selects returned fields (see youtrack://reference/articles)."
    )]
    pub async fn find_article(
        &self,
        params: rmcp::handler::server::wrapper::Parameters<FindArticleArgs>,
    ) -> Result<CallToolResult, McpError> {
        let article: Article = self
            .service
            .find_article(&params.0.id, params.0.fields)
            .await
            .map_err(Self::map_err)?;
        Self::json_result(article)
    }

    #[tool(
        name = "youtrack_search_articles",
        description = "Search the YouTrack Knowledge Base by keywords (all words must occur in title or body, case-insensitive); empty query lists articles. Optional 'project' narrows to one project. Returns titles WITHOUT the body — call youtrack_find_article for the text. Paginated: the result carries 'matched', 'next_skip' (pass it back as 'skip' for the next page, null when exhausted) and 'scan_truncated' (true = not the whole knowledge base was scanned, narrow the query or set 'project')."
    )]
    pub async fn search_articles(
        &self,
        params: rmcp::handler::server::wrapper::Parameters<SearchArticlesArgs>,
    ) -> Result<CallToolResult, McpError> {
        let args = params.0;
        let page = self
            .service
            .search_articles(&args.query, args.project.as_deref(), args.limit, args.skip)
            .await
            .map_err(Self::map_write_err)?;
        Self::json_result(page)
    }

    #[tool(
        name = "youtrack_create_article",
        description = "DESTRUCTIVE: requires explicit user confirmation in the dialog before calling. Creates a Knowledge Base article in the given project (shortName), optionally nested under a parent article"
    )]
    pub async fn create_article(
        &self,
        params: rmcp::handler::server::wrapper::Parameters<CreateArticleArgs>,
    ) -> Result<CallToolResult, McpError> {
        let args = params.0;
        let article: Article = self
            .service
            .create_article(
                &args.project,
                &args.summary,
                args.content.as_deref(),
                args.parent.as_deref(),
            )
            .await
            .map_err(Self::map_write_err)?;
        Self::json_result(article)
    }

    #[tool(
        name = "youtrack_update_article",
        description = "DESTRUCTIVE: requires explicit user confirmation in the dialog before calling. Rewrites the title and/or the whole body of a Knowledge Base article — read it with youtrack_find_article first and send the complete edited text"
    )]
    pub async fn update_article(
        &self,
        params: rmcp::handler::server::wrapper::Parameters<UpdateArticleArgs>,
    ) -> Result<CallToolResult, McpError> {
        let args = params.0;
        let article: Article = self
            .service
            .update_article(&args.id, args.summary.as_deref(), args.content.as_deref())
            .await
            .map_err(Self::map_write_err)?;
        Self::json_result(article)
    }

    #[tool(
        name = "youtrack_add_article_comment",
        description = "DESTRUCTIVE: requires explicit user confirmation in the dialog before calling. Adds a comment to a Knowledge Base article"
    )]
    pub async fn add_article_comment(
        &self,
        params: rmcp::handler::server::wrapper::Parameters<AddArticleCommentArgs>,
    ) -> Result<CallToolResult, McpError> {
        let args = params.0;
        self.service
            .add_article_comment(&args.article, &args.text)
            .await
            .map_err(Self::map_write_err)?;
        Ok(CallToolResult::success(vec![ContentBlock::text(format!(
            "OK: comment added to article {}",
            args.article
        ))]))
    }
}
