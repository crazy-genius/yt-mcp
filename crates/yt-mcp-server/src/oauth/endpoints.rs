//! Метаданные OAuth (RFC 9728, RFC 8414) и заглушка динамической регистрации
//! клиентов (RFC 7591). Всё без аутентификации: по этим ответам клиент как раз
//! и узнаёт, как её пройти.

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::{Value, json};
use url::Url;

use super::{OAuth, Pending, random_state};

/// `client_id`, который получают все: клиент публичный, а настоящая
/// идентичность — это Hub-клиент самого сервера.
const SHARED_CLIENT_ID: &str = "yt-mcp";

pub fn routes() -> Router<Arc<OAuth>> {
    Router::new()
        .route("/.well-known/oauth-protected-resource", get(protected_resource))
        .route("/.well-known/oauth-protected-resource/mcp", get(protected_resource))
        .route("/.well-known/oauth-authorization-server", get(authorization_server))
        .route("/.well-known/openid-configuration", get(authorization_server))
        .route("/register", post(register))
        .route("/authorize", get(authorize))
        .route("/token", post(token))
        .route("/mcp/auth_callback", get(auth_callback))
}

/// RFC 9728: что защищено и кто выдаёт токены.
async fn protected_resource(State(oauth): State<Arc<OAuth>>) -> Json<Value> {
    let public = &oauth.settings.public_url;
    Json(json!({
        "resource": format!("{public}/mcp"),
        "authorization_servers": [public],
        "scopes_supported": [oauth.hub.scope],
        "bearer_methods_supported": ["header"],
    }))
}

/// RFC 8414. Отдаётся и по `openid-configuration`: часть клиентов ходит
/// только туда.
async fn authorization_server(State(oauth): State<Arc<OAuth>>) -> Json<Value> {
    let public = &oauth.settings.public_url;
    Json(json!({
        "issuer": public,
        "authorization_endpoint": format!("{public}/authorize"),
        "token_endpoint": format!("{public}/token"),
        "registration_endpoint": format!("{public}/register"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "scopes_supported": [oauth.hub.scope],
        // ponytail: флаг CIMD только объявлен — документ клиента не скачивается, redirect
        // не сверяется с его `redirect_uris` (доверяем `MCP_OAUTH_REDIRECT_URIS`).
        // Апгрейд: fetch документа по `client_id`-URL и сверка redirect_uri.
        "client_id_metadata_document_supported": true,
    }))
}

fn bad_request(error: &str) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": error }))).into_response()
}

/// Заглушка DCR: ничего не запоминает, всем отвечает одним `client_id`, но
/// отказывает, если хоть один `redirect_uri` не из доверенных.
///
/// Тело разбирается руками, а не экстрактором `Json<T>`: тот на кривом теле
/// отвечает своим текстом, а RFC 7591 требует JSON `{"error": ...}`.
async fn register(State(oauth): State<Arc<OAuth>>, body: axum::body::Bytes) -> Response {
    let Ok(meta) = serde_json::from_slice::<Value>(&body) else {
        return bad_request("invalid_client_metadata");
    };
    let uris: Option<Vec<&str>> = meta
        .get("redirect_uris")
        .and_then(Value::as_array)
        .map(|list| list.iter().map(Value::as_str).collect::<Option<Vec<_>>>())
        .unwrap_or(None);
    let Some(uris) = uris.filter(|uris| !uris.is_empty()) else {
        return bad_request("invalid_client_metadata");
    };
    if !uris.iter().all(|uri| oauth.settings.redirects.allows(uri)) {
        return bad_request("invalid_redirect_uri");
    }
    (
        StatusCode::CREATED,
        Json(json!({
            "client_id": SHARED_CLIENT_ID,
            "redirect_uris": uris,
            "token_endpoint_auth_method": "none",
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
        })),
    )
        .into_response()
}

#[derive(Deserialize)]
struct AuthorizeQuery {
    response_type: Option<String>,
    redirect_uri: Option<String>,
    state: Option<String>,
    code_challenge: Option<String>,
    code_challenge_method: Option<String>,
}

/// `302` на `to` с добавленными параметрами. Существующий query в `to`
/// сохраняется. `state` клиента дописывается, только если он был.
fn redirect_with(mut to: Url, params: &[(&str, &str)], client_state: Option<&str>) -> Response {
    {
        let mut query = to.query_pairs_mut();
        for (key, value) in params {
            query.append_pair(key, value);
        }
        if let Some(state) = client_state {
            query.append_pair("state", state);
        }
    }
    // 302, а не 303/307 из `Redirect`: так ждут OAuth-клиенты.
    (StatusCode::FOUND, [(header::LOCATION, to.as_str())]).into_response()
}

/// Начало входа: запоминаем, куда вернуть клиента, и отправляем пользователя
/// в Hub с нашим `state` и нашим callback-ом (клиентский `redirect_uri` в Hub
/// не зарегистрирован).
///
/// Единственная защита от open redirect — `allows`: пока `redirect_uri` не
/// прошёл её, никуда не редиректим, а отвечаем `400`. Дальше редирект идёт
/// только на разобранный `Url`, который проверялся, а не на сырую строку.
async fn authorize(State(oauth): State<Arc<OAuth>>, Query(q): Query<AuthorizeQuery>) -> Response {
    let Some(redirect) =
        q.redirect_uri.as_deref().filter(|uri| oauth.settings.redirects.allows(uri))
    else {
        return (StatusCode::BAD_REQUEST, "invalid redirect_uri").into_response();
    };
    let Ok(client_redirect) = Url::parse(redirect) else {
        return (StatusCode::BAD_REQUEST, "invalid redirect_uri").into_response();
    };
    let client_state = q.state.as_deref();
    let fail =
        |error: &str| redirect_with(client_redirect.clone(), &[("error", error)], client_state);

    if q.response_type.as_deref() != Some("code") {
        return fail("unsupported_response_type");
    }
    let challenge = match (q.code_challenge.as_deref(), q.code_challenge_method.as_deref()) {
        (Some(challenge), Some("S256")) if !challenge.is_empty() => challenge,
        _ => return fail("invalid_request"),
    };

    let ours = random_state();
    let pending =
        Pending { client_redirect_uri: client_redirect.clone(), client_state: q.state.clone() };
    if oauth.pending.insert(ours.clone(), pending).is_err() {
        return fail("temporarily_unavailable");
    }

    let callback = oauth.callback_url();
    let scope = format!("{} offline_access", oauth.hub.scope);
    // ponytail: refresh просится и scope-ом, и access_type — Task 9 проверяет
    // на реальном Hub, какой сработал, и лишний удаляется.
    redirect_with(
        oauth.hub.authorization_endpoint.clone(),
        &[
            ("response_type", "code"),
            ("client_id", &oauth.settings.hub_client_id),
            ("redirect_uri", &callback),
            ("scope", &scope),
            ("access_type", "offline"),
            ("state", &ours),
            ("code_challenge", challenge),
            ("code_challenge_method", "S256"),
        ],
        None,
    )
}

#[derive(Deserialize)]
struct CallbackQuery {
    state: Option<String>,
    code: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

/// Возврат из Hub: наш `state` одноразовый (`take`), по нему находим клиента
/// и пересылаем ему `code` (или ошибку Hub). `code` и `state` не логируются.
async fn auth_callback(
    State(oauth): State<Arc<OAuth>>,
    Query(q): Query<CallbackQuery>,
) -> Response {
    let Some(pending) = q.state.as_deref().and_then(|state| oauth.pending.take(state)) else {
        return (StatusCode::BAD_REQUEST, "unknown or expired state").into_response();
    };
    let client_state = pending.client_state.as_deref();
    if let Some(error) = q.error.as_deref() {
        let mut params = vec![("error", error)];
        if let Some(description) = q.error_description.as_deref() {
            params.push(("error_description", description));
        }
        return redirect_with(pending.client_redirect_uri, &params, client_state);
    }
    match q.code.as_deref() {
        Some(code) => redirect_with(pending.client_redirect_uri, &[("code", code)], client_state),
        None => {
            redirect_with(pending.client_redirect_uri, &[("error", "server_error")], client_state)
        }
    }
}

/// Обмен кода и refresh: форма клиента переписывается под Hub-клиента сервера
/// и уходит в Hub, ответ Hub возвращается как есть. Клиентские `client_id`,
/// `client_secret`, `redirect_uri` и `resource` в Hub не попадают: у Hub свой
/// клиент и свой callback. Тело, `code`, verifier и токены не логируются.
async fn token(State(oauth): State<Arc<OAuth>>, body: axum::body::Bytes) -> Response {
    // Разбор не падает на кривом теле: мусор даст пустой список и уйдёт в
    // `unsupported_grant_type`. Отдельный `invalid_request` нужен только для
    // не-UTF-8, которое `form_urlencoded` молча подменяет, поэтому проверяем его явно.
    if std::str::from_utf8(&body).is_err() {
        return bad_request("invalid_request");
    }
    let mut grant = None;
    let mut form: Vec<(String, String)> = Vec::new();
    for (key, value) in url::form_urlencoded::parse(&body) {
        match key.as_ref() {
            "client_id" | "client_secret" | "redirect_uri" | "resource" => {}
            "grant_type" => grant = Some(value.into_owned()),
            _ => form.push((key.into_owned(), value.into_owned())),
        }
    }
    let grant = match grant.as_deref() {
        Some(g @ ("authorization_code" | "refresh_token")) => g.to_owned(),
        _ => return bad_request("unsupported_grant_type"),
    };
    form.push(("grant_type".to_owned(), grant.clone()));
    form.push(("client_id".to_owned(), oauth.settings.hub_client_id.clone()));
    if grant == "authorization_code" {
        form.push(("redirect_uri".to_owned(), oauth.callback_url()));
    }

    let mut request = oauth.http.post(oauth.hub.token_endpoint.clone()).form(&form);
    if let Some(secret) = &oauth.settings.hub_client_secret {
        request = request.basic_auth(&oauth.settings.hub_client_id, Some(secret));
    }
    let hub = match request.send().await {
        Ok(response) => response,
        Err(error) => {
            // Только вид ошибки: текст reqwest несёт URL.
            tracing::warn!(
                connect = error.is_connect(),
                timeout = error.is_timeout(),
                "hub token request failed"
            );
            return hub_unavailable();
        }
    };
    let status = StatusCode::from_u16(hub.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let content_type = hub.headers().get(header::CONTENT_TYPE).cloned();
    let Ok(bytes) = hub.bytes().await else {
        return hub_unavailable();
    };
    let mut response = (status, bytes).into_response();
    let headers = response.headers_mut();
    headers.remove(header::CONTENT_TYPE);
    if let Some(content_type) = content_type {
        headers.insert(header::CONTENT_TYPE, content_type);
    }
    headers.insert(header::CACHE_CONTROL, header::HeaderValue::from_static("no-store"));
    response
}

fn hub_unavailable() -> Response {
    let mut response =
        (StatusCode::BAD_GATEWAY, Json(json!({ "error": "temporarily_unavailable" })))
            .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, header::HeaderValue::from_static("no-store"));
    response
}
