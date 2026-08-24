use wiremock::matchers::{body_partial_json, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};
use yt_client::{Youtrack, YoutrackService};

async fn service_for(base: &str) -> YoutrackService {
    Youtrack::new(base).expect("youtrack builds").as_user("t").expect("service builds")
}

const CARD_FIELDS: &str = "$type,idReadable,summary,updated,hasChildren,\
project(shortName,name),parentArticle(idReadable,summary),content,created,\
reporter(login,fullName),childArticles(idReadable,summary)";

#[tokio::test]
async fn find_article_uses_full_card_fields() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/articles/DEMO-A-4"))
        .and(query_param("fields", CARD_FIELDS))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "$type": "Article", "idReadable": "DEMO-A-4",
            "summary": "Deploy runbook", "content": "kubectl apply"
        })))
        .mount(&server)
        .await;

    let article =
        service_for(&server.uri()).await.find_article("DEMO-A-4", None).await.expect("ok");
    assert_eq!(article.content.as_deref(), Some("kubectl apply"));
}

fn articles_body() -> serde_json::Value {
    serde_json::json!([
        { "$type": "Article", "idReadable": "DEMO-A-1", "summary": "Deploy runbook",
          "content": "Раскатка через kubectl" },
        { "$type": "Article", "idReadable": "DEMO-A-2", "summary": "Onboarding",
          "content": "Как завести доступы" },
    ])
}

#[tokio::test]
async fn search_articles_matches_all_terms_and_strips_content() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/articles"))
        .and(query_param("$top", "500"))
        .respond_with(ResponseTemplate::new(200).set_body_json(articles_body()))
        .mount(&server)
        .await;

    let svc = service_for(&server.uri()).await;

    // Слова ищутся и в заголовке, и в теле, без учёта регистра.
    let page = svc.search_articles("deploy КУБЕКТЛ", None, None, None).await.expect("ok");
    assert!(page.articles.is_empty(), "лишнее слово не должно матчиться");

    let page = svc.search_articles("Deploy KUBECTL", None, None, None).await.expect("ok");
    assert_eq!(page.articles.len(), 1);
    assert_eq!(page.articles[0].id_readable.as_deref(), Some("DEMO-A-1"));
    assert_eq!(page.articles[0].content, None, "тело выдаётся только через find_article");

    // Пустой запрос = просто список; окно сканирования не переполнено.
    let page = svc.search_articles("", None, None, None).await.expect("ok");
    assert_eq!(page.articles.len(), 2);
    assert_eq!(page.matched, 2);
    assert_eq!(page.next_skip, None);
    assert!(!page.scan_truncated);
}

#[tokio::test]
async fn search_articles_pages_matches_and_reports_next_skip() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/articles"))
        .respond_with(ResponseTemplate::new(200).set_body_json(articles_body()))
        .mount(&server)
        .await;

    let svc = service_for(&server.uri()).await;

    let first = svc.search_articles("", None, Some(1), None).await.expect("ok");
    assert_eq!(first.articles.len(), 1);
    assert_eq!(first.articles[0].id_readable.as_deref(), Some("DEMO-A-1"));
    assert_eq!(first.matched, 2, "matched — по всем совпадениям, не по странице");
    assert_eq!(first.next_skip, Some(1));

    let second = svc.search_articles("", None, Some(1), first.next_skip).await.expect("ok");
    assert_eq!(second.articles[0].id_readable.as_deref(), Some("DEMO-A-2"));
    assert_eq!(second.next_skip, None, "последняя страница");

    // limit=0 от модели не должен вырождаться в вечно пустую страницу.
    assert_eq!(svc.search_articles("", None, Some(0), None).await.expect("ok").articles.len(), 1);
    // skip за пределами выдачи — пустая страница без next_skip.
    let beyond = svc.search_articles("", None, None, Some(99)).await.expect("ok");
    assert!(beyond.articles.is_empty());
    assert_eq!(beyond.next_skip, None);
}

#[tokio::test]
async fn search_articles_scoped_by_project_resolves_short_name() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/admin/projects"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"$type":"Project","id":"0-7","shortName":"DEMO","name":"Demo"}
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/admin/projects/0-7/articles"))
        .respond_with(ResponseTemplate::new(200).set_body_json(articles_body()))
        .mount(&server)
        .await;

    let page = service_for(&server.uri())
        .await
        .search_articles("onboarding", Some("demo"), None, None)
        .await
        .expect("ok"); // регистр!
    assert_eq!(page.articles.len(), 1);
    assert_eq!(page.articles[0].id_readable.as_deref(), Some("DEMO-A-2"));
}

#[tokio::test]
async fn create_article_posts_project_and_parent() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/admin/projects"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"$type":"Project","id":"0-7","shortName":"DEMO","name":"Demo"}
        ])))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/articles"))
        .and(body_partial_json(serde_json::json!({
            "project": {"id": "0-7"},
            "summary": "Runbook",
            "content": "text",
            "parentArticle": {"idReadable": "DEMO-A-1"}
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "$type":"Article","idReadable":"DEMO-A-9","summary":"Runbook"
        })))
        .mount(&server)
        .await;

    let article = service_for(&server.uri())
        .await
        .create_article("demo", "Runbook", Some("text"), Some("DEMO-A-1"))
        .await
        .expect("ok");
    assert_eq!(article.id_readable.as_deref(), Some("DEMO-A-9"));
}

#[tokio::test]
async fn update_article_sends_only_given_fields() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/articles/DEMO-A-4"))
        .and(body_partial_json(serde_json::json!({"content": "new text"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "$type":"Article","idReadable":"DEMO-A-4"
        })))
        .mount(&server)
        .await;

    let article = service_for(&server.uri())
        .await
        .update_article("DEMO-A-4", None, Some("new text"))
        .await
        .expect("ok");
    assert_eq!(article.id_readable.as_deref(), Some("DEMO-A-4"));
}

const LIST_FIELDS: &str = "$type,idReadable,summary,updated,hasChildren,\
project(shortName,name),parentArticle(idReadable,summary)";

#[tokio::test]
async fn list_articles_page_passes_top_and_skip() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/articles"))
        .and(query_param("fields", LIST_FIELDS))
        .and(query_param("$top", "200"))
        .and(query_param("$skip", "400"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"$type": "Article", "idReadable": "DEMO-A-7", "summary": "s", "updated": 1700000000000i64}
        ])))
        .mount(&server)
        .await;

    let page = service_for(&server.uri()).await.list_articles_page(200, 400).await.expect("ok");
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].id_readable.as_deref(), Some("DEMO-A-7"));
    assert_eq!(page[0].updated, Some(1700000000000));
    assert!(page[0].content.is_none(), "листинг не должен тащить content");
}

#[tokio::test]
async fn list_project_articles_page_resolves_short_name() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/admin/projects"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"$type": "Project", "id": "0-5", "shortName": "BSA"}
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/admin/projects/0-5/articles"))
        .and(query_param("fields", LIST_FIELDS))
        .and(query_param("$top", "200"))
        .and(query_param("$skip", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"$type": "Article", "idReadable": "BSA-A-313", "summary": "s", "updated": 1i64}
        ])))
        .mount(&server)
        .await;

    let page = service_for(&server.uri())
        .await
        .list_project_articles_page("BSA", 200, 0)
        .await
        .expect("ok");
    assert_eq!(page[0].id_readable.as_deref(), Some("BSA-A-313"));
    assert!(page[0].content.is_none(), "листинг не должен тащить тело статьи");
}

#[tokio::test]
async fn add_article_comment_posts_text() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/articles/DEMO-A-4/comments"))
        .and(body_partial_json(serde_json::json!({"text": "hello"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "$type":"ArticleComment","id":"c1"
        })))
        .mount(&server)
        .await;

    service_for(&server.uri()).await.add_article_comment("DEMO-A-4", "hello").await.expect("ok");
}
