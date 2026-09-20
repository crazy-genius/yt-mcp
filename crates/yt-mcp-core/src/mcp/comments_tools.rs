use crate::mcp::{
    AddArticleCommentArgs, AddCommentArgs, DeleteArticleCommentArgs, DeleteCommentArgs,
    ListArticleCommentsArgs, ListCommentsArgs, UpdateArticleCommentArgs, UpdateCommentArgs,
    UserToken, YoutrackMCPServer,
};
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::ContentBlock;
use rmcp::{ErrorData as McpError, model::CallToolResult, tool, tool_router};

#[tool_router(router = "youtrack_comments", vis = "pub")]
impl YoutrackMCPServer {
    #[tool(
        name = "youtrack_add_comment",
        description = "Adds a comment to the issue",
        annotations(
            title = "Comment on issue",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    pub async fn add_comment(
        &self,
        params: Parameters<AddCommentArgs>,
        Extension(token): Extension<UserToken>,
    ) -> Result<CallToolResult, McpError> {
        let args = params.0;
        let yt = self.as_user(&token)?;
        match yt.add_comment(&args.issue, &args.text).await {
            Ok(()) => Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "OK: comment added to {}",
                args.issue
            ))])),
            Err(e) => Ok(Self::tool_error(e)),
        }
    }

    #[tool(
        name = "youtrack_list_comments",
        description = "Read the comment thread of an issue: author, text, timestamps and the internal comment id needed by youtrack_update_comment and youtrack_delete_comment. Paginated with 'top' (default 50, capped at 200) and 'skip'.",
        annotations(title = "Read issue comments", read_only_hint = true, open_world_hint = true)
    )]
    pub async fn list_comments(
        &self,
        params: Parameters<ListCommentsArgs>,
        Extension(token): Extension<UserToken>,
    ) -> Result<CallToolResult, McpError> {
        let args = params.0;
        let yt = self.as_user(&token)?;
        match yt.list_comments(&args.issue, args.top, args.skip).await {
            Ok(comments) => Self::json_result(comments),
            Err(e) => Ok(Self::tool_error(e)),
        }
    }

    #[tool(
        name = "youtrack_update_comment",
        description = "Rewrites the text of one issue comment — send the complete new text. Get 'comment_id' from youtrack_list_comments. YouTrack only allows editing your own comments.",
        annotations(
            title = "Rewrite issue comment",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    pub async fn update_comment(
        &self,
        params: Parameters<UpdateCommentArgs>,
        Extension(token): Extension<UserToken>,
    ) -> Result<CallToolResult, McpError> {
        let args = params.0;
        let yt = self.as_user(&token)?;
        match yt.update_comment(&args.issue, &args.comment_id, &args.text).await {
            Ok(comment) => Self::json_result(comment),
            Err(e) => Ok(Self::tool_error(e)),
        }
    }

    #[tool(
        name = "youtrack_delete_comment",
        description = "Deletes one issue comment. Get 'comment_id' from youtrack_list_comments. YouTrack only allows deleting your own comments.",
        annotations(
            title = "Delete issue comment",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    pub async fn delete_comment(
        &self,
        params: Parameters<DeleteCommentArgs>,
        Extension(token): Extension<UserToken>,
    ) -> Result<CallToolResult, McpError> {
        let args = params.0;
        let yt = self.as_user(&token)?;
        match yt.delete_comment(&args.issue, &args.comment_id).await {
            Ok(()) => Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "OK: comment {} deleted from {}",
                args.comment_id, args.issue
            ))])),
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

    #[tool(
        name = "youtrack_list_article_comments",
        description = "Read the comment thread of a Knowledge Base article: author, text, timestamps and the internal comment id needed by youtrack_update_article_comment and youtrack_delete_article_comment. Paginated with 'top' (default 50, capped at 200) and 'skip'.",
        annotations(
            title = "Read article comments",
            read_only_hint = true,
            open_world_hint = true
        )
    )]
    pub async fn list_article_comments(
        &self,
        params: Parameters<ListArticleCommentsArgs>,
        Extension(token): Extension<UserToken>,
    ) -> Result<CallToolResult, McpError> {
        let args = params.0;
        let yt = self.as_user(&token)?;
        match yt.list_article_comments(&args.article, args.top, args.skip).await {
            Ok(comments) => Self::json_result(comments),
            Err(e) => Ok(Self::tool_error(e)),
        }
    }

    #[tool(
        name = "youtrack_update_article_comment",
        description = "Rewrites the text of one article comment — send the complete new text. Get 'comment_id' from youtrack_list_article_comments. YouTrack only allows editing your own comments.",
        annotations(
            title = "Rewrite article comment",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    pub async fn update_article_comment(
        &self,
        params: Parameters<UpdateArticleCommentArgs>,
        Extension(token): Extension<UserToken>,
    ) -> Result<CallToolResult, McpError> {
        let args = params.0;
        let yt = self.as_user(&token)?;
        match yt.update_article_comment(&args.article, &args.comment_id, &args.text).await {
            Ok(comment) => Self::json_result(comment),
            Err(e) => Ok(Self::tool_error(e)),
        }
    }

    #[tool(
        name = "youtrack_delete_article_comment",
        description = "Deletes one article comment. Get 'comment_id' from youtrack_list_article_comments. YouTrack only allows deleting your own comments.",
        annotations(
            title = "Delete article comment",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    pub async fn delete_article_comment(
        &self,
        params: Parameters<DeleteArticleCommentArgs>,
        Extension(token): Extension<UserToken>,
    ) -> Result<CallToolResult, McpError> {
        let args = params.0;
        let yt = self.as_user(&token)?;
        match yt.delete_article_comment(&args.article, &args.comment_id).await {
            Ok(()) => Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "OK: comment {} deleted from article {}",
                args.comment_id, args.article
            ))])),
            Err(e) => Ok(Self::tool_error(e)),
        }
    }
}
