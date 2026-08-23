use reqwest::Client;
use yt_rs::{AuthorizationFlow, YoutrackClient};

pub struct Missing;
pub struct Set<T>(T);

pub struct YoutrackClientBuilder<B, T> {
    base_url: B,
    token: T,
}
impl YoutrackClientBuilder<Missing, Missing> {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn with_base_url(
        self,
        base_url: impl AsRef<str>,
    ) -> YoutrackClientBuilder<Set<String>, Missing> {
        YoutrackClientBuilder { base_url: Set(base_url.as_ref().to_owned()), token: Missing }
    }
}
impl Default for YoutrackClientBuilder<Missing, Missing> {
    fn default() -> Self {
        Self { base_url: Missing, token: Missing }
    }
}

impl YoutrackClientBuilder<Set<String>, Missing> {
    pub fn with_token(
        self,
        token: impl AsRef<str>,
    ) -> YoutrackClientBuilder<Set<String>, Set<String>> {
        YoutrackClientBuilder { base_url: self.base_url, token: Set(token.as_ref().to_owned()) }
    }
}
impl YoutrackClientBuilder<Set<String>, Set<String>> {
    pub fn build(self) -> yt_rs::Result<YoutrackClient> {
        let client = Client::new();
        let base_url = self.base_url.0;
        let af = AuthorizationFlow::PermanentBearerToken(self.token.0);

        YoutrackClient::new(client, &base_url, af)
    }
}

pub use yt_rs::{Article, Issue, Project, YoutrackError};
use yt_rs::{
    ArticleComment, CommandList, FieldsQuery, IssueComment, IssueSearchParams, ListParams,
    MutationParams,
};

use std::collections::HashMap;
use std::sync::Mutex;

/// Ошибки write-методов обёртки. `YoutrackError` из yt-rs не покрывает
/// ситуации, детектируемые локально (например неизвестный project shortName
/// до похода в сеть за созданием issue) — добавляем тонкий enum вместо
/// изобретения фейкового HTTP-ответа.
#[derive(thiserror::Error, Debug)]
pub enum WriteError {
    #[error("project '{0}' not found; available: {1}")]
    ProjectNotFound(String, String),
    #[error(transparent)]
    Youtrack(#[from] YoutrackError),
}

/// Сколько статей максимум вытягиваем за раз для локального поиска по базе
/// знаний (см. `search_articles`).
const ARTICLE_SCAN_LIMIT: i64 = 500;

/// Размер страницы выдачи поиска по умолчанию.
const DEFAULT_ARTICLE_PAGE: usize = 25;

/// Страница результатов поиска по базе знаний. У MCP нет протокольной
/// пагинации для результата тула (курсор есть только у `*/list`), поэтому
/// границы выдачи едут в самом теле ответа — включая честный признак того,
/// что окно сканирования упёрлось в потолок.
#[derive(Debug, serde::Serialize)]
pub struct ArticleSearchPage {
    pub articles: Vec<Article>,
    /// Сколько статей совпало во всём просканированном окне.
    pub matched: usize,
    /// Значение `skip` для следующей страницы; `null` — страниц больше нет.
    pub next_skip: Option<usize>,
    /// true — просмотрены не все статьи базы: сузь запрос или задай project.
    pub scan_truncated: bool,
}

/// Тонкий read-API поверх yt-rs. Один источник правды по YouTrack.
pub struct YoutrackService {
    client: YoutrackClient,
    /// Кэш project shortName (UPPERCASE) -> project id, чтобы не ходить в
    /// /api/admin/projects при каждом create_issue.
    project_ids: Mutex<HashMap<String, String>>,
}

impl YoutrackService {
    pub fn new(client: YoutrackClient) -> Self {
        Self { client, project_ids: Mutex::new(HashMap::new()) }
    }

    /// Дефолт для СПИСКОВ (search/my_issues): без description — оно тяжёлое
    /// и в списках раздувает JSON tool-результатов, токены LLM и дайджест.
    fn default_list_fields() -> Vec<String> {
        vec![
            "idReadable".to_owned(),
            "summary".to_owned(),
            "created".to_owned(),
            "updated".to_owned(),
            "resolved".to_owned(),
            "project(shortName,name)".to_owned(),
            "customFields(name,value(name,login,fullName))".to_owned(),
        ]
    }

    /// Дефолт для КАРТОЧКИ одной задачи (find_issue): полный, с description
    /// и reporter. Значения State/Priority/Assignee живут в customFields —
    /// без вложенной выборки они не приходят.
    fn default_issue_fields() -> Vec<String> {
        vec![
            "idReadable".to_owned(),
            "summary".to_owned(),
            "description".to_owned(),
            "created".to_owned(),
            "updated".to_owned(),
            "resolved".to_owned(),
            "project(shortName,name)".to_owned(),
            "reporter(login,fullName)".to_owned(),
            "customFields(name,value(name,login,fullName))".to_owned(),
        ]
    }

    fn fields_query(fields: Option<Vec<String>>, default: fn() -> Vec<String>) -> FieldsQuery {
        FieldsQuery::from(fields.unwrap_or_else(default))
    }

    pub async fn find_issue(&self, id: &str, fields: Option<Vec<String>>) -> yt_rs::Result<Issue> {
        self.client
            .issues_api()
            .get(id, Some(Self::fields_query(fields, Self::default_issue_fields)))
            .await
    }

    pub async fn search_issues(
        &self,
        query: &str,
        fields: Option<Vec<String>>,
    ) -> yt_rs::Result<Vec<Issue>> {
        let params = IssueSearchParams::default()
            .query(query.to_owned())
            .fields(Self::fields_query(fields, Self::default_list_fields))
            .top(100);
        self.client.issues_api().list(params).await
    }

    pub async fn my_issues(&self, assignee: &str) -> yt_rs::Result<Vec<Issue>> {
        let query = format!("assignee: {assignee} #Unresolved");
        self.search_issues(&query, None).await
    }

    fn project_fields() -> FieldsQuery {
        FieldsQuery::from(vec!["id".to_owned(), "shortName".to_owned(), "name".to_owned()])
    }

    /// Резолвит project shortName (регистронезависимо) в project id, используя
    /// кэш `project_ids`; при кэш-миссе — один запрос `/api/admin/projects`.
    async fn resolve_project_id(&self, short_name: &str) -> Result<String, WriteError> {
        let key = short_name.to_uppercase();
        if let Some(id) = self.project_ids.lock().unwrap().get(&key).cloned() {
            return Ok(id);
        }

        let projects = self
            .client
            .projects_api()
            .list(ListParams::default().fields(Self::project_fields()))
            .await?;

        let mut cache = self.project_ids.lock().unwrap();
        for p in &projects {
            if let (Some(id), Some(sn)) = (&p.id, &p.short_name) {
                cache.insert(sn.to_uppercase(), id.clone());
            }
        }

        cache.get(&key).cloned().ok_or_else(|| {
            let available =
                projects.iter().filter_map(|p| p.short_name.clone()).collect::<Vec<_>>().join(", ");
            WriteError::ProjectNotFound(short_name.to_owned(), available)
        })
    }

    /// Применяет YouTrack command language к одной задаче, опционально с
    /// комментарием в том же запросе.
    pub async fn apply_command(
        &self,
        issue_id: &str,
        command: &str,
        comment: Option<&str>,
    ) -> Result<(), WriteError> {
        // Ссылка на задачу идёт через idReadable: наружу (MCP/LLM) ходят
        // человекочитаемые "DEMO-1", а поле `id` ждёт внутренний "2-345".
        let issues = vec![Issue { id_readable: Some(issue_id.to_owned()), ..Default::default() }];
        let mut req = CommandList {
            query: Some(command.to_owned()),
            issues: Some(issues),
            ..Default::default()
        };
        if let Some(comment) = comment {
            req.comment = Some(comment.to_owned());
        }
        self.client.commands_api().apply(&req, MutationParams::default()).await?;
        Ok(())
    }

    /// Создаёт issue в проекте по shortName (регистронезависимо) и
    /// возвращает полную карточку (те же поля, что и find_issue).
    pub async fn create_issue(
        &self,
        project_short_name: &str,
        summary: &str,
        description: Option<&str>,
    ) -> Result<Issue, WriteError> {
        let project_id = self.resolve_project_id(project_short_name).await?;
        let issue = Issue {
            project: Some(Box::new(Project { id: Some(project_id), ..Default::default() })),
            summary: Some(summary.to_owned()),
            description: description.map(str::to_owned),
            ..Default::default()
        };
        let created = self
            .client
            .issues_api()
            .create(
                &issue,
                MutationParams::default()
                    .fields(Self::fields_query(None, Self::default_issue_fields)),
            )
            .await?;
        Ok(created)
    }

    /// Добавляет текстовый комментарий к задаче.
    pub async fn add_comment(&self, issue_id: &str, text: &str) -> Result<(), WriteError> {
        let comment = IssueComment { text: Some(text.to_owned()), ..Default::default() };
        self.client
            .issues_api()
            .comments_api(issue_id)
            .create(&comment, MutationParams::default())
            .await?;
        Ok(())
    }

    // ---- Knowledge Base (articles) ----

    /// Дефолт для СПИСКОВ статей: без content — тело статьи тяжёлое.
    fn default_article_list_fields() -> Vec<String> {
        vec![
            "idReadable".to_owned(),
            "summary".to_owned(),
            "updated".to_owned(),
            "hasChildren".to_owned(),
            "project(shortName,name)".to_owned(),
            "parentArticle(idReadable,summary)".to_owned(),
        ]
    }

    /// Дефолт для КАРТОЧКИ статьи: полный текст плюс соседи по дереву, чтобы
    /// агент мог идти вглубь базы знаний без лишнего вызова.
    fn default_article_fields() -> Vec<String> {
        let mut fields = Self::default_article_list_fields();
        fields.extend([
            "content".to_owned(),
            "created".to_owned(),
            "reporter(login,fullName)".to_owned(),
            "childArticles(idReadable,summary)".to_owned(),
        ]);
        fields
    }

    pub async fn find_article(
        &self,
        id: &str,
        fields: Option<Vec<String>>,
    ) -> yt_rs::Result<Article> {
        self.client
            .articles_api()
            .get(id, Some(Self::fields_query(fields, Self::default_article_fields)))
            .await
    }

    /// Поиск по базе знаний. У `/api/articles` НЕТ server-side query (в отличие
    /// от issues), поэтому тянем первые `ARTICLE_SCAN_LIMIT` статей (опционально
    /// одного проекта) и фильтруем локально: все слова запроса должны
    /// встретиться в summary+content. Пустой query = просто список статей.
    /// content в выдаче обнуляется — за текстом идти в `find_article`.
    ///
    /// Пагинация — по совпадениям, а не по сырому списку: `skip`/`limit`
    /// режут уже отфильтрованное. Окно сканирования при этом каждый раз одно
    /// и то же, так что страницы согласованы между вызовами.
    ///
    /// ponytail: линейное сканирование с потолком в ARTICLE_SCAN_LIMIT статей
    /// (о срезе честно сообщает `scan_truncated`); если база знаний
    /// перерастёт — переезжать на $skip-пагинацию самого сканирования или на
    /// собственный индекс/эмбеддинги.
    pub async fn search_articles(
        &self,
        query: &str,
        project_short_name: Option<&str>,
        limit: Option<usize>,
        skip: Option<usize>,
    ) -> Result<ArticleSearchPage, WriteError> {
        let mut fields = Self::default_article_list_fields();
        fields.push("content".to_owned());
        let params =
            ListParams::default().fields(FieldsQuery::from(fields)).top(ARTICLE_SCAN_LIMIT);

        let scanned = match project_short_name {
            Some(short_name) => {
                let project_id = self.resolve_project_id(short_name).await?;
                self.client.projects_api().articles(&project_id, params).await?
            }
            None => self.client.articles_api().list(params).await?,
        };
        // Ровно упёрлись в потолок — значит за окном могут быть ещё статьи.
        let scan_truncated = scanned.len() as i64 >= ARTICLE_SCAN_LIMIT;

        let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        let matches: Vec<Article> = scanned
            .into_iter()
            .filter_map(|mut article| {
                let haystack = format!(
                    "{} {}",
                    article.summary.as_deref().unwrap_or(""),
                    article.content.as_deref().unwrap_or("")
                )
                .to_lowercase();
                if !terms.iter().all(|t| haystack.contains(t)) {
                    return None;
                }
                article.content = None;
                Some(article)
            })
            .collect();

        // limit приходит от LLM: 0 или пропуск дают бесполезную пустую
        // страницу, поэтому зажимаем в осмысленный диапазон.
        let limit = limit.unwrap_or(DEFAULT_ARTICLE_PAGE).clamp(1, ARTICLE_SCAN_LIMIT as usize);
        let skip = skip.unwrap_or(0);
        let matched = matches.len();
        let articles: Vec<Article> = matches.into_iter().skip(skip).take(limit).collect();
        let next_skip = (skip + articles.len() < matched).then_some(skip + articles.len());

        Ok(ArticleSearchPage { articles, matched, next_skip, scan_truncated })
    }

    /// Страница списка статей для инкрементального синка: только метаданные,
    /// без `content`.
    ///
    /// В отличие от `search_articles` не упирается в `ARTICLE_SCAN_LIMIT` и
    /// ничего не фильтрует — вызывающий листает сам, пока страница приходит
    /// полной. Нужен `kb-index`, чтобы сравнивать `updated` по всей базе знаний.
    pub async fn list_articles_page(&self, top: i64, skip: i64) -> yt_rs::Result<Vec<Article>> {
        let params = ListParams::default()
            .top(top)
            .skip(skip)
            .fields(Self::fields_query(None, Self::default_article_list_fields));
        self.client.articles_api().list(params).await
    }

    /// То же в границах одного проекта: у `/api/articles` фильтра по проекту
    /// нет, поэтому при заданном списке проектов синк идёт сюда, по одному
    /// проекту за раз.
    pub async fn list_project_articles_page(
        &self,
        short_name: &str,
        top: i64,
        skip: i64,
    ) -> Result<Vec<Article>, WriteError> {
        let project_id = self.resolve_project_id(short_name).await?;
        let params = ListParams::default()
            .top(top)
            .skip(skip)
            .fields(Self::fields_query(None, Self::default_article_list_fields));
        Ok(self.client.projects_api().articles(&project_id, params).await?)
    }

    /// Создаёт статью в проекте по shortName; `parent_id` (idReadable другой
    /// статьи) делает её дочерней.
    pub async fn create_article(
        &self,
        project_short_name: &str,
        summary: &str,
        content: Option<&str>,
        parent_id: Option<&str>,
    ) -> Result<Article, WriteError> {
        let project_id = self.resolve_project_id(project_short_name).await?;
        let article = Article {
            project: Some(Box::new(Project { id: Some(project_id), ..Default::default() })),
            summary: Some(summary.to_owned()),
            content: content.map(str::to_owned),
            parent_article: parent_id.map(|id| {
                Box::new(Article { id_readable: Some(id.to_owned()), ..Default::default() })
            }),
            ..Default::default()
        };
        let created = self
            .client
            .articles_api()
            .create(
                &article,
                MutationParams::default()
                    .fields(Self::fields_query(None, Self::default_article_fields)),
            )
            .await?;
        Ok(created)
    }

    /// Обновляет заголовок и/или текст статьи. Не переданные поля YouTrack
    /// оставляет как есть — это patch, а не replace.
    pub async fn update_article(
        &self,
        id: &str,
        summary: Option<&str>,
        content: Option<&str>,
    ) -> Result<Article, WriteError> {
        let article = Article {
            summary: summary.map(str::to_owned),
            content: content.map(str::to_owned),
            ..Default::default()
        };
        let updated = self
            .client
            .articles_api()
            .update(
                id,
                &article,
                MutationParams::default()
                    .fields(Self::fields_query(None, Self::default_article_fields)),
            )
            .await?;
        Ok(updated)
    }

    /// Добавляет комментарий к статье.
    pub async fn add_article_comment(
        &self,
        article_id: &str,
        text: &str,
    ) -> Result<(), WriteError> {
        let comment = ArticleComment { text: Some(text.to_owned()), ..Default::default() };
        self.client
            .articles_api()
            .comments_api(article_id)
            .create(&comment, MutationParams::default())
            .await?;
        Ok(())
    }

    /// Список всех проектов (id/shortName/name) — без кэша.
    pub async fn list_projects(&self) -> Result<Vec<Project>, WriteError> {
        let projects = self
            .client
            .projects_api()
            .list(ListParams::default().fields(Self::project_fields()))
            .await?;
        Ok(projects)
    }
}
