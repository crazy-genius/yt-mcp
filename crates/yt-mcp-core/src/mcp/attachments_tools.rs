use crate::mcp::{
    DeleteArticleAttachmentArgs, DeleteAttachmentArgs, ListArticleAttachmentsArgs,
    ListAttachmentsArgs, ReadArticleAttachmentArgs, ReadAttachmentArgs,
    UploadArticleAttachmentArgs, UploadAttachmentArgs, UserToken, YoutrackMCPServer,
};
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::ContentBlock;
use rmcp::{ErrorData as McpError, model::CallToolResult, tool, tool_router};
use yt_client::{AttachmentContent, UploadSource};

/// Лимиты выдачи МОДЕЛИ, а не хранения: текст — столько, сколько она реально
/// прочитает; картинка — столько, сколько влезет в контекст изображением.
/// Потолок самого вложения живёт в yt-client (`MAX_ATTACHMENT_BYTES`).
const MAX_READ_TEXT_BYTES: i64 = 256 * 1024;
const MAX_READ_IMAGE_BYTES: i64 = 5 * 1024 * 1024;

/// Чем отдавать содержимое вложения.
#[derive(Debug)]
enum ReadPlan {
    Text,
    Image,
}

/// Решение по mime и размеру — ДО скачивания содержимого. Метаданные стоят
/// один дешёвый запрос, а отказ на 200-мегабайтном PDF не должен стоить нам
/// этих 200 мегабайт.
///
/// `Err` — готовый текст для модели: она читает его и может поправиться,
/// например попросить пользователя открыть файл самому.
fn plan_read(mime: Option<&str>, size: Option<i64>) -> Result<ReadPlan, String> {
    // mime приходит с параметрами (`text/csv; charset=utf-8`) — они не влияют
    // на решение, а точное сравнение бы ломали.
    let full = mime.unwrap_or("application/octet-stream").to_lowercase();
    let mime = full.split(';').next().unwrap_or(&full).trim();
    let size = size.unwrap_or(0);

    let textual = mime.starts_with("text/")
        || matches!(mime, "application/json" | "application/xml" | "application/yaml")
        || mime.ends_with("+json")
        || mime.ends_with("+xml")
        || mime.ends_with("+yaml");

    if textual {
        if size > MAX_READ_TEXT_BYTES {
            return Err(format!(
                "text attachment too large: {size} bytes, limit is {MAX_READ_TEXT_BYTES}"
            ));
        }
        return Ok(ReadPlan::Text);
    }

    if mime.starts_with("image/") {
        if size > MAX_READ_IMAGE_BYTES {
            return Err(format!(
                "image attachment too large: {size} bytes, limit is {MAX_READ_IMAGE_BYTES}"
            ));
        }
        return Ok(ReadPlan::Image);
    }

    Err(format!(
        "cannot read '{mime}' ({size} bytes) as text or an image — ask the user to open it \
         in YouTrack instead"
    ))
}

/// Ровно один канал байтов на вызов. Ноль или два — это ошибка конфигурации
/// вызова, и модель должна услышать, что выбрать.
fn upload_source(
    content_base64: Option<String>,
    url: Option<String>,
) -> Result<UploadSource, String> {
    match (content_base64, url) {
        (Some(_), Some(_)) => Err("give either 'content_base64' or 'url', not both".to_owned()),
        (None, None) => Err("give the file as 'content_base64' or as an https 'url'".to_owned()),
        (Some(raw), None) => Ok(UploadSource::Base64(raw)),
        (None, Some(raw)) => Ok(UploadSource::Url(raw)),
    }
}

/// Второй, финальный лимит — уже по байтам, которые реально приехали вторым
/// (content) запросом, а не по `size` из первого (meta). `size` бывает
/// `null` (YouTrack не всегда его считает, поэтому у yt-rs он `Option`), и
/// тогда `plan_read` решал бы по `unwrap_or(0)` — то есть вообще без лимита.
fn content_block(content: AttachmentContent, plan: ReadPlan) -> Result<ContentBlock, String> {
    let limit = match plan {
        ReadPlan::Text => MAX_READ_TEXT_BYTES,
        ReadPlan::Image => MAX_READ_IMAGE_BYTES,
    };
    if content.bytes.len() as i64 > limit {
        return Err(format!("attachment is {} bytes, limit is {limit}", content.bytes.len()));
    }
    Ok(match plan {
        ReadPlan::Text => ContentBlock::text(String::from_utf8_lossy(&content.bytes).into_owned()),
        ReadPlan::Image => ContentBlock::image(
            content.base64,
            content.mime_type.unwrap_or_else(|| "application/octet-stream".to_owned()),
        ),
    })
}

#[tool_router(router = "youtrack_attachments", vis = "pub")]
impl YoutrackMCPServer {
    #[tool(
        name = "youtrack_list_attachments",
        description = "List the files attached to an issue: name, size, mime type, author and the attachment id needed by youtrack_read_attachment. Does NOT return file contents.",
        annotations(
            title = "List issue attachments",
            read_only_hint = true,
            open_world_hint = true
        )
    )]
    pub async fn list_attachments(
        &self,
        params: Parameters<ListAttachmentsArgs>,
        Extension(token): Extension<UserToken>,
    ) -> Result<CallToolResult, McpError> {
        let yt = self.as_user(&token)?;
        match yt.list_attachments(&params.0.issue).await {
            Ok(list) => Self::json_result(list),
            Err(e) => Ok(Self::tool_error(e)),
        }
    }

    #[tool(
        name = "youtrack_upload_attachment",
        description = "Attach a file to an issue. Give the bytes either as 'content_base64' (for something you produced yourself) or as an https 'url' the server downloads — exactly one of the two. Limit is 10 MiB; plain http urls are refused.",
        annotations(
            title = "Attach file to issue",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    pub async fn upload_attachment(
        &self,
        params: Parameters<UploadAttachmentArgs>,
        Extension(token): Extension<UserToken>,
    ) -> Result<CallToolResult, McpError> {
        let args = params.0;
        let source = match upload_source(args.content_base64, args.url) {
            Ok(source) => source,
            Err(msg) => return Ok(Self::tool_error(msg)),
        };
        let yt = self.as_user(&token)?;
        match yt.upload_attachment(&args.issue, &args.file_name, source).await {
            Ok(created) => Self::json_result(created),
            Err(e) => Ok(Self::tool_error(e)),
        }
    }

    #[tool(
        name = "youtrack_read_attachment",
        description = "Read the content of one issue attachment. Text files come back as text, images as an image. Anything else (PDF, docx, archives) is refused with its size and mime type — ask the user to open those in YouTrack. Get 'attachment_id' from youtrack_list_attachments.",
        annotations(
            title = "Read issue attachment",
            read_only_hint = true,
            open_world_hint = true
        )
    )]
    pub async fn read_attachment(
        &self,
        params: Parameters<ReadAttachmentArgs>,
        Extension(token): Extension<UserToken>,
    ) -> Result<CallToolResult, McpError> {
        let args = params.0;
        let yt = self.as_user(&token)?;

        let meta = match yt.attachment_meta(&args.issue, &args.attachment_id).await {
            Ok(meta) => meta,
            Err(e) => return Ok(Self::tool_error(e)),
        };
        let plan = match plan_read(meta.mime_type.as_deref(), meta.size) {
            Ok(plan) => plan,
            Err(msg) => return Ok(Self::tool_error(msg)),
        };
        match yt.read_attachment(&args.issue, &args.attachment_id).await {
            Ok(content) => match content_block(content, plan) {
                Ok(block) => Ok(CallToolResult::success(vec![block])),
                Err(msg) => Ok(Self::tool_error(msg)),
            },
            Err(e) => Ok(Self::tool_error(e)),
        }
    }

    #[tool(
        name = "youtrack_delete_attachment",
        description = "Deletes one attachment from an issue. Get 'attachment_id' from youtrack_list_attachments.",
        annotations(
            title = "Delete issue attachment",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    pub async fn delete_attachment(
        &self,
        params: Parameters<DeleteAttachmentArgs>,
        Extension(token): Extension<UserToken>,
    ) -> Result<CallToolResult, McpError> {
        let args = params.0;
        let yt = self.as_user(&token)?;
        match yt.delete_attachment(&args.issue, &args.attachment_id).await {
            Ok(()) => Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "OK: attachment {} deleted from {}",
                args.attachment_id, args.issue
            ))])),
            Err(e) => Ok(Self::tool_error(e)),
        }
    }

    #[tool(
        name = "youtrack_list_article_attachments",
        description = "List the files attached to a Knowledge Base article: name, size, mime type, author and the attachment id needed by youtrack_read_article_attachment. Does NOT return file contents.",
        annotations(
            title = "List article attachments",
            read_only_hint = true,
            open_world_hint = true
        )
    )]
    pub async fn list_article_attachments(
        &self,
        params: Parameters<ListArticleAttachmentsArgs>,
        Extension(token): Extension<UserToken>,
    ) -> Result<CallToolResult, McpError> {
        let yt = self.as_user(&token)?;
        match yt.list_article_attachments(&params.0.article).await {
            Ok(list) => Self::json_result(list),
            Err(e) => Ok(Self::tool_error(e)),
        }
    }

    #[tool(
        name = "youtrack_upload_article_attachment",
        description = "Attach a file to a Knowledge Base article. Give the bytes either as 'content_base64' or as an https 'url' the server downloads — exactly one of the two. Limit is 10 MiB; plain http urls are refused.",
        annotations(
            title = "Attach file to article",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    pub async fn upload_article_attachment(
        &self,
        params: Parameters<UploadArticleAttachmentArgs>,
        Extension(token): Extension<UserToken>,
    ) -> Result<CallToolResult, McpError> {
        let args = params.0;
        let source = match upload_source(args.content_base64, args.url) {
            Ok(source) => source,
            Err(msg) => return Ok(Self::tool_error(msg)),
        };
        let yt = self.as_user(&token)?;
        match yt.upload_article_attachment(&args.article, &args.file_name, source).await {
            Ok(created) => Self::json_result(created),
            Err(e) => Ok(Self::tool_error(e)),
        }
    }

    #[tool(
        name = "youtrack_read_article_attachment",
        description = "Read the content of one article attachment. Text files come back as text, images as an image. Anything else (PDF, docx, archives) is refused with its size and mime type. Get 'attachment_id' from youtrack_list_article_attachments.",
        annotations(
            title = "Read article attachment",
            read_only_hint = true,
            open_world_hint = true
        )
    )]
    pub async fn read_article_attachment(
        &self,
        params: Parameters<ReadArticleAttachmentArgs>,
        Extension(token): Extension<UserToken>,
    ) -> Result<CallToolResult, McpError> {
        let args = params.0;
        let yt = self.as_user(&token)?;

        let meta = match yt.article_attachment_meta(&args.article, &args.attachment_id).await {
            Ok(meta) => meta,
            Err(e) => return Ok(Self::tool_error(e)),
        };
        let plan = match plan_read(meta.mime_type.as_deref(), meta.size) {
            Ok(plan) => plan,
            Err(msg) => return Ok(Self::tool_error(msg)),
        };
        match yt.read_article_attachment(&args.article, &args.attachment_id).await {
            Ok(content) => match content_block(content, plan) {
                Ok(block) => Ok(CallToolResult::success(vec![block])),
                Err(msg) => Ok(Self::tool_error(msg)),
            },
            Err(e) => Ok(Self::tool_error(e)),
        }
    }

    #[tool(
        name = "youtrack_delete_article_attachment",
        description = "Deletes one attachment from a Knowledge Base article. Get 'attachment_id' from youtrack_list_article_attachments.",
        annotations(
            title = "Delete article attachment",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    pub async fn delete_article_attachment(
        &self,
        params: Parameters<DeleteArticleAttachmentArgs>,
        Extension(token): Extension<UserToken>,
    ) -> Result<CallToolResult, McpError> {
        let args = params.0;
        let yt = self.as_user(&token)?;
        match yt.delete_article_attachment(&args.article, &args.attachment_id).await {
            Ok(()) => Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "OK: attachment {} deleted from article {}",
                args.attachment_id, args.article
            ))])),
            Err(e) => Ok(Self::tool_error(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn textual_mimes_are_read_as_text() {
        for mime in [
            "text/plain",
            "text/markdown",
            "text/csv; charset=utf-8",
            "application/json",
            "application/xml",
            "application/vnd.api+json",
            "image/svg+xml",
        ] {
            assert!(
                matches!(plan_read(Some(mime), Some(100)), Ok(ReadPlan::Text)),
                "{mime} должен читаться текстом"
            );
        }
    }

    #[test]
    fn images_are_returned_as_an_image_block() {
        assert!(matches!(plan_read(Some("image/png"), Some(100)), Ok(ReadPlan::Image)));
        assert!(matches!(plan_read(Some("IMAGE/JPEG"), Some(100)), Ok(ReadPlan::Image)));
    }

    /// Бинарь возвращается ошибкой сознательно: отдавать модели base64 PDF —
    /// сжечь контекст ради данных, которые она всё равно не прочитает.
    /// `is_error` при этом оставляет ей шанс поправиться.
    #[test]
    fn binaries_are_refused_with_a_useful_message() {
        let err = plan_read(Some("application/pdf"), Some(4096)).expect_err("must refuse");
        assert!(err.contains("application/pdf"), "{err}");
        assert!(err.contains("4096"), "{err}");

        // Неизвестный mime — тот же отказ, а не паника.
        assert!(plan_read(None, None).is_err());
    }

    #[test]
    fn oversize_is_refused_before_downloading() {
        let err =
            plan_read(Some("text/plain"), Some(MAX_READ_TEXT_BYTES + 1)).expect_err("must refuse");
        assert!(err.contains("too large"), "{err}");

        let err =
            plan_read(Some("image/png"), Some(MAX_READ_IMAGE_BYTES + 1)).expect_err("must refuse");
        assert!(err.contains("too large"), "{err}");

        // Ровно на лимите — ещё можно.
        assert!(plan_read(Some("text/plain"), Some(MAX_READ_TEXT_BYTES)).is_ok());
    }

    /// `size` из meta может быть `null` (YouTrack его не всегда считает), и
    /// тогда `plan_read` решает по `unwrap_or(0)` — без реального лимита.
    /// `content_block` обязан перепроверить лимит по байтам, которые
    /// действительно приехали вторым (content) запросом.
    #[test]
    fn content_block_rechecks_the_downloaded_size_against_the_chosen_plan() {
        let content = AttachmentContent {
            name: "x.txt".to_owned(),
            mime_type: Some("text/plain".to_owned()),
            size: None,
            bytes: vec![0u8; (MAX_READ_TEXT_BYTES + 1) as usize],
            base64: String::new(),
        };
        let err = content_block(content, ReadPlan::Text).expect_err("must refuse");
        assert!(err.contains("limit"), "{err}");

        let content = AttachmentContent {
            name: "y.png".to_owned(),
            mime_type: Some("image/png".to_owned()),
            size: None,
            bytes: vec![0u8; (MAX_READ_IMAGE_BYTES + 1) as usize],
            base64: String::new(),
        };
        let err = content_block(content, ReadPlan::Image).expect_err("must refuse");
        assert!(err.contains("limit"), "{err}");

        // В пределах лимита — блок собирается.
        let content = AttachmentContent {
            name: "z.txt".to_owned(),
            mime_type: Some("text/plain".to_owned()),
            size: None,
            bytes: b"hello".to_vec(),
            base64: String::new(),
        };
        assert!(content_block(content, ReadPlan::Text).is_ok());
    }
}
