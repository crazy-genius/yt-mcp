use wiremock::matchers::{body_string_contains, header_exists, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};
use yt_client::{UploadSource, Youtrack, YoutrackService};

async fn service_for(base: &str) -> YoutrackService {
    Youtrack::new(base).expect("youtrack builds").as_user("t").expect("service builds")
}

const LIST_FIELDS: &str = "$type,id,name,size,mimeType,created,author(login,fullName),url";
const META_FIELDS: &str = "$type,id,name,size,mimeType";
const CONTENT_FIELDS: &str = "$type,id,name,size,mimeType,base64Content";

#[tokio::test]
async fn list_attachments_asks_for_metadata_only() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/issues/DEMO-1/attachments"))
        .and(query_param("fields", LIST_FIELDS))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "$type": "IssueAttachment", "id": "8-1", "name": "runbook.md",
              "size": 120, "mimeType": "text/markdown" }
        ])))
        .mount(&server)
        .await;

    let list = service_for(&server.uri()).await.list_attachments("DEMO-1").await.expect("ok");
    assert_eq!(list[0].name.as_deref(), Some("runbook.md"));
    assert_eq!(list[0].base64_content, None, "содержимое едет только через read");
}

#[tokio::test]
async fn attachment_meta_asks_for_the_cheap_fields() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/issues/DEMO-1/attachments/8-1"))
        .and(query_param("fields", META_FIELDS))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "$type": "IssueAttachment", "id": "8-1", "name": "big.pdf",
            "size": 9999, "mimeType": "application/pdf"
        })))
        .mount(&server)
        .await;

    let meta = service_for(&server.uri()).await.attachment_meta("DEMO-1", "8-1").await.expect("ok");
    assert_eq!(meta.mime_type.as_deref(), Some("application/pdf"));
    assert_eq!(meta.size, Some(9999));
}

/// YouTrack отдаёт содержимое data-URI — префикс должен быть срезан, а не
/// уехать в первые байты файла.
#[tokio::test]
async fn read_attachment_decodes_a_data_uri_payload() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/issues/DEMO-1/attachments/8-1"))
        .and(query_param("fields", CONTENT_FIELDS))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "$type": "IssueAttachment", "id": "8-1", "name": "hi.txt",
            "size": 5, "mimeType": "text/plain",
            "base64Content": "data:text/plain;base64,aGVsbG8="
        })))
        .mount(&server)
        .await;

    let content =
        service_for(&server.uri()).await.read_attachment("DEMO-1", "8-1").await.expect("ok");
    assert_eq!(content.name, "hi.txt");
    assert_eq!(content.mime_type.as_deref(), Some("text/plain"));
    assert_eq!(content.bytes, b"hello");
    assert_eq!(content.base64, "aGVsbG8=", "base64 без префикса — для ContentBlock::image");
}

/// Отсутствие `base64Content` (внешнее хранилище, права, поменялся набор
/// полей) — это отказ, а не пустой файл: молча вернуть `bytes: vec![]` для
/// такого случая неотличимо от честного нулевого файла.
#[tokio::test]
async fn read_attachment_errors_when_base64_content_is_missing() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/issues/DEMO-1/attachments/8-1"))
        .and(query_param("fields", CONTENT_FIELDS))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "$type": "IssueAttachment", "id": "8-1", "name": "hi.txt",
            "size": 5, "mimeType": "text/plain"
        })))
        .mount(&server)
        .await;

    let err = service_for(&server.uri())
        .await
        .read_attachment("DEMO-1", "8-1")
        .await
        .expect_err("must fail");
    assert!(err.to_string().contains("base64Content"), "got {err}");
}

#[tokio::test]
async fn upload_attachment_posts_multipart_with_the_file_name() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/issues/DEMO-1/attachments"))
        .and(query_param("fields", LIST_FIELDS))
        .and(header_exists("content-type"))
        .and(body_string_contains("report.txt"))
        .and(body_string_contains("hello"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "$type": "IssueAttachment", "id": "8-9", "name": "report.txt" }
        ])))
        .mount(&server)
        .await;

    let created = service_for(&server.uri())
        .await
        .upload_attachment("DEMO-1", "report.txt", UploadSource::Base64("aGVsbG8=".to_owned()))
        .await
        .expect("ok");
    assert_eq!(created[0].id.as_deref(), Some("8-9"));
}

/// Канал `url` отбивает не-https ещё до всякой сети.
#[tokio::test]
async fn upload_attachment_rejects_a_plain_http_url() {
    let server = MockServer::start().await;
    let err = service_for(&server.uri())
        .await
        .upload_attachment("DEMO-1", "a.pdf", UploadSource::Url("http://example.com/a".to_owned()))
        .await
        .expect_err("must fail");
    assert!(err.to_string().contains("https"), "got {err}");
}

#[tokio::test]
async fn delete_attachment_hits_the_attachment_path() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/issues/DEMO-1/attachments/8-1"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    service_for(&server.uri()).await.delete_attachment("DEMO-1", "8-1").await.expect("ok");
}

#[tokio::test]
async fn article_attachments_use_the_articles_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/articles/DEMO-A-4/attachments"))
        .and(query_param("fields", LIST_FIELDS))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "$type": "ArticleAttachment", "id": "9-1", "name": "scheme.png",
              "mimeType": "image/png" }
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/articles/DEMO-A-4/attachments/9-1"))
        .and(query_param("fields", META_FIELDS))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "$type": "ArticleAttachment", "id": "9-1", "name": "scheme.png",
            "size": 3, "mimeType": "image/png"
        })))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/articles/DEMO-A-4/attachments/9-1"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let svc = service_for(&server.uri()).await;
    let list = svc.list_article_attachments("DEMO-A-4").await.expect("ok");
    assert_eq!(list[0].name.as_deref(), Some("scheme.png"));

    let meta = svc.article_attachment_meta("DEMO-A-4", "9-1").await.expect("ok");
    assert_eq!(meta.mime_type.as_deref(), Some("image/png"));

    svc.delete_article_attachment("DEMO-A-4", "9-1").await.expect("ok");
}

#[tokio::test]
async fn read_article_attachment_decodes_the_content() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/articles/DEMO-A-4/attachments/9-1"))
        .and(query_param("fields", CONTENT_FIELDS))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "$type": "ArticleAttachment", "id": "9-1", "name": "notes.txt",
            "size": 5, "mimeType": "text/plain", "base64Content": "aGVsbG8="
        })))
        .mount(&server)
        .await;

    let content = service_for(&server.uri())
        .await
        .read_article_attachment("DEMO-A-4", "9-1")
        .await
        .expect("ok");
    assert_eq!(content.bytes, b"hello");
}

#[tokio::test]
async fn upload_article_attachment_posts_multipart() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/articles/DEMO-A-4/attachments"))
        .and(query_param("fields", LIST_FIELDS))
        .and(body_string_contains("notes.txt"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "$type": "ArticleAttachment", "id": "9-9", "name": "notes.txt" }
        ])))
        .mount(&server)
        .await;

    let created = service_for(&server.uri())
        .await
        .upload_article_attachment(
            "DEMO-A-4",
            "notes.txt",
            UploadSource::Base64("aGVsbG8=".to_owned()),
        )
        .await
        .expect("ok");
    assert_eq!(created[0].id.as_deref(), Some("9-9"));
}
