use std::sync::Arc;

use rmcp::model::CallToolRequestParams;
use rmcp::{ServiceExt, serve_server};
use yt_client::Youtrack;
use yt_mcp_core::mcp::YoutrackMCPServer;

/// Поднимает сервер и клиента на паре дуплексных стримов в памяти.
/// HTTP-транспорта здесь нет, значит нет и `http::request::Parts`, значит
/// вызов тула обязан упереться в проверку токена уровня 2.
#[tokio::test]
async fn call_tool_dispatches_and_reports_a_missing_token() {
    let (server_io, client_io) = tokio::io::duplex(4096);

    let yt = Arc::new(Youtrack::new("https://example.com").expect("youtrack builds"));
    // `RunningService` держит сервер живым: если уронить его сразу, клиент
    // получит BrokenPipe ещё на initialized-нотификации.
    tokio::spawn(async move {
        if let Ok(server) = serve_server(YoutrackMCPServer::new(yt), server_io).await {
            let _ = server.waiting().await;
        }
    });

    let client = ().serve(client_io).await.expect("client connects");

    let tools = client.list_all_tools().await.expect("tools/list works");
    assert!(!tools.is_empty(), "tools/list must serve the router's tools over the wire");

    let result = client
        .call_tool(CallToolRequestParams::new("youtrack_list_projects"))
        .await
        .expect("tools/call доехал до тула, а не упал в -32601");

    assert_eq!(result.is_error, Some(true), "нет токена — это ошибка исполнения тула");
    let text = format!("{:?}", result.content);
    assert!(text.contains("X-YouTrack-Token"), "текст говорит, что именно добавить: {text}");

    client.cancel().await.expect("clean shutdown");
}
