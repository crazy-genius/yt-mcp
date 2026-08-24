use crate::mcp::{
    AddArticleCommentArgs, CreateArticleArgs, FindArticleArgs, SearchArticlesArgs,
    UpdateArticleArgs, UserToken, YoutrackMCPServer,
};
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::ContentBlock;
use rmcp::{ErrorData as McpError, model::CallToolResult, tool, tool_router};

#[tool_router(router = "youtrack_articles", vis = "pub")]
impl YoutrackMCPServer {
    #[tool(
        name = "youtrack_find_article",
        description = "Read one Knowledge Base article by its human-readable ID, including the full text (content) and its parent/child articles. Optional 'fields' selects returned fields (see youtrack://reference/articles).",
        annotations(title = "Find article", read_only_hint = true, open_world_hint = true)
    )]
    pub async fn find_article(
        &self,
        params: Parameters<FindArticleArgs>,
        Extension(token): Extension<UserToken>,
    ) -> Result<CallToolResult, McpError> {
        let yt = self.as_user(&token)?;
        match yt.find_article(&params.0.id, params.0.fields).await {
            Ok(article) => Self::json_result(article),
            Err(e) => Ok(Self::tool_error(e)),
        }
    }

    #[tool(
        name = "youtrack_search_articles",
        description = "Search the YouTrack Knowledge Base by keywords (all words must occur in title or body, case-insensitive); empty query lists articles. Optional 'project' narrows to one project. Returns titles WITHOUT the body — call youtrack_find_article for the text. Paginated: the result carries 'matched', 'next_skip' (pass it back as 'skip' for the next page, null when exhausted) and 'scan_truncated' (true = not the whole knowledge base was scanned, narrow the query or set 'project').",
        annotations(
            title = "Search knowledge base",
            read_only_hint = true,
            open_world_hint = true
        )
    )]
    pub async fn search_articles(
        &self,
        params: Parameters<SearchArticlesArgs>,
        Extension(token): Extension<UserToken>,
    ) -> Result<CallToolResult, McpError> {
        let args = params.0;
        let yt = self.as_user(&token)?;
        match yt.search_articles(&args.query, args.project.as_deref(), args.limit, args.skip).await
        {
            Ok(page) => Self::json_result(page),
            Err(e) => Ok(Self::tool_error(e)),
        }
    }

    #[tool(
        name = "youtrack_create_article",
        description = "Creates a Knowledge Base article in the given project (shortName), optionally nested under a parent article",
        annotations(
            title = "Create article",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    pub async fn create_article(
        &self,
        params: Parameters<CreateArticleArgs>,
        Extension(token): Extension<UserToken>,
    ) -> Result<CallToolResult, McpError> {
        let args = params.0;
        let yt = self.as_user(&token)?;
        match yt
            .create_article(
                &args.project,
                &args.summary,
                args.content.as_deref(),
                args.parent.as_deref(),
            )
            .await
        {
            Ok(article) => Self::json_result(article),
            Err(e) => Ok(Self::tool_error(e)),
        }
    }

    #[tool(
        name = "youtrack_update_article",
        description = "Rewrites the title and/or the whole body of a Knowledge Base article — read it with youtrack_find_article first and send the complete edited text",
        annotations(
            title = "Rewrite article",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    pub async fn update_article(
        &self,
        params: Parameters<UpdateArticleArgs>,
        Extension(token): Extension<UserToken>,
    ) -> Result<CallToolResult, McpError> {
        let args = params.0;
        let yt = self.as_user(&token)?;
        match yt.update_article(&args.id, args.summary.as_deref(), args.content.as_deref()).await {
            Ok(article) => Self::json_result(article),
            Err(e) => Ok(Self::tool_error(e)),
        }
    }

    #[tool(
        name = "youtrack_add_article_comment",
        description = "Adds a comment to a Knowledge Base article",
        annotations(
            title = "Comment on article",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    pub async fn add_article_comment(
        &self,
        params: Parameters<AddArticleCommentArgs>,
        Extension(token): Extension<UserToken>,
    ) -> Result<CallToolResult, McpError> {
        let args = params.0;
        let yt = self.as_user(&token)?;
        match yt.add_article_comment(&args.article, &args.text).await {
            Ok(()) => Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "OK: comment added to article {}",
                args.article
            ))])),
            Err(e) => Ok(Self::tool_error(e)),
        }
    }
}
