use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use yt_client::{Youtrack, YoutrackService};

async fn service_for(base: &str) -> YoutrackService {
    Youtrack::new(base).expect("youtrack builds").as_user("t").expect("service builds")
}

#[tokio::test]
async fn apply_command_posts_query_and_issue() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/commands"))
        .and(body_partial_json(serde_json::json!({
            "query": "state Fixed",
            "issues": [{"idReadable": "DEMO-1"}]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
        .mount(&server)
        .await;
    service_for(&server.uri())
        .await
        .apply_command("DEMO-1", "state Fixed", None)
        .await
        .expect("ok");
}

#[tokio::test]
async fn create_issue_resolves_project_id_and_posts() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/admin/projects"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"$type":"Project","id":"0-7","shortName":"DEMO","name":"Demo"}
        ])))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/issues"))
        .and(body_partial_json(serde_json::json!({
            "project": {"id": "0-7"}, "summary": "New task"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "$type":"Issue","idReadable":"DEMO-42","summary":"New task"
        })))
        .mount(&server)
        .await;
    let issue =
        service_for(&server.uri()).await.create_issue("demo", "New task", None).await.expect("ok"); // регистр!
    assert_eq!(issue.id_readable.as_deref(), Some("DEMO-42"));
}

#[tokio::test]
async fn add_comment_posts_text() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/issues/DEMO-1/comments"))
        .and(body_partial_json(serde_json::json!({"text": "hello"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "$type":"IssueComment","id":"c1"
        })))
        .mount(&server)
        .await;
    service_for(&server.uri()).await.add_comment("DEMO-1", "hello").await.expect("ok");
}

#[tokio::test]
async fn create_issue_unknown_project_lists_available() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/admin/projects"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"$type":"Project","id":"0-7","shortName":"DEMO","name":"Demo"}
        ])))
        .mount(&server)
        .await;
    let err = service_for(&server.uri())
        .await
        .create_issue("NOPE", "x", None)
        .await
        .expect_err("must fail");
    assert!(err.to_string().contains("DEMO"));
}
