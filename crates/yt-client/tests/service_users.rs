use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};
use yt_client::{Youtrack, YoutrackService};

async fn service_for(base: &str) -> YoutrackService {
    Youtrack::new(base).expect("youtrack builds").as_user("t").expect("service builds")
}

const USER_FIELDS: &str = "$type,id,login,fullName,email,banned";

#[tokio::test]
async fn whoami_reads_the_me_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/users/me"))
        .and(query_param("fields", USER_FIELDS))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "$type": "Me", "id": "1-1", "login": "ivan", "fullName": "Иван Петров"
        })))
        .mount(&server)
        .await;

    let me = service_for(&server.uri()).await.whoami().await.expect("ok");
    assert_eq!(me.login(), Some("ivan"));
}

fn users_body() -> serde_json::Value {
    serde_json::json!([
        { "$type": "User", "id": "1-1", "login": "ivan",
          "fullName": "Иван Петров", "email": "ivan@example.com", "banned": false },
        { "$type": "User", "id": "1-2", "login": "maria",
          "fullName": "Мария Сидорова", "email": "maria@example.com", "banned": false },
        { "$type": "User", "id": "1-3", "login": "oldtimer",
          "fullName": "Пётр Уволенный", "email": "petr@example.com", "banned": true },
    ])
}

#[tokio::test]
async fn search_users_matches_login_full_name_and_email() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/users"))
        .and(query_param("fields", USER_FIELDS))
        .and(query_param("$top", "500"))
        .respond_with(ResponseTemplate::new(200).set_body_json(users_body()))
        .mount(&server)
        .await;

    let svc = service_for(&server.uri()).await;

    // По логину.
    let page = svc.search_users("ivan", None, None).await.expect("ok");
    assert_eq!(page.matched, 1);
    assert_eq!(page.users[0].login(), Some("ivan"));

    // По fullName, без учёта регистра.
    let page = svc.search_users("СИДОРОВА", None, None).await.expect("ok");
    assert_eq!(page.users.len(), 1);
    assert_eq!(page.users[0].login(), Some("maria"));

    // По email.
    let page = svc.search_users("maria@example.com", None, None).await.expect("ok");
    assert_eq!(page.users.len(), 1);

    // Все слова обязательны, как в search_articles.
    let page = svc.search_users("Иван Сидорова", None, None).await.expect("ok");
    assert!(page.users.is_empty());
}

/// Подставлять уволенного в меншн или в assignee бессмысленно, поэтому
/// забаненные не попадают в выдачу вообще — даже при пустом запросе.
#[tokio::test]
async fn search_users_drops_banned_accounts() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/users"))
        .respond_with(ResponseTemplate::new(200).set_body_json(users_body()))
        .mount(&server)
        .await;

    let page = service_for(&server.uri()).await.search_users("", None, None).await.expect("ok");
    assert_eq!(page.matched, 2, "забаненный не должен попасть в выдачу");
    assert!(page.users.iter().all(|u| u.login() != Some("oldtimer")));
    assert!(!page.scan_truncated);
    assert_eq!(page.next_skip, None);
}

#[tokio::test]
async fn search_users_pages_matches_and_reports_next_skip() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/users"))
        .respond_with(ResponseTemplate::new(200).set_body_json(users_body()))
        .mount(&server)
        .await;

    let svc = service_for(&server.uri()).await;
    let page = svc.search_users("", Some(1), None).await.expect("ok");
    assert_eq!(page.users.len(), 1);
    assert_eq!(page.matched, 2);
    assert_eq!(page.next_skip, Some(1));

    let page = svc.search_users("", Some(1), Some(1)).await.expect("ok");
    assert_eq!(page.users.len(), 1);
    assert_eq!(page.next_skip, None);
}

/// Ровно упёрлись в потолок скана — значит за окном могут быть ещё люди, и
/// модель обязана об этом узнать, а не считать выдачу полной.
#[tokio::test]
async fn search_users_reports_a_truncated_scan() {
    let server = MockServer::start().await;
    let many: Vec<serde_json::Value> = (0..500)
        .map(|i| {
            serde_json::json!({
                "$type": "User", "id": format!("1-{i}"),
                "login": format!("user{i}"), "banned": false
            })
        })
        .collect();
    Mock::given(method("GET"))
        .and(path("/api/users"))
        .respond_with(ResponseTemplate::new(200).set_body_json(many))
        .mount(&server)
        .await;

    let page = service_for(&server.uri()).await.search_users("", None, None).await.expect("ok");
    assert!(page.scan_truncated, "потолок скана должен быть виден наружу");
}
