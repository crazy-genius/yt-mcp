use wiremock::matchers::{body_partial_json, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};
use yt_client::{Youtrack, YoutrackService};

async fn service_for(base: &str) -> YoutrackService {
    Youtrack::new(base).expect("youtrack builds").as_user("t").expect("service builds")
}

/// `text` входит в дефолт списка — в отличие от `description` у задач: текст
/// комментария и есть смысл вызова, без него list вернёт пустые оболочки.
const COMMENT_FIELDS: &str = "$type,id,text,created,updated,author(login,fullName)";

fn comments_body() -> serde_json::Value {
    serde_json::json!([
        { "$type": "IssueComment", "id": "4-1", "text": "первый",
          "author": { "$type": "User", "login": "ivan", "fullName": "Иван" } },
        { "$type": "IssueComment", "id": "4-2", "text": "второй" },
    ])
}

#[tokio::test]
async fn list_comments_uses_comment_fields_and_the_default_page() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/issues/DEMO-1/comments"))
        .and(query_param("fields", COMMENT_FIELDS))
        .and(query_param("$top", "50"))
        .and(query_param("$skip", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(comments_body()))
        .mount(&server)
        .await;

    let comments = service_for(&server.uri()).await.list_comments("DEMO-1", None, None).await;
    let comments = comments.expect("ok");
    assert_eq!(comments.len(), 2);
    assert_eq!(comments[0].text.as_deref(), Some("первый"));
}

/// `top` приезжает от LLM: 0 дал бы бесполезную пустую страницу, а 100000 —
/// вывалил бы в контекст весь тред. Зажимаем в 1..=200.
#[tokio::test]
async fn list_comments_clamps_the_page_size_from_below() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/issues/DEMO-1/comments"))
        .and(query_param("$top", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&server)
        .await;

    service_for(&server.uri()).await.list_comments("DEMO-1", Some(0), None).await.expect("ok");
}

#[tokio::test]
async fn list_comments_clamps_the_page_size_from_above() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/issues/DEMO-1/comments"))
        .and(query_param("$top", "200"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&server)
        .await;

    service_for(&server.uri()).await.list_comments("DEMO-1", Some(9999), None).await.expect("ok");
}

#[tokio::test]
async fn update_comment_posts_the_text_to_the_comment_path() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/issues/DEMO-1/comments/4-1"))
        .and(query_param("fields", COMMENT_FIELDS))
        .and(body_partial_json(serde_json::json!({ "text": "исправлено" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "$type": "IssueComment", "id": "4-1", "text": "исправлено"
        })))
        .mount(&server)
        .await;

    let updated = service_for(&server.uri())
        .await
        .update_comment("DEMO-1", "4-1", "исправлено")
        .await
        .expect("ok");
    assert_eq!(updated.text.as_deref(), Some("исправлено"));
}

#[tokio::test]
async fn delete_comment_hits_the_comment_path() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/issues/DEMO-1/comments/4-1"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    service_for(&server.uri()).await.delete_comment("DEMO-1", "4-1").await.expect("ok");
}

#[tokio::test]
async fn list_article_comments_uses_the_articles_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/articles/DEMO-A-4/comments"))
        .and(query_param("fields", COMMENT_FIELDS))
        .and(query_param("$top", "50"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "$type": "ArticleComment", "id": "5-1", "text": "по статье" }
        ])))
        .mount(&server)
        .await;

    let comments =
        service_for(&server.uri()).await.list_article_comments("DEMO-A-4", None, None).await;
    assert_eq!(comments.expect("ok")[0].text.as_deref(), Some("по статье"));
}

#[tokio::test]
async fn update_article_comment_posts_the_text() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/articles/DEMO-A-4/comments/5-1"))
        .and(body_partial_json(serde_json::json!({ "text": "правка" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "$type": "ArticleComment", "id": "5-1", "text": "правка"
        })))
        .mount(&server)
        .await;

    let updated = service_for(&server.uri())
        .await
        .update_article_comment("DEMO-A-4", "5-1", "правка")
        .await
        .expect("ok");
    assert_eq!(updated.text.as_deref(), Some("правка"));
}

#[tokio::test]
async fn delete_article_comment_hits_the_comment_path() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/articles/DEMO-A-4/comments/5-1"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    service_for(&server.uri()).await.delete_article_comment("DEMO-A-4", "5-1").await.expect("ok");
}
