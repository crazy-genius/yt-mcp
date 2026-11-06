use crate::mcp::YoutrackMCPServer;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{PromptMessage, Role};
use rmcp::{prompt, prompt_router};
use schemars::JsonSchema;
use serde::Deserialize;

/// Хвост каждого промпта: язык ответа определяет пользователь, не шаблон.
const LANG: &str = "Answer in the user's language.";

/// Хвост промптов, ведущих к записи: черновик сначала видит человек.
const CONFIRM: &str = "Show the draft and create it only after the user confirms.";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SummarizeIssueArgs {
    /// Issue id, e.g. DEMO-7.
    pub id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DraftIssueArgs {
    /// Project short name the issue goes to, e.g. DEMO.
    pub project: String,
    /// Free-form description of the problem or task.
    pub text: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct KbFromIssuesArgs {
    /// YouTrack search query that selects the source issues.
    pub query: String,
    /// Project short name for the new article; defaults to the project of the first issue found.
    pub project: Option<String>,
}

/// Промпты — шаблоны, запускаемые пользователем. YouTrack они не трогают,
/// поэтому токен не нужен: тулы, которые назовёт текст, проверят его сами.
#[prompt_router(router = "youtrack_prompts", vis = "pub")]
impl YoutrackMCPServer {
    #[prompt(
        name = "triage_my_issues",
        description = "Review the issues assigned to me and pick what to do first."
    )]
    pub async fn triage_my_issues(&self) -> Vec<PromptMessage> {
        vec![PromptMessage::new_text(
            Role::User,
            format!(
                "Call youtrack_whoami to learn who I am, then youtrack_my_issues to load my \
                 open issues; follow next_skip until all pages are read, up to 100 issues. \
                 Group the issues by priority and state, name the 3 I should take first and \
                 say why. {LANG}"
            ),
        )]
    }

    #[prompt(
        name = "summarize_issue",
        description = "Summarize an issue with its comments and attachments."
    )]
    pub async fn summarize_issue(
        &self,
        Parameters(args): Parameters<SummarizeIssueArgs>,
    ) -> Vec<PromptMessage> {
        vec![PromptMessage::new_text(
            Role::User,
            format!(
                "Read issue {id} with youtrack_find_issue, its comments with \
                 youtrack_list_comments and its attachments with youtrack_list_attachments. \
                 Summarize the current state, the decisions made, the blockers and the next \
                 step. {LANG}",
                id = args.id
            ),
        )]
    }

    #[prompt(name = "draft_issue", description = "Turn free-form text into a well-formed issue.")]
    pub async fn draft_issue(
        &self,
        Parameters(args): Parameters<DraftIssueArgs>,
    ) -> Vec<PromptMessage> {
        vec![PromptMessage::new_text(
            Role::User,
            format!(
                "From the text below draft an issue for project {project}: a summary of at \
                 most 80 characters and a description with context, steps to reproduce, \
                 expected and actual result, and acceptance criteria. {CONFIRM} Create it \
                 with youtrack_create_issue. {LANG}\n\nText:\n{text}",
                project = args.project,
                text = args.text
            ),
        )]
    }

    #[prompt(
        name = "kb_from_issues",
        description = "Draft a Knowledge Base article from resolved issues."
    )]
    pub async fn kb_from_issues(
        &self,
        Parameters(args): Parameters<KbFromIssuesArgs>,
    ) -> Vec<PromptMessage> {
        let target = match &args.project {
            Some(p) => format!("project {p}"),
            None => "the project of the first issue found".to_string(),
        };
        vec![PromptMessage::new_text(
            Role::User,
            format!(
                "Find resolved issues with youtrack_search_issues using the query: {query}. \
                 If the query does not already restrict resolution, add `#Resolved` to it. \
                 Draft a Knowledge Base article from them: the problem, the solution, and \
                 links to the issues. {CONFIRM} Create it with youtrack_create_article in \
                 {target}. {LANG}",
                query = args.query
            ),
        )]
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::mcp::YoutrackMCPServer;
    use rmcp::model::GetPromptRequestParams;
    use rmcp::{ServiceExt, serve_server};
    use yt_client::Youtrack;

    fn server() -> YoutrackMCPServer {
        YoutrackMCPServer::new(Arc::new(Youtrack::new("https://example.com").expect("builds")))
    }

    #[test]
    fn the_four_prompts_are_listed() {
        let names: Vec<_> = server().prompt_router.list_all().into_iter().map(|p| p.name).collect();
        assert_eq!(names, ["draft_issue", "kb_from_issues", "summarize_issue", "triage_my_issues"]);
    }

    async fn client() -> rmcp::service::RunningService<rmcp::RoleClient, ()> {
        let (server_io, client_io) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            if let Ok(s) = serve_server(server(), server_io).await {
                let _ = s.waiting().await;
            }
        });
        ().serve(client_io).await.expect("client connects")
    }

    #[tokio::test]
    async fn summarize_issue_puts_the_id_into_the_text() {
        let client = client().await;
        let args = serde_json::json!({"id": "DEMO-7"}).as_object().unwrap().clone();
        let got = client
            .get_prompt(GetPromptRequestParams::new("summarize_issue").with_arguments(args))
            .await
            .expect("prompt renders");
        assert_eq!(got.messages.len(), 1);
        let text = &got.messages[0].content.as_text().expect("text expected").text;
        assert!(text.contains("DEMO-7"), "{text}");
        assert!(text.ends_with("Answer in the user's language."), "{text}");
    }

    #[tokio::test]
    async fn a_missing_required_argument_is_invalid_params() {
        let client = client().await;
        let err = client
            .get_prompt(GetPromptRequestParams::new("summarize_issue"))
            .await
            .expect_err("no id");
        let rmcp::ServiceError::McpError(e) = err else { panic!("{err:?}") };
        assert_eq!(e.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    }
}
