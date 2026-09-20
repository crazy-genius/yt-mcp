use base64::Engine;
use reqwest::Client;
use url::Url;
use yt_rs::{AuthorizationFlow, YoutrackClient};

/// Разделяемая на весь процесс часть: базовый URL и пул соединений.
/// Токена здесь нет — он приезжает с каждым запросом, см. [`Youtrack::as_user`].
pub struct Youtrack {
    http: Client,
    /// Отдельный клиент для скачивания вложения по `url`: без редиректов и с
    /// таймаутом. Общий `http` уходит в каждый вызов YouTrack API через
    /// `YoutrackClient` — запрещать ему редиректы нельзя, YouTrack однажды
    /// ответит 3xx и всё сломает. А для скачивания по чужому `url`
    /// автоследование редиректам ломает https-only правило
    /// `validate_attachment_url`: `https://attacker/x` отвечает
    /// `302 → http://169.254.169.254/...`, и правило держит ровно один hop.
    attachment_http: Client,
    base_url: String,
}

impl Youtrack {
    /// Проверяет URL сразу, чтобы ошибка конфигурации всплыла при старте
    /// процесса, а не на первом вызове тула.
    pub fn new(base_url: impl Into<String>) -> yt_rs::Result<Self> {
        let base_url = base_url.into();
        let http = Client::new();
        // Пробное построение: разбор URL живёт внутри YoutrackClient::new,
        // отдельного парсера наружу yt-rs не даёт.
        YoutrackClient::new(
            http.clone(),
            &base_url,
            AuthorizationFlow::PermanentBearerToken(String::new()),
        )?;
        // Таймаут — чтобы сервер, медленно капающий байтами под лимитом
        // MAX_ATTACHMENT_BYTES, не держал вызов тула (и соединение) вечно:
        // счётчик байт его не поймает, он весь честно укладывается в лимит.
        let attachment_http = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(30))
            // Защита в глубину: https-only держится и на уровне транспорта,
            // а не только на вызове validate_attachment_url перед fetch_bytes.
            .https_only(true)
            .build()?;
        Ok(Self { http, attachment_http, base_url })
    }

    /// Сервис от имени конкретного пользователя. Стоит около нуля:
    /// `reqwest::Client` клонируется по `Arc` и делит пул соединений.
    pub fn as_user(&self, token: &str) -> yt_rs::Result<YoutrackService> {
        let client = YoutrackClient::new(
            self.http.clone(),
            &self.base_url,
            AuthorizationFlow::PermanentBearerToken(token.to_owned()),
        )?;
        Ok(YoutrackService::new(client, self.attachment_http.clone()))
    }
}

pub use yt_rs::{Article, ArticleAttachment, Issue, IssueAttachment, Project, User, YoutrackError};
use yt_rs::{
    ArticleComment, AttachmentUpload, CommandList, FieldsQuery, IssueComment, IssueSearchParams,
    ListParams, MutationParams,
};

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
    #[error("attachment too large: {size} bytes, limit is {limit}")]
    AttachmentTooLarge { size: u64, limit: usize },
    #[error("bad base64 content: {0}")]
    BadBase64(String),
    #[error("attachment url rejected: {0}")]
    BadUrl(String),
    #[error("fetching attachment url: {0}")]
    Fetch(#[from] reqwest::Error),
}

/// Сколько статей максимум вытягиваем за раз для локального поиска по базе
/// знаний (см. `search_articles`).
const ARTICLE_SCAN_LIMIT: i64 = 500;

/// Размер страницы выдачи поиска по умолчанию.
const DEFAULT_ARTICLE_PAGE: usize = 25;

/// Размер страницы комментариев по умолчанию и его потолок. Значение
/// приезжает от LLM: 0 дал бы пустую выдачу, а запрос «все» — вывалил бы
/// весь тред в контекст.
const DEFAULT_COMMENT_PAGE: i64 = 50;
const MAX_COMMENT_PAGE: i64 = 200;

/// Сколько пользователей максимум вытягиваем за раз для локального поиска.
/// У `/api/users` нет серверного `query` — ни в OpenAPI-спеке, ни в
/// `ListParams` у yt-rs, — поэтому фильтр локальный.
///
/// ponytail: линейное сканирование с потолком; о срезе честно сообщает
/// `scan_truncated`. Если инстанс перерастёт — идти по $skip-страницам скана
/// или ждать серверного поиска в API.
const USER_SCAN_LIMIT: i64 = 500;

/// Размер страницы выдачи поиска людей по умолчанию.
const DEFAULT_USER_PAGE: usize = 25;

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

/// Страница поиска людей. Форма повторяет `ArticleSearchPage` по той же
/// причине: у MCP нет протокольной пагинации результата тула, поэтому границы
/// выдачи едут в теле ответа. Общий `SearchPage<T>` здесь делать нельзя —
/// он переименовал бы `articles` в `items` в JSON, который уже описан в
/// описании `youtrack_search_articles` и читается моделью.
#[derive(Debug, serde::Serialize)]
pub struct UserSearchPage {
    pub users: Vec<User>,
    /// Сколько людей совпало во всём просканированном окне.
    pub matched: usize,
    /// Значение `skip` для следующей страницы; `null` — страниц больше нет.
    pub next_skip: Option<usize>,
    /// true — просмотрены не все пользователи: сузь запрос.
    pub scan_truncated: bool,
}

/// Потолок на вложение: и на `content_base64`, и на скачивание по `url`, и
/// как страховка при декодировании прочитанного из YouTrack.
const MAX_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;

/// Откуда взять байты вложения. Ровно один канал на вызов: взаимоисключение
/// проверяется в теле тула, до попадания сюда.
pub enum UploadSource {
    Base64(String),
    Url(String),
}

/// Содержимое вложения, уже раскодированное.
///
/// `bytes` — для текстовой выдачи, `base64` — для `ContentBlock::image`:
/// кодировать обратно то, что только что раскодировали, незачем, а тащить
/// зависимость base64 во второй крейт ради этого — тем более.
#[derive(Debug)]
pub struct AttachmentContent {
    pub name: String,
    pub mime_type: Option<String>,
    pub size: Option<i64>,
    pub bytes: Vec<u8>,
    pub base64: String,
}

/// Срезает `data:<mime>;base64,` — YouTrack отдаёт `base64Content` именно так,
/// а модель, скопировавшая data-URI, иначе получила бы мусор в первых байтах.
fn strip_data_uri(raw: &str) -> &str {
    raw.strip_prefix("data:")
        .and_then(|rest| rest.split_once(";base64,"))
        .map(|(_, payload)| payload)
        .unwrap_or(raw)
}

/// Декодирует base64 с проверкой лимита. Пробелы и переводы строк выбрасываются:
/// их вставляют и YouTrack, и модель.
fn decode_base64(raw: &str, limit: usize) -> Result<Vec<u8>, WriteError> {
    let payload: String = strip_data_uri(raw).split_whitespace().collect();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&payload)
        .map_err(|e| WriteError::BadBase64(e.to_string()))?;
    if bytes.len() > limit {
        return Err(WriteError::AttachmentTooLarge { size: bytes.len() as u64, limit });
    }
    Ok(bytes)
}

/// Разбор и проверка схемы — без сети, поэтому проверяемо юнит-тестом.
///
/// ponytail: проверки, КУДА указывает хост, здесь сознательно нет — решение
/// владельца. `fetch_bytes` редиректы не проходит (клиент собран с
/// `Policy::none()` и сам отказывает на 3xx), так что https→http downgrade
/// за один прыжок исключён — но потолок всё равно есть: сервер сходит по
/// ЛЮБОМУ https-адресу, до которого дотягивается из своего сетевого
/// периметра, и если этот адрес сам отвечает по https без редиректа — ничего
/// его не остановит, содержимое осядет вложением в YouTrack. Вектор реальный
/// — prompt injection в тексте задачи может подсунуть агенту такой url. Путь
/// наверх: резолв хоста через `tokio::net::lookup_host` с отказом
/// loopback/private/link-local/CGNAT, либо allowlist хостов в env.
fn validate_attachment_url(raw: &str) -> Result<Url, WriteError> {
    let url = Url::parse(raw).map_err(|e| WriteError::BadUrl(format!("{raw}: {e}")))?;
    if url.scheme() != "https" {
        return Err(WriteError::BadUrl(format!(
            "{raw}: scheme must be https, got '{}'",
            url.scheme()
        )));
    }
    Ok(url)
}

/// Скачивание с обрывом по лимиту. Схему не проверяет — это сделано выше, и
/// разделение не косметическое: только так канал `url` покрывается тестами,
/// потому что wiremock отдаёт http.
///
/// Редирект отклоняется явно: клиент вызывающей стороны собран с
/// `Policy::none()`, так что сам он 3xx не пройдёт, но `error_for_status()`
/// не считает 3xx ошибкой — без явной проверки статуса это тихо стало бы
/// пустым "успешным" файлом вместо отказа.
///
/// `Content-Length` отсекает до чтения тела, но полагаться на него нельзя —
/// он умеет врать или отсутствовать, поэтому прочитанное всё равно считается.
async fn fetch_bytes(http: &Client, url: Url, limit: usize) -> Result<Vec<u8>, WriteError> {
    let response = http.get(url).send().await?;
    if response.status().is_redirection() {
        return Err(WriteError::BadUrl(format!(
            "server answered with a redirect ({}); redirects are not followed",
            response.status()
        )));
    }
    let mut response = response.error_for_status()?;
    if let Some(len) = response.content_length()
        && len > limit as u64
    {
        return Err(WriteError::AttachmentTooLarge { size: len, limit });
    }

    let mut bytes: Vec<u8> = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len() + chunk.len() > limit {
            return Err(WriteError::AttachmentTooLarge {
                size: (bytes.len() + chunk.len()) as u64,
                limit,
            });
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// Тонкий API поверх yt-rs от имени одного пользователя.
pub struct YoutrackService {
    client: YoutrackClient,
    /// Клиент без автоследования редиректам и с таймаутом — специально для
    /// скачивания вложения по `url`, собран в `Youtrack::new`. Не тот
    /// `http`, что уходит в `YoutrackClient`: у него другая (обычная)
    /// политика редиректов, и смешивать их нельзя.
    attachment_http: Client,
}

impl YoutrackService {
    /// Приватно для крейта: единственный вызывающий — `Youtrack::as_user`.
    /// Публичный конструктор позволил бы собрать сервис с обычным (следующим
    /// редиректам) клиентом вместо `attachment_http` — именно та дыра,
    /// которую закрывает `Youtrack::new`.
    pub(crate) fn new(client: YoutrackClient, attachment_http: Client) -> Self {
        Self { client, attachment_http }
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

    /// Пустой список от вызывающего — это «не выбрал», а не «верни ничего»:
    /// YouTrack на пустой `fields=` отдаёт объект с одним `$type`.
    fn fields_query(fields: Option<Vec<String>>, default: fn() -> Vec<String>) -> FieldsQuery {
        FieldsQuery::from(fields.filter(|f| !f.is_empty()).unwrap_or_else(default))
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

    /// Резолвит project shortName (регистронезависимо) в project id.
    ///
    /// ponytail: без кэша — запрос к /api/admin/projects на каждый вызов
    /// с shortName. Общий кэш здесь нельзя: у пользователей разная видимость
    /// проектов. Если станет больно — пер-юзерный кэш с TTL.
    async fn resolve_project_id(&self, short_name: &str) -> Result<String, WriteError> {
        let key = short_name.to_uppercase();
        let projects = self
            .client
            .projects_api()
            .list(ListParams::default().fields(Self::project_fields()))
            .await?;

        projects
            .iter()
            .find(|p| p.short_name.as_deref().map(str::to_uppercase).as_deref() == Some(&key))
            .and_then(|p| p.id.clone())
            .ok_or_else(|| {
                let available = projects
                    .iter()
                    .filter_map(|p| p.short_name.clone())
                    .collect::<Vec<_>>()
                    .join(", ");
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

    /// Дефолт для комментариев. `text` входит в список сознательно: в отличие
    /// от `description` у задач, текст комментария и есть смысл вызова — без
    /// него `list` вернул бы пустые оболочки с одними id.
    fn default_comment_fields() -> Vec<String> {
        vec![
            "id".to_owned(),
            "text".to_owned(),
            "created".to_owned(),
            "updated".to_owned(),
            "author(login,fullName)".to_owned(),
        ]
    }

    fn comment_page(top: Option<i64>, skip: Option<i64>) -> ListParams {
        ListParams::default()
            .top(top.unwrap_or(DEFAULT_COMMENT_PAGE).clamp(1, MAX_COMMENT_PAGE))
            .skip(skip.unwrap_or(0).max(0))
            .fields(FieldsQuery::from(Self::default_comment_fields()))
    }

    fn comment_mutation() -> MutationParams {
        MutationParams::default().fields(FieldsQuery::from(Self::default_comment_fields()))
    }

    /// Страница комментариев задачи.
    pub async fn list_comments(
        &self,
        issue_id: &str,
        top: Option<i64>,
        skip: Option<i64>,
    ) -> yt_rs::Result<Vec<IssueComment>> {
        self.client.issues_api().comments_api(issue_id).list(Self::comment_page(top, skip)).await
    }

    /// Заменяет текст комментария задачи. `comment_id` — внутренний id
    /// YouTrack (вида `4-123`), человекочитаемого у комментариев нет; его
    /// отдаёт `list_comments`.
    pub async fn update_comment(
        &self,
        issue_id: &str,
        comment_id: &str,
        text: &str,
    ) -> yt_rs::Result<IssueComment> {
        let comment = IssueComment { text: Some(text.to_owned()), ..Default::default() };
        self.client
            .issues_api()
            .comments_api(issue_id)
            .update(comment_id, &comment, Self::comment_mutation())
            .await
    }

    pub async fn delete_comment(&self, issue_id: &str, comment_id: &str) -> yt_rs::Result<()> {
        self.client.issues_api().comments_api(issue_id).delete(comment_id).await
    }

    pub async fn list_article_comments(
        &self,
        article_id: &str,
        top: Option<i64>,
        skip: Option<i64>,
    ) -> yt_rs::Result<Vec<ArticleComment>> {
        self.client
            .articles_api()
            .comments_api(article_id)
            .list(Self::comment_page(top, skip))
            .await
    }

    pub async fn update_article_comment(
        &self,
        article_id: &str,
        comment_id: &str,
        text: &str,
    ) -> yt_rs::Result<ArticleComment> {
        let comment = ArticleComment { text: Some(text.to_owned()), ..Default::default() };
        self.client
            .articles_api()
            .comments_api(article_id)
            .update(comment_id, &comment, Self::comment_mutation())
            .await
    }

    pub async fn delete_article_comment(
        &self,
        article_id: &str,
        comment_id: &str,
    ) -> yt_rs::Result<()> {
        self.client.articles_api().comments_api(article_id).delete(comment_id).await
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

    // ---- Люди ----

    fn user_fields() -> FieldsQuery {
        FieldsQuery::from(vec![
            "id".to_owned(),
            "login".to_owned(),
            "fullName".to_owned(),
            "email".to_owned(),
            "banned".to_owned(),
        ])
    }

    /// Кто владелец токена этого вызова. Нужен, чтобы агент мог ответить на
    /// «покажи мои задачи», не спрашивая логин у человека.
    pub async fn whoami(&self) -> yt_rs::Result<User> {
        self.client.users_api().me(Some(Self::user_fields())).await
    }

    /// Поиск людей по подстроке в login/fullName/email: все слова запроса
    /// должны встретиться, регистр не важен. Пустой запрос = просто список.
    /// Забаненные не выдаются никогда — подставлять их в меншн или в assignee
    /// бессмысленно.
    pub async fn search_users(
        &self,
        query: &str,
        limit: Option<usize>,
        skip: Option<usize>,
    ) -> yt_rs::Result<UserSearchPage> {
        let params = ListParams::default().fields(Self::user_fields()).top(USER_SCAN_LIMIT);
        let scanned = self.client.users_api().list(params).await?;
        // Ровно упёрлись в потолок — значит за окном могут быть ещё люди.
        let scan_truncated = scanned.len() as i64 >= USER_SCAN_LIMIT;

        let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        let matches: Vec<User> = scanned
            .into_iter()
            .filter(|user| user.banned() != Some(true))
            .filter(|user| {
                let haystack = format!(
                    "{} {} {}",
                    user.login().unwrap_or(""),
                    user.full_name().unwrap_or(""),
                    user.email().unwrap_or("")
                )
                .to_lowercase();
                terms.iter().all(|t| haystack.contains(t))
            })
            .collect();

        let limit = limit.unwrap_or(DEFAULT_USER_PAGE).clamp(1, USER_SCAN_LIMIT as usize);
        let skip = skip.unwrap_or(0);
        let matched = matches.len();
        let users: Vec<User> = matches.into_iter().skip(skip).take(limit).collect();
        let next_skip = (skip + users.len() < matched).then_some(skip + users.len());

        Ok(UserSearchPage { users, matched, next_skip, scan_truncated })
    }

    // ---- Вложения ----

    /// Дефолт списка вложений. `base64Content` здесь НЕТ сознательно: иначе
    /// один `list` вывалил бы в контекст модели все файлы задачи целиком.
    fn default_attachment_fields() -> Vec<String> {
        vec![
            "id".to_owned(),
            "name".to_owned(),
            "size".to_owned(),
            "mimeType".to_owned(),
            "created".to_owned(),
            "author(login,fullName)".to_owned(),
            "url".to_owned(),
        ]
    }

    /// Дешёвые поля для решения «читать или отказать» до скачивания.
    fn attachment_meta_fields() -> Vec<String> {
        vec!["id".to_owned(), "name".to_owned(), "size".to_owned(), "mimeType".to_owned()]
    }

    fn attachment_content_fields() -> Vec<String> {
        let mut fields = Self::attachment_meta_fields();
        fields.push("base64Content".to_owned());
        fields
    }

    /// Байты вложения из того канала, который выбрал вызывающий.
    async fn attachment_bytes(&self, source: UploadSource) -> Result<Vec<u8>, WriteError> {
        match source {
            UploadSource::Base64(raw) => decode_base64(&raw, MAX_ATTACHMENT_BYTES),
            UploadSource::Url(raw) => {
                let url = validate_attachment_url(&raw)?;
                fetch_bytes(&self.attachment_http, url, MAX_ATTACHMENT_BYTES).await
            }
        }
    }

    pub async fn list_attachments(&self, issue_id: &str) -> yt_rs::Result<Vec<IssueAttachment>> {
        self.client
            .issues_api()
            .attachments_api(issue_id)
            .list(
                ListParams::default().fields(FieldsQuery::from(Self::default_attachment_fields())),
            )
            .await
    }

    /// Метаданные одного вложения — без содержимого.
    pub async fn attachment_meta(
        &self,
        issue_id: &str,
        attachment_id: &str,
    ) -> yt_rs::Result<IssueAttachment> {
        self.client
            .issues_api()
            .attachments_api(issue_id)
            .get(attachment_id, Some(FieldsQuery::from(Self::attachment_meta_fields())))
            .await
    }

    /// Содержимое вложения. Звать только после `attachment_meta`: решение по
    /// mime и размеру принимается там, чтобы отказ на большом бинаре не стоил
    /// нам его скачивания.
    pub async fn read_attachment(
        &self,
        issue_id: &str,
        attachment_id: &str,
    ) -> Result<AttachmentContent, WriteError> {
        let attachment = self
            .client
            .issues_api()
            .attachments_api(issue_id)
            .get(attachment_id, Some(FieldsQuery::from(Self::attachment_content_fields())))
            .await?;
        Self::into_content(
            attachment.name,
            attachment.mime_type,
            attachment.size,
            attachment.base64_content,
        )
    }

    /// Общая сборка `AttachmentContent` для задач и статей: модели у yt-rs
    /// разные, а поля — одни и те же четыре.
    ///
    /// Отсутствие `base64Content` — не пустой файл, а отказ YouTrack его
    /// отдать (внешнее хранилище, права, изменившийся набор полей); молчаливое
    /// превращение этого в `bytes: vec![]` неотличимо от честного пустого
    /// файла, поэтому это ошибка.
    fn into_content(
        name: Option<String>,
        mime_type: Option<String>,
        size: Option<i64>,
        base64_content: Option<String>,
    ) -> Result<AttachmentContent, WriteError> {
        let raw = base64_content
            .ok_or_else(|| WriteError::BadBase64("YouTrack returned no base64Content".into()))?;
        // Нормализация (срез data-URI + пробелы) считается один раз: `base64`
        // и `bytes` обязаны описывать один и тот же payload, а не две
        // независимо посчитанные копии.
        let base64: String = strip_data_uri(&raw).split_whitespace().collect();
        let bytes = decode_base64(&base64, MAX_ATTACHMENT_BYTES)?;
        Ok(AttachmentContent { name: name.unwrap_or_default(), mime_type, size, bytes, base64 })
    }

    pub async fn upload_attachment(
        &self,
        issue_id: &str,
        file_name: &str,
        source: UploadSource,
    ) -> Result<Vec<IssueAttachment>, WriteError> {
        let bytes = self.attachment_bytes(source).await?;
        let upload = AttachmentUpload { file_name: file_name.to_owned(), bytes };
        Ok(self
            .client
            .issues_api()
            .attachments_api(issue_id)
            .create(
                vec![upload],
                MutationParams::default()
                    .fields(FieldsQuery::from(Self::default_attachment_fields())),
            )
            .await?)
    }

    pub async fn delete_attachment(
        &self,
        issue_id: &str,
        attachment_id: &str,
    ) -> yt_rs::Result<()> {
        self.client.issues_api().attachments_api(issue_id).delete(attachment_id).await
    }

    pub async fn list_article_attachments(
        &self,
        article_id: &str,
    ) -> yt_rs::Result<Vec<ArticleAttachment>> {
        self.client
            .articles_api()
            .attachments_api(article_id)
            .list(
                ListParams::default().fields(FieldsQuery::from(Self::default_attachment_fields())),
            )
            .await
    }

    pub async fn article_attachment_meta(
        &self,
        article_id: &str,
        attachment_id: &str,
    ) -> yt_rs::Result<ArticleAttachment> {
        self.client
            .articles_api()
            .attachments_api(article_id)
            .get(attachment_id, Some(FieldsQuery::from(Self::attachment_meta_fields())))
            .await
    }

    pub async fn read_article_attachment(
        &self,
        article_id: &str,
        attachment_id: &str,
    ) -> Result<AttachmentContent, WriteError> {
        let attachment = self
            .client
            .articles_api()
            .attachments_api(article_id)
            .get(attachment_id, Some(FieldsQuery::from(Self::attachment_content_fields())))
            .await?;
        Self::into_content(
            attachment.name,
            attachment.mime_type,
            attachment.size,
            attachment.base64_content,
        )
    }

    pub async fn upload_article_attachment(
        &self,
        article_id: &str,
        file_name: &str,
        source: UploadSource,
    ) -> Result<Vec<ArticleAttachment>, WriteError> {
        let bytes = self.attachment_bytes(source).await?;
        let upload = AttachmentUpload { file_name: file_name.to_owned(), bytes };
        Ok(self
            .client
            .articles_api()
            .attachments_api(article_id)
            .create(
                vec![upload],
                MutationParams::default()
                    .fields(FieldsQuery::from(Self::default_attachment_fields())),
            )
            .await?)
    }

    pub async fn delete_article_attachment(
        &self,
        article_id: &str,
        attachment_id: &str,
    ) -> yt_rs::Result<()> {
        self.client.articles_api().attachments_api(article_id).delete(attachment_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// YouTrack отдаёт `base64Content` как data-URI, а модель на загрузке
    /// может прислать и то, и другое. Префикс срезается в обе стороны.
    #[test]
    fn strip_data_uri_handles_both_shapes() {
        assert_eq!(strip_data_uri("aGk="), "aGk=");
        assert_eq!(strip_data_uri("data:text/plain;base64,aGk="), "aGk=");
        assert_eq!(strip_data_uri("data:image/png;base64,iVBOR"), "iVBOR");
        // Не base64 data-URI — не наш случай, отдаём как есть.
        assert_eq!(strip_data_uri("data:text/plain,hi"), "data:text/plain,hi");
    }

    #[test]
    fn decode_base64_accepts_whitespace_and_data_uri() {
        // Переводы строк внутрь base64 вставляют и YouTrack, и LLM.
        assert_eq!(decode_base64("aG\nVsbG8=", 64).expect("ok"), b"hello");
        assert_eq!(decode_base64("data:text/plain;base64,aGVsbG8=", 64).expect("ok"), b"hello");
    }

    #[test]
    fn decode_base64_rejects_garbage_and_oversize() {
        let err = decode_base64("не base64!!", 64).expect_err("must fail");
        assert!(matches!(err, WriteError::BadBase64(_)), "got {err:?}");

        // "aGVsbG8=" это 5 байт; лимит 4 — отказ.
        let err = decode_base64("aGVsbG8=", 4).expect_err("must fail");
        assert!(matches!(err, WriteError::AttachmentTooLarge { size: 5, limit: 4 }), "got {err:?}");
    }

    /// Схема проверяется отдельно от скачивания именно затем, чтобы её можно
    /// было покрыть без TLS: wiremock отдаёт http, а прод-правило — https.
    #[test]
    fn validate_attachment_url_accepts_only_https() {
        assert!(validate_attachment_url("https://example.com/a.pdf").is_ok());

        let err = validate_attachment_url("http://example.com/a.pdf").expect_err("must fail");
        assert!(matches!(err, WriteError::BadUrl(_)), "got {err:?}");
        assert!(err.to_string().contains("https"), "сообщение должно объяснять причину");

        assert!(validate_attachment_url("file:///etc/passwd").is_err());
        assert!(validate_attachment_url("не url").is_err());
    }

    #[tokio::test]
    async fn fetch_bytes_reads_a_body_within_the_limit() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/a.txt"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string("hello"))
            .mount(&server)
            .await;

        let url = Url::parse(&format!("{}/a.txt", server.uri())).expect("url");
        let bytes = fetch_bytes(&Client::new(), url, 64).await.expect("ok");
        assert_eq!(bytes, b"hello");
    }

    /// Content-Length режет до чтения тела.
    #[tokio::test]
    async fn fetch_bytes_rejects_by_content_length() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string("hello world"))
            .mount(&server)
            .await;

        let url = Url::parse(&format!("{}/big", server.uri())).expect("url");
        let err = fetch_bytes(&Client::new(), url, 4).await.expect_err("must fail");
        assert!(matches!(err, WriteError::AttachmentTooLarge { .. }), "got {err:?}");
    }

    /// А счётчик по ходу чтения — когда Content-Length соврал или его нет.
    /// Без этой второй проверки заголовку можно было бы просто не верить.
    #[tokio::test]
    async fn fetch_bytes_rejects_a_lying_content_length() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_string("hello world")
                    .insert_header("content-length", "2"),
            )
            .mount(&server)
            .await;

        let url = Url::parse(&format!("{}/liar", server.uri())).expect("url");
        let err = fetch_bytes(&Client::new(), url, 4).await.expect_err("must fail");
        assert!(
            matches!(err, WriteError::AttachmentTooLarge { .. } | WriteError::Fetch(_)),
            "врущий Content-Length обязан кончиться отказом, а не тихой усечённой загрузкой: {err:?}"
        );
    }

    /// Клиент здесь собран без автоследования (только `Policy::none()`, без
    /// `https_only` — его нарочно нет, чтобы 3xx долетал до fetch_bytes как
    /// есть на голом http-моке wiremock). Изолирует ИМЕННО явную проверку
    /// статуса внутри fetch_bytes: она обязана отказать сама, а не отдать
    /// пустой файл как "успех", потому что `error_for_status()` не считает
    /// 3xx ошибкой.
    #[tokio::test]
    async fn fetch_bytes_rejects_a_redirect() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(
                wiremock::ResponseTemplate::new(302)
                    .insert_header("location", "http://169.254.169.254/"),
            )
            .mount(&server)
            .await;

        let no_redirect = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("client builds");
        let url = Url::parse(&format!("{}/redirect", server.uri())).expect("url");
        let err = fetch_bytes(&no_redirect, url, 64).await.expect_err("must fail");
        assert!(matches!(err, WriteError::BadUrl(_)), "got {err:?}");
    }

    /// Та же атака, но через клиент, который реально уходит в прод:
    /// `Youtrack::new(...).attachment_http`, а не отдельно собранный в тесте
    /// `Client::builder()...` из теста выше. Тот тест доказывает только, что
    /// у `fetch_bytes` есть явная проверка статуса; этот — что клиент,
    /// который `Youtrack::new` реально строит, эту проверку не обходит.
    ///
    /// wiremock отдаёт только http, а прод-клиент теперь ещё собран с
    /// `https_only(true)` (защита в глубину, см. `Youtrack::new`) — он
    /// откажет ДО отправки запроса на голый http-адрес, а не на самом
    /// редиректе. Путь другой, итог тот же: если из `Youtrack::new` пропадёт
    /// и `Policy::none()`, и `https_only(true)` одновременно, тест это не
    /// поймает — но по отдельности каждый из двух держит его красным.
    #[tokio::test]
    async fn attachment_http_from_youtrack_new_refuses_the_redirect_scenario() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(
                wiremock::ResponseTemplate::new(302)
                    .insert_header("location", "http://169.254.169.254/"),
            )
            .mount(&server)
            .await;

        let yt = Youtrack::new(server.uri()).expect("builds");
        let url = Url::parse(&format!("{}/redirect", server.uri())).expect("url");
        let err = fetch_bytes(&yt.attachment_http, url, 64).await.expect_err("must fail");
        assert!(matches!(err, WriteError::BadUrl(_) | WriteError::Fetch(_)), "got {err:?}");
    }
}
