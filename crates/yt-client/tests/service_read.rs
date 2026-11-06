use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};
use yt_client::{Youtrack, YoutrackService};

async fn service_for(base: &str) -> YoutrackService {
    Youtrack::new(base).expect("youtrack builds").as_user("test-token").expect("service builds")
}

#[tokio::test]
async fn find_issue_returns_parsed_issue() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/issues/DEMO-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "$type": "Issue",
            "idReadable": "DEMO-1",
            "summary": "Fix the thing"
        })))
        .mount(&server)
        .await;

    let svc = service_for(&server.uri()).await;
    let issue = svc.find_issue("DEMO-1", None).await.expect("ok");
    assert_eq!(issue.id_readable.as_deref(), Some("DEMO-1"));
    assert_eq!(issue.summary.as_deref(), Some("Fix the thing"));
}

#[tokio::test]
async fn my_issues_filters_by_assignee_and_unresolved() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/issues"))
        .and(query_param("query", "assignee: {alice} #Unresolved"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "$type": "Issue", "idReadable": "DEMO-2", "summary": "Task A" }
        ])))
        .mount(&server)
        .await;

    let svc = service_for(&server.uri()).await;
    let page = svc.my_issues("alice", None, None).await.expect("ok");
    let issues = page.issues;
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].id_readable.as_deref(), Some("DEMO-2"));
}

/// Без скобок `assignee: Ivan Petrov` YouTrack читает как «assignee Ivan и
/// где-то в тексте Petrov» — без ошибки, просто не те задачи.
#[tokio::test]
async fn my_issues_keeps_a_value_with_spaces_in_one_piece() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/issues"))
        .and(query_param("query", "assignee: {Ivan Petrov} #Unresolved"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .expect(1)
        .mount(&server)
        .await;

    service_for(&server.uri()).await.my_issues("Ivan Petrov", None, None).await.expect("ok");
}

#[tokio::test]
async fn search_issues_asks_one_extra_and_reports_next_skip() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/issues"))
        .and(query_param("$top", "3"))
        .and(query_param("$skip", "10"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "$type": "Issue", "idReadable": "D-1" },
            { "$type": "Issue", "idReadable": "D-2" },
            { "$type": "Issue", "idReadable": "D-3" }
        ])))
        .expect(1)
        .mount(&server)
        .await;

    let svc = service_for(&server.uri()).await;
    let page = svc.search_issues("#Unresolved", None, Some(2), Some(10)).await.unwrap();
    assert_eq!(page.issues.len(), 2);
    assert_eq!(page.next_skip, Some(12));
}

#[tokio::test]
async fn the_last_page_has_no_next_skip_and_top_is_clamped() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/issues"))
        .and(query_param("$top", "101"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "$type": "Issue" }, { "$type": "Issue" }, { "$type": "Issue" },
            { "$type": "Issue" }, { "$type": "Issue" }
        ])))
        .expect(1)
        .mount(&server)
        .await;

    let svc = service_for(&server.uri()).await;
    let page = svc.search_issues("x", None, Some(500), None).await.unwrap();
    assert_eq!(page.issues.len(), 5);
    assert_eq!(page.next_skip, None);
}

#[tokio::test]
async fn exactly_top_items_means_no_next_skip() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/issues"))
        .and(query_param("$top", "3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "$type": "Issue" }, { "$type": "Issue" }
        ])))
        .expect(1)
        .mount(&server)
        .await;

    let svc = service_for(&server.uri()).await;
    // Огромный skip не должен переполнять skip + top.
    let page = svc.search_issues("x", None, Some(2), Some(i64::MAX)).await.unwrap();
    assert_eq!(page.issues.len(), 2);
    assert_eq!(page.next_skip, None);
}

const LIST_FIELDS: &str = "$type,idReadable,summary,created,updated,resolved,\
project(shortName,name),customFields(name,value(name,login,fullName))";
const FULL_FIELDS: &str = "$type,idReadable,summary,description,created,updated,resolved,\
project(shortName,name),reporter(login,fullName),customFields(name,value(name,login,fullName))";

#[tokio::test]
async fn default_search_uses_list_fields_without_description() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/issues"))
        .and(query_param("fields", LIST_FIELDS))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&server)
        .await;

    let svc = service_for(&server.uri()).await;
    let _ = svc.search_issues("assignee: me", None, None, None).await.expect("ok");
}

#[tokio::test]
async fn default_find_issue_uses_full_card_fields() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/issues/DEMO-9"))
        .and(query_param("fields", FULL_FIELDS))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "$type": "Issue", "idReadable": "DEMO-9", "summary": "x"
        })))
        .mount(&server)
        .await;

    let svc = service_for(&server.uri()).await;
    let _ = svc.find_issue("DEMO-9", None).await.expect("ok");
}

#[tokio::test]
async fn explicit_fields_override_is_sent_verbatim() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/issues"))
        .and(query_param("fields", "$type,idReadable,summary"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&server)
        .await;

    let svc = service_for(&server.uri()).await;
    let fields = Some(vec!["idReadable".to_owned(), "summary".to_owned()]);
    let _ = svc.search_issues("assignee: me", fields, None, None).await.expect("ok");
}

#[tokio::test]
async fn as_user_sends_the_callers_bearer_token() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/issues/DEMO-1"))
        .and(wiremock::matchers::header("authorization", "Bearer user-42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "$type": "Issue",
            "idReadable": "DEMO-1",
            "summary": "Fix the thing"
        })))
        .mount(&server)
        .await;

    let yt = yt_client::Youtrack::new(server.uri()).expect("youtrack builds");
    let issue = yt
        .as_user("user-42")
        .expect("service builds")
        .find_issue("DEMO-1", None)
        .await
        .expect("ok");
    assert_eq!(issue.id_readable.as_deref(), Some("DEMO-1"));
}

/// Пустой список полей от LLM — это не «верни ничего», а «я не выбрал».
/// Отдаём дефолтную карточку, иначе YouTrack вернёт один $type.
#[tokio::test]
async fn empty_fields_falls_back_to_the_card_default() {
    // Тот же набор, что у `find_issue` без параметров, — константа одна,
    // иначе копия разъедется с дефолтом при первой же правке полей.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/issues/DEMO-1"))
        .and(query_param("fields", FULL_FIELDS))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "$type": "Issue", "idReadable": "DEMO-1", "summary": "Fix the thing"
        })))
        .mount(&server)
        .await;

    let issue =
        service_for(&server.uri()).await.find_issue("DEMO-1", Some(vec![])).await.expect("ok");
    assert_eq!(issue.id_readable.as_deref(), Some("DEMO-1"));
}
