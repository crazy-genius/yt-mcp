mod articles_tools;
mod attachments_tools;
mod commands_tools;
mod comments_tools;
mod issues_tools;
mod projects_tools;
mod users_tools;

use std::sync::Arc;

use crate::references::{ARTICLE_FIELDS, ISSUE_FIELDS, QUERY_SYNTAX};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, ListResourcesResult, ListToolsResult,
    PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult,
    Resource, ResourceContents, ServerCapabilities, ServerInfo, Tool,
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
use yt_client::{Youtrack, YoutrackService};

const RESOURCE_QUERY_SYNTAX: &str = "youtrack://reference/issues/search/query-syntax";
const RESOURCE_ISSUE_FIELDS: &str = "youtrack://reference/issues/fields";
const RESOURCE_ARTICLE_FIELDS: &str = "youtrack://reference/articles/fields";

/// Заголовок с токеном пользователя к YouTrack — уровень 2 авторизации.
/// Уровень 1 (`Authorization`) снимается axum-слоем до MCP и сюда не доходит.
pub const USER_TOKEN_HEADER: &str = "x-youtrack-token";

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
pub(crate) fn user_token(extensions: &rmcp::model::Extensions) -> Option<String> {
    extensions
        .get::<http::request::Parts>()
        .and_then(|parts| parts.headers.get(USER_TOKEN_HEADER))
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
}

#[derive(Clone)]
pub struct YoutrackMCPServer {
    pub(crate) tool_router: ToolRouter<Self>,
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

        Self {
            tool_router: issues_tools
                + project_tools
                + articles_tools
                + commands_tools
                + comments_tools
                + users_tools
                + attachments_tools,
            yt,
        }
    }

    fn json_result(value: impl serde::Serialize) -> Result<CallToolResult, McpError> {
        let text = serde_json::to_string_pretty(&value)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
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

impl ServerHandler for YoutrackMCPServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_resources().enable_tools().build())
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
    fn router_lists_issue_and_article_tools() {
        let names: Vec<_> =
            server().tool_router.list_all().into_iter().map(|t| t.name.to_string()).collect();
        let expected = [
            "youtrack_find_issue",
            "youtrack_search_issues",
            "youtrack_my_issues",
            "youtrack_apply_command",
            "youtrack_create_issue",
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
        const POLICY: [(&str, bool, bool, bool); 28] = [
            ("youtrack_find_issue", true, false, false),
            ("youtrack_search_issues", true, false, false),
            ("youtrack_my_issues", true, false, false),
            ("youtrack_list_projects", true, false, false),
            ("youtrack_find_article", true, false, false),
            ("youtrack_search_articles", true, false, false),
            ("youtrack_list_comments", true, false, false),
            ("youtrack_list_article_comments", true, false, false),
            ("youtrack_apply_command", false, true, false),
            ("youtrack_update_article", false, true, false),
            ("youtrack_create_issue", false, false, false),
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
            assert_eq!(ann.open_world_hint, Some(true), "{name}: ходит во внешнюю систему");
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
}
