mod articles_tools;
mod attachments_tools;
mod commands_tools;
mod comments_tools;
mod issues_tools;
mod projects_tools;
mod reference_tools;
mod users_tools;

use std::sync::Arc;

use crate::references::{ARTICLE_FIELDS, ISSUE_FIELDS, QUERY_SYNTAX};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, GetPromptRequestParams, GetPromptResponse,
    Implementation, ListPromptsResult, ListResourcesResult, ListToolsResult,
    PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult,
    Resource, ResourceContents, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler,
    handler::server::{
        prompt::PromptContext,
        router::prompt::PromptRouter,
        tool::{ToolCallContext, ToolRouter},
    },
    model::{CallToolResult, ContentBlock},
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use yt_client::{Youtrack, YoutrackService};

const RESOURCE_QUERY_SYNTAX: &str = "youtrack://reference/issues/search/query-syntax";
const RESOURCE_ISSUE_FIELDS: &str = "youtrack://reference/issues/fields";
const RESOURCE_ARTICLE_FIELDS: &str = "youtrack://reference/articles/fields";

/// Заголовок с токеном пользователя к YouTrack — уровень 2 авторизации.
/// Уровень 1 (`Authorization`) снимается axum-слоем до MCP и сюда не доходит.
pub const USER_TOKEN_HEADER: &str = "x-youtrack-token";

/// Заголовок выбора режима: `X-API-Version: v2` включает OAuth-режим, где токен
/// пользователя приходит в `Authorization`, а не в `X-YouTrack-Token`.
pub const API_VERSION_HEADER: &str = "x-api-version";

/// `true`, если клиент просит режим v2. Значение обрезается и сравнивается
/// без учёта регистра; любой другой заголовок или его отсутствие — это v1.
pub fn api_v2(headers: &http::HeaderMap) -> bool {
    headers
        .get(API_VERSION_HEADER)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("v2"))
}

/// Токен из `Authorization: Bearer <token>`. Схема регистронезависима
/// (RFC 6750), токен обрезается; пустой токен или другая схема — `None`.
pub fn bearer(headers: &http::HeaderMap) -> Option<&str> {
    let value = headers.get(http::header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    let token = token.trim();
    (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty()).then_some(token)
}

const NO_TOKEN_MSG: &str = "no YouTrack token: add the X-YouTrack-Token header to this \
server's entry in your MCP client configuration";

/// Токен пользователя, доехавший из заголовка до тела тула.
///
/// `Debug` написан руками: производный вылил бы секрет в любой отладочный
/// вывод расширений запроса.
#[derive(Clone)]
pub struct UserToken(pub String);

impl std::fmt::Debug for UserToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("UserToken(<redacted>)")
    }
}

/// Достаёт токен уровня 2 из HTTP-заголовков запроса.
///
/// `StreamableHttpService` кладёт `http::request::Parts` в расширения
/// запроса; при других транспортах их просто нет, и это честный `None`.
/// Пустая строка считается отсутствием: заголовок без значения — это
/// незаполненный конфиг, а не пустой токен. Одни пробелы — то же самое:
/// YouTrack на них ответит 401, а пользователю нужно услышать, какого
/// заголовка не хватает.
///
/// По той же причине значение обрезается: пробел, доехавший из конфига
/// вместе со скопированным токеном, ушёл бы в YouTrack внутри
/// `Authorization: Bearer` и вернул 401, который нечем объяснить.
///
/// В режиме v2 (`X-API-Version: v2`) токен берётся из `Authorization: Bearer`,
/// а `X-YouTrack-Token` игнорируется; v1 читает `X-YouTrack-Token`.
pub(crate) fn user_token(extensions: &rmcp::model::Extensions) -> Option<String> {
    let parts = extensions.get::<http::request::Parts>()?;
    if api_v2(&parts.headers) {
        return bearer(&parts.headers).map(str::to_owned);
    }
    parts
        .headers
        .get(USER_TOKEN_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
}

#[derive(Clone)]
pub struct YoutrackMCPServer {
    pub(crate) tool_router: ToolRouter<Self>,
    pub(crate) prompt_router: PromptRouter<Self>,
    yt: Arc<Youtrack>,
}

impl YoutrackMCPServer {
    /// Конфигурация разбирается в `main`, сюда приезжает готовый `Youtrack`.
    /// Ронять здесь нечего, поэтому конструктор инфаллибелен.
    pub fn new(yt: Arc<Youtrack>) -> Self {
        let issues_tools = YoutrackMCPServer::youtrack_issues();
        let project_tools = YoutrackMCPServer::youtrack_projects();
        let articles_tools = YoutrackMCPServer::youtrack_articles();
        let commands_tools = YoutrackMCPServer::youtrack_commands();
        let comments_tools = YoutrackMCPServer::youtrack_comments();
        let users_tools = YoutrackMCPServer::youtrack_users();
        let attachments_tools = YoutrackMCPServer::youtrack_attachments();
        let reference_tools = YoutrackMCPServer::youtrack_reference();

        Self {
            tool_router: issues_tools
                + project_tools
                + articles_tools
                + commands_tools
                + comments_tools
                + users_tools
                + attachments_tools
                + reference_tools,
            prompt_router: YoutrackMCPServer::youtrack_prompts(),
            yt,
        }
    }

    /// Успешный результат тула: компактный JSON в `content[0]` и тот же
    /// объект в `structuredContent`.
    ///
    /// Спека MCP требует, чтобы `structuredContent` был объектом, поэтому не-объект
    /// (массив) оборачивается в `{"items": ...}`. `outputSchema` не объявляем:
    /// модели `yt-rs` не реализуют `JsonSchema`, схему пришлось бы писать вручную.
    fn json_result(value: impl serde::Serialize) -> Result<CallToolResult, McpError> {
        let value = serde_json::to_value(&value)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let object = match value {
            serde_json::Value::Object(_) => value,
            other => serde_json::json!({ "items": other }),
        };
        Ok(CallToolResult::structured(object))
    }

    /// Ошибка исполнения тула: её видит модель и может исправиться —
    /// сменить проект, поправить запрос, попросить у пользователя доступ.
    /// Протокольные ошибки остаются для того, что чиним только мы.
    fn tool_error(e: impl std::fmt::Display) -> CallToolResult {
        CallToolResult::error(vec![ContentBlock::text(format!("youtrack: {e}"))])
    }

    /// Сервис от имени пользователя текущего вызова.
    ///
    /// Сбой здесь означает битый `YOUTRACK_URL`, но он проверен при старте
    /// в `Youtrack::new`, поэтому ветка недостижима — и раз она наша, а не
    /// пользовательская, это протокольная ошибка.
    fn as_user(&self, token: &UserToken) -> Result<YoutrackService, McpError> {
        self.yt
            .as_user(&token.0)
            .map_err(|e| McpError::internal_error(format!("youtrack client: {e}"), None))
    }
}

/// Указания модели по работе с сервером целиком: они живут здесь, а не в
/// описаниях тулов, которые только констатируют, что тул делает и возвращает.
pub const INSTRUCTIONS: &str = r#"YouTrack issues, Knowledge Base articles, comments and attachments, acting as the user whose token is sent.
- Issue ids are human-readable ("DEMO-123"); article ids look like "DEMO-A-4"; project arguments take the shortName ("DEMO", see youtrack_list_projects).
- When the user means themselves ("my issues", "assign to me", "mention me") and gives no login, youtrack_whoami returns it.
- youtrack_update_issue and youtrack_update_article replace the whole description/body: read the current text first and send the full edited version.
- Summary and description change through youtrack_update_issue; state, assignee, tags and other fields change through youtrack_apply_command.
- Comment and attachment ids come from the matching youtrack_list_* tool.
- Issue, article and user search results are paginated: pass next_skip back as skip to get the next page.
- Query language and `fields` syntax: youtrack_reference.
- Attachments that are not text or images cannot be read; the user can open them in YouTrack."#;

impl ServerHandler for YoutrackMCPServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_resources()
                .enable_tools()
                .enable_prompts()
                .build(),
        )
        .with_server_info(Implementation::new("yt-mcp", env!("CARGO_PKG_VERSION")))
        .with_instructions(INSTRUCTIONS)
    }

    /// Список тулов. Как и `call_tool`, пишется руками: `#[tool_handler]`
    /// не применён, а дефолт трейта отдаёт пустой список — по протоколу это
    /// «тулов нет», хотя роутер полон.
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(self.tool_router.list_all()))
    }

    /// Диспетчер тулов и единственная точка проверки токена уровня 2.
    ///
    /// `impl ServerHandler` здесь написан руками, поэтому `#[tool_handler]`
    /// его не генерирует, а дефолт трейта отвечает `-32601 tools/call` —
    /// список тулов при этом отдаётся нормально, и наружу это выглядит как
    /// «сервер знает тулы, но не умеет их звать». Проверка стоит здесь, а не
    /// в двадцати восьми телах: `initialize`, `tools/list` и `resources/*` обязаны
    /// работать без токена, иначе клиент не сможет сказать пользователю,
    /// чего не хватает.
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        mut context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let Some(token) = user_token(&context.extensions) else {
            return Ok(CallToolResult::error(vec![ContentBlock::text(NO_TOKEN_MSG)]).into());
        };
        context.extensions.insert(UserToken(token));

        self.tool_router.call(ToolCallContext::new(self, request, context)).await
    }

    /// Список промптов. Пишется руками по той же причине, что и `list_tools`:
    /// `#[prompt_handler]` не применён, а дефолт трейта отдаёт пустой список.
    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        Ok(ListPromptsResult::with_all_items(self.prompt_router.list_all()))
    }

    /// Рендер промпта. Токен не проверяется: шаблон YouTrack не трогает.
    /// Отсутствующий обязательный аргумент роутер превращает в `invalid_params`.
    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, McpError> {
        let ctx = PromptContext::new(self, request.name, request.arguments, context);
        self.prompt_router.get_prompt(ctx).await
    }

    /// Схема тула по имени — третий метод, который сгенерировал бы
    /// `#[tool_handler]`. Транспорт streamable-http сверяет по ней
    /// заголовки `Mcp-Param-*` (SEP-2243); дефолт трейта отдаёт `None`, и
    /// проверка молча пропускается — то же семейство, что пустой
    /// `tools/list` из-за неперекрытого `list_tools`.
    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tool_router.get(name).cloned()
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
    // Список не должен быть пустым — пустой массив вырождается в ответ с одним `$type`.
    /// Optional field selection (see youtrack_reference topic 'issue-fields' (resource
    /// youtrack://reference/issues/fields)). Omit for the sensible default. Nested
    /// selection allowed, e.g. "customFields(name,value(name,login,fullName))".
    /// Must not be empty — an empty list returns only `$type`.
    #[serde(default)]
    pub fields: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchIssuesArgs {
    /// YouTrack query language string (see youtrack_reference topic 'query-syntax' (resource
    /// youtrack://reference/issues/search/query-syntax))
    pub query: String,
    // Список не должен быть пустым — пустой массив вырождается в ответ с одним `$type`.
    /// Optional field selection (see youtrack_reference topic 'issue-fields' (resource
    /// youtrack://reference/issues/fields)). Must not be empty — an empty list returns
    /// only `$type`.
    #[serde(default)]
    pub fields: Option<Vec<String>>,
    /// Page size, default 50, capped at 100
    #[serde(default)]
    pub top: Option<i64>,
    /// Offset; pass next_skip from the previous page
    #[serde(default)]
    pub skip: Option<i64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MyIssuesArgs {
    /// YouTrack login of the assignee
    pub assignee: String,
    /// Page size, default 50, capped at 100
    #[serde(default)]
    pub top: Option<i64>,
    /// Offset; pass next_skip from the previous page
    #[serde(default)]
    pub skip: Option<i64>,
}

// Тема справочника для `youtrack_reference`; имена в kebab-case — это то,
// что видит модель в enum схемы.
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ReferenceTopic {
    QuerySyntax,
    IssueFields,
    ArticleFields,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReferenceArgs {
    /// Which reference to read
    pub topic: ReferenceTopic,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ApplyCommandArgs {
    /// Human-readable issue id, e.g. "DEMO-123"
    pub issue: String,
    /// YouTrack command string in the YouTrack command language
    /// (https://www.jetbrains.com/help/youtrack/server/commands.html), e.g. "state Fixed"
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
pub struct UpdateIssueArgs {
    /// Human-readable issue id, e.g. "DEMO-123"
    pub id: String,
    /// New summary (title); omit to keep the current one
    #[serde(default)]
    pub summary: Option<String>,
    /// New description — REPLACES the whole text, so send the full edited version;
    /// omit to keep it
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
    /// Optional field selection (see youtrack_reference topic 'article-fields' (resource
    /// youtrack://reference/articles/fields)).
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
    /// New body — REPLACES the whole article text, so send the full edited version;
    /// omit to keep it
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

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListCommentsArgs {
    /// Human-readable issue id, e.g. "DEMO-123"
    pub issue: String,
    /// Page size, default 50, capped at 200
    #[serde(default)]
    pub top: Option<i64>,
    /// How many comments to skip, default 0
    #[serde(default)]
    pub skip: Option<i64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateCommentArgs {
    /// Human-readable issue id, e.g. "DEMO-123"
    pub issue: String,
    /// Internal comment id, e.g. "4-123" — comments have no human-readable id,
    /// get it from youtrack_list_comments
    pub comment_id: String,
    /// New text — REPLACES the whole comment
    pub text: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DeleteCommentArgs {
    /// Human-readable issue id, e.g. "DEMO-123"
    pub issue: String,
    /// Internal comment id from youtrack_list_comments, e.g. "4-123"
    pub comment_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListArticleCommentsArgs {
    /// Human-readable article id, e.g. "DEMO-A-4"
    pub article: String,
    /// Page size, default 50, capped at 200
    #[serde(default)]
    pub top: Option<i64>,
    /// How many comments to skip, default 0
    #[serde(default)]
    pub skip: Option<i64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateArticleCommentArgs {
    /// Human-readable article id, e.g. "DEMO-A-4"
    pub article: String,
    /// Internal comment id from youtrack_list_article_comments, e.g. "5-123"
    pub comment_id: String,
    /// New text — REPLACES the whole comment
    pub text: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DeleteArticleCommentArgs {
    /// Human-readable article id, e.g. "DEMO-A-4"
    pub article: String,
    /// Internal comment id from youtrack_list_article_comments, e.g. "5-123"
    pub comment_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchUsersArgs {
    /// Space-separated words; ALL of them must occur in the login, full name
    /// or email (case-insensitive substring match). Empty string lists users.
    /// NOT the YouTrack query language — the users endpoint has none.
    pub query: String,
    /// Page size, default 25
    #[serde(default)]
    pub limit: Option<usize>,
    /// How many matches to skip — pass the `next_skip` value from the
    /// previous response to get the next page.
    #[serde(default)]
    pub skip: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListAttachmentsArgs {
    /// Human-readable issue id, e.g. "DEMO-123"
    pub issue: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UploadAttachmentArgs {
    /// Human-readable issue id, e.g. "DEMO-123"
    pub issue: String,
    /// File name as it should appear in YouTrack, e.g. "report.csv"
    pub file_name: String,
    /// File content, base64. A `data:<mime>;base64,` prefix is accepted.
    /// Use this for content you produced yourself. Exactly one of
    /// 'content_base64' and 'url' must be given.
    #[serde(default)]
    pub content_base64: Option<String>,
    /// https URL the server will download the file from — cheaper than
    /// base64 for anything sizeable, because the bytes never pass through
    /// the conversation. Exactly one of 'content_base64' and 'url' must be
    /// given. Plain http is refused.
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadAttachmentArgs {
    /// Human-readable issue id, e.g. "DEMO-123"
    pub issue: String,
    /// Attachment id from youtrack_list_attachments, e.g. "8-1"
    pub attachment_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DeleteAttachmentArgs {
    /// Human-readable issue id, e.g. "DEMO-123"
    pub issue: String,
    /// Attachment id from youtrack_list_attachments, e.g. "8-1"
    pub attachment_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListArticleAttachmentsArgs {
    /// Human-readable article id, e.g. "DEMO-A-4"
    pub article: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UploadArticleAttachmentArgs {
    /// Human-readable article id, e.g. "DEMO-A-4"
    pub article: String,
    /// File name as it should appear in YouTrack, e.g. "scheme.png"
    pub file_name: String,
    /// File content, base64. A `data:<mime>;base64,` prefix is accepted.
    /// Exactly one of 'content_base64' and 'url' must be given.
    #[serde(default)]
    pub content_base64: Option<String>,
    /// https URL the server will download the file from. Exactly one of
    /// 'content_base64' and 'url' must be given. Plain http is refused.
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadArticleAttachmentArgs {
    /// Human-readable article id, e.g. "DEMO-A-4"
    pub article: String,
    /// Attachment id from youtrack_list_article_attachments, e.g. "9-1"
    pub attachment_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DeleteArticleAttachmentArgs {
    /// Human-readable article id, e.g. "DEMO-A-4"
    pub article: String,
    /// Attachment id from youtrack_list_article_attachments, e.g. "9-1"
    pub attachment_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server() -> YoutrackMCPServer {
        YoutrackMCPServer::new(Arc::new(Youtrack::new("https://example.com").expect("builds")))
    }

    #[test]
    fn server_info_names_yt_mcp_and_carries_instructions() {
        let info = server().get_info();
        assert_eq!(info.server_info.name, "yt-mcp");
        assert!(info.instructions.as_deref().unwrap().contains("youtrack_whoami"));
    }

    #[test]
    fn tool_texts_describe_instead_of_steering() {
        const STEERING: [&str; 5] =
            ["call this", "if the user", "ask the user", " first", "you must"];
        for tool in server().tool_router.list_all() {
            let text = format!(
                "{} {}",
                tool.description.as_deref().unwrap_or(""),
                serde_json::to_string(&tool.input_schema).unwrap()
            )
            .to_lowercase();
            for s in STEERING {
                assert!(!text.contains(s), "{}: {s:?}", tool.name);
            }
        }
    }

    #[test]
    fn json_results_are_compact_and_structured() {
        let r = YoutrackMCPServer::json_result(serde_json::json!({"a": [1, 2]})).unwrap();
        assert_eq!(r.structured_content, Some(serde_json::json!({"a": [1, 2]})));
        let text = r.content[0].as_text().unwrap().text.clone();
        assert_eq!(text, r#"{"a":[1,2]}"#);
    }

    #[test]
    fn an_array_result_is_wrapped_in_an_object() {
        let r = YoutrackMCPServer::json_result(vec![1, 2]).unwrap();
        assert_eq!(r.structured_content, Some(serde_json::json!({"items": [1, 2]})));
    }

    #[test]
    fn router_lists_issue_and_article_tools() {
        let names: Vec<_> =
            server().tool_router.list_all().into_iter().map(|t| t.name.to_string()).collect();
        let expected = [
            "youtrack_find_issue",
            "youtrack_search_issues",
            "youtrack_my_issues",
            "youtrack_apply_command",
            "youtrack_create_issue",
            "youtrack_update_issue",
            "youtrack_list_projects",
            "youtrack_find_article",
            "youtrack_search_articles",
            "youtrack_create_article",
            "youtrack_update_article",
            "youtrack_add_comment",
            "youtrack_list_comments",
            "youtrack_update_comment",
            "youtrack_delete_comment",
            "youtrack_add_article_comment",
            "youtrack_list_article_comments",
            "youtrack_update_article_comment",
            "youtrack_delete_article_comment",
            "youtrack_whoami",
            "youtrack_search_users",
            "youtrack_list_attachments",
            "youtrack_upload_attachment",
            "youtrack_read_attachment",
            "youtrack_delete_attachment",
            "youtrack_list_article_attachments",
            "youtrack_upload_article_attachment",
            "youtrack_read_article_attachment",
            "youtrack_delete_article_attachment",
            "youtrack_reference",
        ];
        for name in expected {
            assert!(names.contains(&name.to_string()), "missing tool {name}");
        }
        assert_eq!(names.len(), expected.len(), "лишние тулы в роутере");
    }

    /// `Debug` у `UserToken` написан руками именно затем, чтобы токен не
    /// вытек в лог. Производный `Debug` вернул бы всё на место молча, и
    /// защищал бы это только комментарий — поэтому инвариант исполняемый.
    #[test]
    fn user_token_debug_redacts_the_secret() {
        let printed = format!("{:?}", UserToken("perm:super-secret".to_owned()));
        assert!(printed.contains("<redacted>"), "нет заглушки: {printed}");
        assert!(!printed.contains("super-secret"), "токен вытек в Debug: {printed}");
    }

    #[test]
    fn user_token_reads_the_header_and_ignores_an_empty_one() {
        let with_header = |name: &str, value: &str| {
            let (parts, ()) = http::Request::builder()
                .header(name, value)
                .body(())
                .expect("request builds")
                .into_parts();
            let mut ext = rmcp::model::Extensions::new();
            ext.insert(parts);
            user_token(&ext)
        };
        let with = |value: &str| with_header(USER_TOKEN_HEADER, value);

        assert_eq!(with("secret"), Some("secret".to_owned()));
        assert_eq!(with(""), None, "пустой заголовок — это незаполненный конфиг");
        assert_eq!(with("   "), None, "одни пробелы — тот же незаполненный конфиг");
        assert_eq!(
            with(" secret "),
            Some("secret".to_owned()),
            "пробел из конфига уехал бы в Bearer и вернул 401"
        );
        assert_eq!(user_token(&rmcp::model::Extensions::new()), None, "заголовков нет вовсе");
        assert_eq!(
            with_header("X-YouTrack-Token", "secret"),
            Some("secret".to_owned()),
            "HTTP-заголовки регистронезависимы"
        );
    }

    fn headers(pairs: &[(&str, &str)]) -> http::HeaderMap {
        let mut map = http::HeaderMap::new();
        for (name, value) in pairs {
            map.append(
                http::header::HeaderName::from_bytes(name.as_bytes()).expect("name"),
                http::HeaderValue::from_str(value).expect("value"),
            );
        }
        map
    }

    fn ext(pairs: &[(&str, &str)]) -> rmcp::model::Extensions {
        let mut request = http::Request::builder();
        for (name, value) in pairs {
            request = request.header(*name, *value);
        }
        let (parts, ()) = request.body(()).expect("request builds").into_parts();
        let mut ext = rmcp::model::Extensions::new();
        ext.insert(parts);
        ext
    }

    #[test]
    fn api_v2_is_case_and_whitespace_insensitive() {
        assert!(api_v2(&headers(&[("X-API-Version", "v2")])));
        assert!(api_v2(&headers(&[("X-API-Version", " V2 ")])));
        assert!(!api_v2(&headers(&[("X-API-Version", "v1")])));
        assert!(!api_v2(&headers(&[])));
    }

    #[test]
    fn bearer_accepts_any_scheme_case_and_refuses_an_empty_token() {
        assert_eq!(bearer(&headers(&[("Authorization", "Bearer abc")])), Some("abc"));
        assert_eq!(bearer(&headers(&[("Authorization", "bearer abc")])), Some("abc"));
        assert_eq!(bearer(&headers(&[("Authorization", "Bearer   ")])), None);
        assert_eq!(bearer(&headers(&[("Authorization", "Basic abc")])), None);
    }

    #[test]
    fn in_v2_the_user_token_comes_from_authorization_not_x_youtrack_token() {
        let v2 = ext(&[
            ("X-API-Version", "v2"),
            ("Authorization", "Bearer hub-token"),
            ("X-YouTrack-Token", "perm-token"),
        ]);
        assert_eq!(user_token(&v2).as_deref(), Some("hub-token"));
        let v1 = ext(&[("Authorization", "Bearer mcp-secret"), ("X-YouTrack-Token", "perm-token")]);
        assert_eq!(user_token(&v1).as_deref(), Some("perm-token"));
    }

    /// Политика вызова живёт в аннотациях, а не в тексте описания. Таблица
    /// читается как спецификация этой политики.
    ///
    /// Предыдущая версия теста требовала `idempotent_hint == Some(false)` у
    /// всех пишущих тулов — «повторный вызов заводит второй комментарий». Для
    /// `update_*` и `delete_*` это неверно: повторный вызов с теми же
    /// аргументами оставляет то же состояние, и врать об этом модели нельзя —
    /// именно на этот хинт она опирается, решая, безопасен ли повтор после
    /// сетевого сбоя.
    #[test]
    fn annotations_match_the_call_policy_table() {
        // (имя, read_only, destructive, idempotent)
        const POLICY: [(&str, bool, bool, bool); 30] = [
            ("youtrack_find_issue", true, false, false),
            ("youtrack_search_issues", true, false, false),
            ("youtrack_my_issues", true, false, false),
            ("youtrack_list_projects", true, false, false),
            ("youtrack_find_article", true, false, false),
            ("youtrack_search_articles", true, false, false),
            ("youtrack_list_comments", true, false, false),
            ("youtrack_list_article_comments", true, false, false),
            ("youtrack_apply_command", false, true, false),
            ("youtrack_update_article", false, true, true),
            ("youtrack_create_issue", false, false, false),
            ("youtrack_update_issue", false, true, true),
            ("youtrack_create_article", false, false, false),
            ("youtrack_add_comment", false, false, false),
            ("youtrack_add_article_comment", false, false, false),
            ("youtrack_update_comment", false, true, true),
            ("youtrack_delete_comment", false, true, true),
            ("youtrack_update_article_comment", false, true, true),
            ("youtrack_delete_article_comment", false, true, true),
            ("youtrack_whoami", true, false, false),
            ("youtrack_search_users", true, false, false),
            ("youtrack_list_attachments", true, false, false),
            ("youtrack_read_attachment", true, false, false),
            ("youtrack_list_article_attachments", true, false, false),
            ("youtrack_read_article_attachment", true, false, false),
            ("youtrack_upload_attachment", false, false, false),
            ("youtrack_upload_article_attachment", false, false, false),
            ("youtrack_delete_attachment", false, true, true),
            ("youtrack_delete_article_attachment", false, true, true),
            ("youtrack_reference", true, false, false),
        ];

        let tools = server().tool_router.list_all();
        assert_eq!(tools.len(), POLICY.len(), "таблица политики разошлась с роутером");

        for tool in tools {
            let name = tool.name.to_string();
            let ann = tool.annotations.as_ref().unwrap_or_else(|| panic!("{name}: нет аннотаций"));
            let (_, read_only, destructive, idempotent) = POLICY
                .iter()
                .find(|(n, ..)| *n == name)
                .unwrap_or_else(|| panic!("{name}: тула нет в таблице политики"));

            assert_eq!(ann.read_only_hint, Some(*read_only), "{name}: read_only_hint");
            if !*read_only {
                assert_eq!(ann.destructive_hint, Some(*destructive), "{name}: destructive_hint");
                assert_eq!(ann.idempotent_hint, Some(*idempotent), "{name}: idempotent_hint");
            }
            // Справочник вшит в бинарь и никуда не ходит.
            let open_world = name != "youtrack_reference";
            assert_eq!(ann.open_world_hint, Some(open_world), "{name}: open_world_hint");
            assert!(ann.title.is_some(), "{name}: нет человекочитаемого title");
        }
    }

    /// Префикс DESTRUCTIVE: в описании был политикой, набранной текстом,
    /// и не отличал перезапись от добавления. Его место — в аннотации.
    ///
    /// `unwrap_or_default()` на месте `description` замаскировал бы пропавшее
    /// описание пустой строкой, в которой подстроки заведомо нет — тест
    /// обязан упасть, если описание исчезло, а не проходить по умолчанию.
    #[test]
    fn descriptions_no_longer_carry_the_destructive_prefix() {
        for tool in server().tool_router.list_all() {
            let description = tool
                .description
                .as_deref()
                .unwrap_or_else(|| panic!("{}: у тула пропало описание", tool.name));
            assert!(
                !description.contains("DESTRUCTIVE"),
                "{}: политика осталась в описании",
                tool.name
            );
        }
    }

    /// Описание тула и схема его аргументов — единственное, откуда модель
    /// узнаёт адрес справочника. Адрес, которого нет в `list_resources`,
    /// она запросит и получит «resource not found», поэтому каждая
    /// подстрока `youtrack://` обязана быть одной из трёх констант.
    #[test]
    fn tools_point_only_at_resources_that_exist() {
        let known = [RESOURCE_QUERY_SYNTAX, RESOURCE_ISSUE_FIELDS, RESOURCE_ARTICLE_FIELDS];
        for tool in server().tool_router.list_all() {
            let text = format!(
                "{} {}",
                tool.description.as_deref().unwrap_or_default(),
                serde_json::Value::Object((*tool.input_schema).clone())
            );
            for (at, _) in text.match_indices("youtrack://") {
                let uri: String = text[at..]
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || "/:-_".contains(*c))
                    .collect();
                assert!(known.contains(&uri.as_str()), "{}: нет ресурса {uri}", tool.name);
            }
        }
    }

    #[test]
    fn tool_schemas_carry_no_cyrillic() {
        for tool in server().tool_router.list_all() {
            let text = format!(
                "{} {}",
                tool.description.as_deref().unwrap_or(""),
                serde_json::to_string(&tool.input_schema).unwrap()
            );
            assert!(
                !text.chars().any(|c| ('\u{0400}'..='\u{04FF}').contains(&c)),
                "{}: кириллица в схеме",
                tool.name
            );
        }
    }

    #[tokio::test]
    async fn the_reference_tool_returns_the_resource_text() {
        use rmcp::handler::server::wrapper::Parameters;
        for (topic, text) in [
            (ReferenceTopic::QuerySyntax, QUERY_SYNTAX),
            (ReferenceTopic::IssueFields, ISSUE_FIELDS),
            (ReferenceTopic::ArticleFields, ARTICLE_FIELDS),
        ] {
            let r = server().reference(Parameters(ReferenceArgs { topic })).await.unwrap();
            assert_ne!(r.is_error, Some(true));
            assert_eq!(r.content[0].as_text().unwrap().text, text);
        }
    }

    /// Неизвестная тема из ручного JSON — ошибка десериализации (rmcp отдаст
    /// её как `invalid_params`), а не паника.
    #[test]
    fn an_unknown_reference_topic_is_a_deserialization_error() {
        assert!(serde_json::from_value::<ReferenceArgs>(json!({"topic": "nope"})).is_err());
        let ok: ReferenceArgs = serde_json::from_value(json!({"topic": "query-syntax"})).unwrap();
        assert!(matches!(ok.topic, ReferenceTopic::QuerySyntax));
    }

    #[test]
    fn apply_command_names_the_command_language_docs() {
        let t = server().get_tool("youtrack_apply_command").unwrap();
        assert!(t.description.unwrap().contains("jetbrains.com/help/youtrack/server/commands"));
    }
}
