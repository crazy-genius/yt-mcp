use std::sync::Arc;

use anyhow::Context;
use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use subtle::ConstantTimeEq;
use tokio::net::TcpListener;
use yt_client::Youtrack;
use yt_mcp_core::mcp::{API_VERSION_HEADER, YoutrackMCPServer, api_v2, bearer};

use crate::oauth::OAuth;
use crate::oauth::config::{OAuthSettings, discover, settings_from};
use crate::oauth::verify::{Verdict, verify};

mod oauth;

/// Конфигурация процесса. Системного YouTrack-токена здесь нет: доступ к
/// YouTrack всегда пер-пользовательский и приезжает заголовком
/// `X-YouTrack-Token` на каждый вызов тула.
struct Config {
    youtrack_url: String,
    mcp_token: String,
    bind: String,
    allowed_hosts: Option<Vec<String>>,
    /// `None` — v2 не настроен, OAuth-маршруты не монтируются.
    oauth: Option<OAuthSettings>,
}

impl Config {
    fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            youtrack_url: std::env::var("YOUTRACK_URL").context("YOUTRACK_URL is required")?,
            mcp_token: checked_mcp_token(
                std::env::var("MCP_TOKEN").context("MCP_TOKEN is required")?,
            )?,
            bind: std::env::var("MCP_BIND").unwrap_or_else(|_| "127.0.0.1:8080".to_owned()),
            allowed_hosts: allowed_hosts_from(std::env::var("MCP_ALLOWED_HOSTS").ok()),
            oauth: settings_from(|name| std::env::var(name).ok())?,
        })
    }
}

/// Секрет уровня 1 не имеет права быть пустым.
///
/// `MCP_TOKEN=` даёт `Ok("")`, и тогда `authorized` сравнивает «» с «»,
/// то есть пускает любого, кто прислал `Authorization: Bearer `. Незаданная
/// переменная роняет процесс, а пустая — ровно то, что получается из
/// потерянного секрета в compose или Kubernetes — открывала бы сервер молча.
/// Отдельной функцией, чтобы проверять без правки окружения процесса.
fn checked_mcp_token(token: String) -> anyhow::Result<String> {
    anyhow::ensure!(!token.trim().is_empty(), "MCP_TOKEN must not be empty");
    Ok(token)
}

/// Разбор `MCP_ALLOWED_HOSTS` — списка `Host`, которые транспорт готов принять.
///
/// `None` значит «переменная не задана», и тогда остаётся дефолт `rmcp`
/// (`localhost`, `127.0.0.1`, `::1`). За обратным прокси этого мало: nginx,
/// Caddy и Traefik по умолчанию форвардят исходный `Host`, и без этой
/// переменной каждый запрос получит `403 Forbidden: Host header is not allowed`.
///
/// Пустой список в `rmcp` означает «пускать любой `Host`», поэтому
/// `MCP_ALLOWED_HOSTS=` схлопывается в `None`, то есть в дефолт, а не в
/// снятую проверку: пустая переменная — та же потерянная конфигурация, что и
/// в `checked_mcp_token`.
fn allowed_hosts_from(raw: Option<String>) -> Option<Vec<String>> {
    let hosts: Vec<String> =
        raw?.split(',').map(str::trim).filter(|host| !host.is_empty()).map(str::to_owned).collect();
    (!hosts.is_empty()).then_some(hosts)
}

/// Самая частая ошибка развёртывания, и притом бесшумная: сервер слушает не
/// loopback, а `MCP_ALLOWED_HOSTS` не задан. Транспорт `rmcp` принимает тогда
/// только `localhost`, `127.0.0.1` и `::1`, то есть отвечает `403` на всё, что
/// придёт снаружи. Оператор видит 403 у клиента и ничего в логе — искать
/// причину негде.
///
/// Отдельной функцией и возвращает текст, а не пишет сам, чтобы проверять без
/// поднятия сервера. Неразобранный `bind` считаем публичным: лишнее
/// предупреждение дешевле пропущенного.
fn unreachable_warning(bind: &str, allowed_hosts: Option<&[String]>) -> Option<String> {
    if allowed_hosts.is_some() {
        return None;
    }
    let loopback_only =
        bind.parse::<std::net::SocketAddr>().map(|addr| addr.ip().is_loopback()).unwrap_or(false);

    (!loopback_only).then(|| {
        format!(
            "MCP_BIND={bind} listens beyond loopback but MCP_ALLOWED_HOSTS is unset: the \
             transport accepts only Host: localhost, 127.0.0.1 or ::1 and answers 403 \
             Forbidden to everything else. Set MCP_ALLOWED_HOSTS to the hostname clients \
             actually use — it replaces that default entirely."
        )
    })
}

/// Гейт уровня 1: пускает ли этот запрос вообще говорить с сервером.
///
/// Сравнение в постоянном времени — наивное течёт по времени и выдаёт
/// секрет по префиксу. `ct_eq` на срезах разной длины честно возвращает
/// «не равно», и длина секрета здесь не тайна.
fn authorized(headers: &HeaderMap, expected: &str) -> bool {
    let Some(got) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
    else {
        return false;
    };
    got.as_bytes().ct_eq(expected.as_bytes()).into()
}

/// Состояние `gate`: секрет v1 и, если v2 настроен, всё для проверки Hub-токена.
struct GateState {
    mcp_token: String,
    v2: Option<(Arc<Youtrack>, Arc<OAuth>)>,
}

/// 401 с OAuth-вызовом (RFC 9728): по `resource_metadata` клиент находит Hub.
fn oauth_challenge(oauth: &OAuth, invalid: bool) -> Response {
    let error = if invalid { r#"error="invalid_token", "# } else { "" };
    let value = format!(
        r#"Bearer {error}resource_metadata="{}", scope="{}""#,
        oauth.resource_metadata_url(),
        oauth.hub.scope
    );
    (StatusCode::UNAUTHORIZED, [(header::WWW_AUTHENTICATE, value)], "unauthorized").into_response()
}

async fn gate(State(state): State<Arc<GateState>>, mut request: Request, next: Next) -> Response {
    if let Some((youtrack, oauth)) = &state.v2
        && api_v2(request.headers())
    {
        // В v2 Hub-токен заменяет MCP_TOKEN. Проверяем до транспорта: загрузка
        // вложения по URL идёт раньше любого запроса в YouTrack, и непроверенный
        // запрос, дошедший до тула, — вектор SSRF.
        let Some(token) = bearer(request.headers()) else {
            return oauth_challenge(oauth, false);
        };
        // Секрет сервера не токен пользователя: не отправляем его в YouTrack.
        if token.as_bytes().ct_eq(state.mcp_token.as_bytes()).into() {
            return oauth_challenge(oauth, true);
        }
        return match verify(youtrack, oauth, token).await {
            Verdict::Valid => next.run(request).await,
            Verdict::Invalid => oauth_challenge(oauth, true),
            Verdict::Unavailable => StatusCode::BAD_GATEWAY.into_response(),
        };
    }
    // v1: заголовок версии здесь не действует. Иначе при ненастроенном v2
    // `user_token` в core, увидев `v2`, взял бы из `Authorization` сам MCP_TOKEN
    // и отправил его в YouTrack как токен пользователя.
    request.headers_mut().remove(API_VERSION_HEADER);
    if authorized(request.headers(), &state.mcp_token) {
        next.run(request).await
    } else {
        // WWW-Authenticate обязателен: по нему клиент понимает, что нужен
        // Bearer, а не что сервер сломан.
        (StatusCode::UNAUTHORIZED, [(header::WWW_AUTHENTICATE, "Bearer")], "unauthorized")
            .into_response()
    }
}

/// MCP на `/mcp` за гейтом уровня 1. Отдельной функцией, чтобы тест гонял
/// ровно то приложение, которое поднимает `main`, а не его пересказ.
fn app(
    youtrack: Arc<Youtrack>,
    mcp_token: String,
    allowed_hosts: Option<Vec<String>>,
    oauth: Option<Arc<OAuth>>,
) -> Router {
    // Дефолт `rmcp` — только loopback-хосты. Задан `MCP_ALLOWED_HOSTS` —
    // список заменяется целиком; не задан — дефолт остаётся как есть.
    let config = match allowed_hosts {
        Some(hosts) => StreamableHttpServerConfig::default().with_allowed_hosts(hosts),
        None => StreamableHttpServerConfig::default(),
    };
    let verify_youtrack = youtrack.clone();
    let mcp = StreamableHttpService::new(
        move || Ok(YoutrackMCPServer::new(youtrack.clone())),
        LocalSessionManager::default().into(),
        config,
    );

    let v2 = oauth.clone().map(|oauth| (verify_youtrack, oauth));
    let gated = Router::new()
        .nest_service("/mcp", mcp)
        .layer(middleware::from_fn_with_state(Arc::new(GateState { mcp_token, v2 }), gate));
    // OAuth-маршруты подмешиваются ПОСЛЕ слоя: гейт уровня 1 на них не действует,
    // иначе клиент без токена не смог бы начать вход.
    match oauth {
        Some(oauth) => gated.merge(oauth::routes(oauth)),
        None => gated,
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Без подписчика `tracing::warn!` самого `rmcp` уходит в никуда: оператор
    // видит 403 у клиента и пустой лог у сервера. Своего логирования здесь не
    // добавляется — `http::request::Parts` и расширения запроса несут оба
    // секрета и не печатаются нигде (см. §3.4 дизайна).
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let config = Config::from_env()?;

    // URL проверяется здесь, чтобы битая конфигурация уронила процесс при
    // старте с внятным сообщением, а не на первом вызове тула.
    // as_str(), а не &config.youtrack_url: параметр обобщённый (impl
    // Into<String>), и разыменование &String -> &str там не сработает.
    let youtrack = Arc::new(
        Youtrack::new(config.youtrack_url.as_str())
            .context("YOUTRACK_URL is not a valid YouTrack URL")?,
    );

    if let Some(warning) = unreachable_warning(&config.bind, config.allowed_hosts.as_deref()) {
        tracing::warn!("{warning}");
    }

    // Ошибка обнаружения Hub роняет процесс при старте, как и битый YOUTRACK_URL.
    // `main` возвращает `anyhow::Result`, поэтому в stderr попадает вся цепочка причин.
    let oauth = match config.oauth {
        Some(settings) => {
            let confidential = settings.hub_client_secret.is_some();
            let hub =
                discover(&oauth::http_client(settings.upstream_timeout)?, &config.youtrack_url)
                    .await
                    .context("cannot discover YouTrack Hub OAuth endpoints")?;
            let oauth = OAuth::new(settings, hub).context("cannot build OAuth state")?;
            // Ни id, ни секрет в лог не идут: только режим клиента.
            tracing::info!(
                "OAuth v2 enabled, Hub client is {}",
                if confidential { "confidential" } else { "public" }
            );
            Some(Arc::new(oauth))
        }
        None => None,
    };

    let app = app(youtrack, config.mcp_token, config.allowed_hosts, oauth);

    let listener = TcpListener::bind(&config.bind)
        .await
        .with_context(|| format!("cannot bind {}", config.bind))?;
    // Через tracing, а не eprintln: подписчик уже стоит, и строка, идущая
    // мимо него, — единственная несогласованная в выводе.
    tracing::info!("yt-mcp-server listening on {}/mcp", config.bind);

    axum::serve(listener, app).await.context("server stopped")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;
    // `header` не импортируется: под этим именем в модуле уже живёт
    // `axum::http::header`, которым набираются заголовки запросов.
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Пустой `MCP_TOKEN` обязан ронять старт, а не открывать сервер: в
    /// `authorized` он сравнивался бы сам с собой и пропускал любой запрос
    /// с `Authorization: Bearer `.
    #[test]
    fn an_empty_mcp_token_is_refused() {
        assert!(checked_mcp_token("s3cret".to_owned()).is_ok());
        assert!(checked_mcp_token(String::new()).is_err(), "MCP_TOKEN= открыл бы гейт уровня 1");
        assert!(checked_mcp_token("   ".to_owned()).is_err(), "одни пробелы — тот же пустой токен");
    }

    #[test]
    fn allowed_hosts_are_parsed_and_an_empty_value_falls_back_to_the_default() {
        assert_eq!(allowed_hosts_from(None), None);
        assert_eq!(
            allowed_hosts_from(Some(" mcp.example.com , mcp.example.com:8443 ".to_owned())),
            Some(vec!["mcp.example.com".to_owned(), "mcp.example.com:8443".to_owned()])
        );
        // Пустой список в rmcp означает «любой Host», поэтому пустая
        // переменная обязана вернуть None (дефолт), а не Some(vec![]).
        assert_eq!(allowed_hosts_from(Some(String::new())), None);
        assert_eq!(allowed_hosts_from(Some(" , ".to_owned())), None);
    }

    /// Предупреждение обязано появляться ровно в той конфигурации, которая
    /// молча отдаёт 403 на каждый запрос, и молчать во всех остальных —
    /// иначе оператор научится его игнорировать.
    #[test]
    fn the_unreachable_warning_fires_only_when_the_deployment_is_broken() {
        let hosts = ["mcp.example.com".to_owned()];

        assert!(
            unreachable_warning("0.0.0.0:8080", None).is_some(),
            "слушает наружу без списка хостов — каждый запрос получит 403"
        );
        assert!(
            unreachable_warning("0.0.0.0:8080", Some(&hosts)).is_none(),
            "список задан — предупреждать не о чем"
        );
        assert!(
            unreachable_warning("127.0.0.1:8080", None).is_none(),
            "loopback покрыт дефолтом rmcp"
        );
        assert!(unreachable_warning("[::1]:8080", None).is_none(), "loopback и в IPv6 — тоже");
        assert!(
            unreachable_warning("not-an-address", None).is_some(),
            "неразобранный bind считаем публичным: промолчать дороже"
        );

        let text = unreachable_warning("0.0.0.0:8080", None).expect("есть");
        assert!(text.contains("MCP_ALLOWED_HOSTS"), "текст обязан назвать переменную: {text}");
        assert!(text.contains("403"), "и симптом, по которому это ищут: {text}");
    }

    /// Гейт поверх заглушки: за ним должно быть видно только «дошло/не дошло».
    fn gated_stub() -> Router {
        Router::new().route("/mcp", axum::routing::get(|| async { "reached" })).layer(
            middleware::from_fn_with_state(
                Arc::new(GateState { mcp_token: "s3cret".to_owned(), v2: None }),
                gate,
            ),
        )
    }

    async fn status_for(header: Option<&str>) -> StatusCode {
        let mut builder = Request::builder().uri("/mcp");
        if let Some(value) = header {
            builder = builder.header(header::AUTHORIZATION, value);
        }
        let request = builder.body(Body::empty()).expect("request builds");
        gated_stub().oneshot(request).await.expect("service responds").status()
    }

    #[tokio::test]
    async fn gate_rejects_a_request_without_the_header() {
        assert_eq!(status_for(None).await, StatusCode::UNAUTHORIZED);
    }

    /// Токен той же длины, что и секрет: короткий отсекается сравнением длин
    /// ещё до побайтового прохода, и такой тест не проверял бы ничего сверх
    /// теста на префикс.
    #[tokio::test]
    async fn gate_rejects_a_wrong_token() {
        assert_eq!(status_for(Some("Bearer s3cre7")).await, StatusCode::UNAUTHORIZED);
    }

    /// Префикс тоже должен не подойти: сравнение обязано быть по всей длине.
    #[tokio::test]
    async fn gate_rejects_a_prefix_of_the_token() {
        assert_eq!(status_for(Some("Bearer s3c")).await, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn gate_lets_the_right_token_through() {
        assert_eq!(status_for(Some("Bearer s3cret")).await, StatusCode::OK);
    }

    /// Настоящий `tools/call` целиком; `auth` — единственное, что отличает
    /// пропущенный запрос от отбитого, поэтому оба теста ниже шлют ровно
    /// этот, а не два похожих.
    ///
    /// Заголовок `MCP-Protocol-Version: 2026-07-28` уводит запрос на
    /// stateless-ветку транспорта: без него пришлось бы сначала гонять
    /// initialize ради `Mcp-Session-Id`.
    ///
    /// `X-YouTrack-Token` ставится только в v1 (без `extra`); v2-вызов
    /// передаёт свои заголовки в `extra`.
    fn tool_call_request(auth: Option<&str>) -> Request<Body> {
        tool_call_request_with(auth, &[("X-YouTrack-Token", "user-token")])
    }

    fn tool_call_request_with(auth: Option<&str>, extra: &[(&str, &str)]) -> Request<Body> {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header(header::HOST, "127.0.0.1")
            .header("MCP-Protocol-Version", "2026-07-28")
            .header("Mcp-Method", "tools/call")
            .header("Mcp-Name", "youtrack_list_projects")
            .header(header::ACCEPT, "application/json, text/event-stream")
            .header(header::CONTENT_TYPE, "application/json");
        for (name, value) in extra {
            builder = builder.header(*name, *value);
        }
        if let Some(value) = auth {
            builder = builder.header(header::AUTHORIZATION, value);
        }
        builder
            .body(Body::from(
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                    "name":"youtrack_list_projects","arguments":{},
                    "_meta":{
                        "io.modelcontextprotocol/protocolVersion":"2026-07-28",
                        "io.modelcontextprotocol/clientCapabilities":{}
                    }}}"#,
            ))
            .expect("request builds")
    }

    /// `Youtrack`, до которого заведомо не достучаться: порт 1 никто не слушает.
    fn dead_youtrack() -> Arc<Youtrack> {
        Arc::new(Youtrack::new("http://127.0.0.1:1").expect("youtrack builds"))
    }

    /// Гейт на настоящей сборке роутера, а не на заглушке рядом.
    ///
    /// Тесты выше гоняют middleware поверх `gated_stub()`, который повторяет
    /// проводку руками — они останутся зелёными, если в `app()` перепутать
    /// порядок и написать `.layer(gate)` до `.nest_service("/mcp", mcp)`.
    /// Слой, повешенный до маршрута, на этот маршрут не действует, и `/mcp`
    /// окажется открыт настежь. Здесь это ловится.
    #[tokio::test]
    async fn the_real_router_rejects_a_tool_call_without_the_bearer() {
        let response = app(dead_youtrack(), "s3cret".to_owned(), None, None)
            .oneshot(tool_call_request(None))
            .await
            .expect("service responds");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "/mcp открыт без гейта");
    }

    /// Вся цепочка на настоящем HTTP-транспорте: гейт уровня 1 пропускает,
    /// `call_tool` достаёт токен уровня 2 из заголовка и передаёт вызов
    /// роутеру тулов, тело тула идёт в YouTrack **с тем самым токеном**.
    ///
    /// YouTrack здесь — wiremock, и мок сматчен по
    /// `Authorization: Bearer user-token`. Это единственное место, где
    /// пришедшее из заголовка и ушедшее в YouTrack сверяются друг с другом:
    /// форвард чужой строки (`MCP_TOKEN`, пустой токен, старый) мок не
    /// сматчит, и `verify()` в конце упадёт. Заодно мок доказывает базовый
    /// URL — на неверный до него просто не дошли бы.
    ///
    /// Диагностика по телу ответа сохранена: `-32601` значит, что `call_tool`
    /// не форвардит в роутер тулов, «no YouTrack token» — что сломалось
    /// чтение заголовка.
    #[tokio::test]
    async fn a_tool_call_travels_from_the_wire_to_youtrack() {
        let youtrack = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/admin/projects"))
            .and(wiremock::matchers::header("authorization", "Bearer user-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .expect(1)
            .mount(&youtrack)
            .await;

        let yt = Arc::new(Youtrack::new(youtrack.uri()).expect("youtrack builds"));
        let response = app(yt, "s3cret".to_owned(), None, None)
            .oneshot(tool_call_request(Some("Bearer s3cret")))
            .await
            .expect("service responds");
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024).await.expect("body reads");
        let body = String::from_utf8_lossy(&body);
        // Порядок проверок — от самого диагностичного к общему: без него
        // пропавший форвард в роутер выглядел бы просто «не 200».
        assert!(!body.contains("-32601"), "call_tool не форвардит в роутер тулов: {body}");
        assert!(!body.contains("no YouTrack token"), "заголовок уровня 2 не доехал: {body}");
        assert_eq!(status, StatusCode::OK, "гейт уровня 1 обязан пропустить: {body}");
        assert!(!body.contains("youtrack:"), "тело тула не дошло до YouTrack: {body}");

        // Явно, а не на дропе: так падение читается как «мок не сматчен»,
        // а не как паника в деструкторе посреди других ассертов.
        youtrack.verify().await;
    }

    // --- OAuth v2: метаданные и /register ---

    use crate::oauth::config::{HubEndpoints, OAuthSettings};
    use crate::oauth::redirect::TrustedRedirects;
    use serde_json::{Value, json};

    fn test_oauth(hub: &MockServer, secret: Option<&str>) -> Arc<OAuth> {
        let settings = OAuthSettings {
            public_url: "https://yt-mcp.dev".to_owned(),
            hub_client_id: "hub-client".to_owned(),
            hub_client_secret: secret.map(str::to_owned),
            redirects: TrustedRedirects::parse(
                "http://localhost/callback, http://localhost/cb2?x=1",
            )
            .expect("parses"),
            upstream_timeout: crate::oauth::config::DEFAULT_UPSTREAM_TIMEOUT,
        };
        let hub_endpoints = HubEndpoints {
            authorization_endpoint: format!("{}/hub/auth", hub.uri()).parse().expect("url"),
            token_endpoint: format!("{}/hub/token", hub.uri()).parse().expect("url"),
            scope: "svc-1".to_owned(),
        };
        Arc::new(OAuth::new(settings, hub_endpoints).expect("oauth builds"))
    }

    fn app_v2(youtrack: Arc<Youtrack>, hub: &MockServer) -> Router {
        app(youtrack, "s3cret".into(), None, Some(test_oauth(hub, Some("hub-s3cret"))))
    }

    struct Reply {
        status: StatusCode,
        json: Value,
        location: Option<String>,
        cache_control: Option<String>,
    }

    async fn send(app: Router, request: Request<Body>) -> Reply {
        let response = app.oneshot(request).await.expect("service responds");
        let status = response.status();
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let cache_control = response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024).await.expect("body");
        let json = serde_json::from_slice(&body).unwrap_or(Value::Null);
        Reply { status, json, location, cache_control }
    }

    async fn get(app: Router, uri: &str) -> Reply {
        send(app, Request::builder().uri(uri).body(Body::empty()).expect("request")).await
    }

    async fn post_json(app: Router, uri: &str, body: Value) -> Reply {
        let request = Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .expect("request");
        send(app, request).await
    }

    async fn post_form(app: Router, uri: &str, body: &str) -> Reply {
        let request = Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body.to_owned()))
            .expect("request");
        send(app, request).await
    }

    #[tokio::test]
    async fn metadata_is_served_without_any_auth() {
        let hub = MockServer::start().await;
        for uri in
            ["/.well-known/oauth-protected-resource/mcp", "/.well-known/oauth-protected-resource"]
        {
            let r = get(app_v2(dead_youtrack(), &hub), uri).await;
            assert_eq!(r.status, 200, "{uri}");
            assert_eq!(r.json["resource"], "https://yt-mcp.dev/mcp");
            assert_eq!(r.json["authorization_servers"][0], "https://yt-mcp.dev");
            assert_eq!(r.json["scopes_supported"][0], "svc-1");
            assert_eq!(r.json["bearer_methods_supported"][0], "header");
        }
        for uri in ["/.well-known/oauth-authorization-server", "/.well-known/openid-configuration"]
        {
            let a = get(app_v2(dead_youtrack(), &hub), uri).await;
            assert_eq!(a.status, 200, "{uri}");
            assert_eq!(a.json["issuer"], "https://yt-mcp.dev");
            assert_eq!(a.json["authorization_endpoint"], "https://yt-mcp.dev/authorize");
            assert_eq!(a.json["token_endpoint"], "https://yt-mcp.dev/token");
            assert_eq!(a.json["registration_endpoint"], "https://yt-mcp.dev/register");
            assert_eq!(a.json["code_challenge_methods_supported"][0], "S256");
            assert_eq!(a.json["token_endpoint_auth_methods_supported"][0], "none");
            assert_eq!(a.json["grant_types_supported"][1], "refresh_token");
        }
    }

    #[tokio::test]
    async fn as_metadata_advertises_cimd_for_public_clients() {
        let hub = MockServer::start().await;
        let a = get(app_v2(dead_youtrack(), &hub), "/.well-known/oauth-authorization-server").await;
        assert_eq!(a.json["client_id_metadata_document_supported"], true);
        assert_eq!(a.json["token_endpoint_auth_methods_supported"][0], "none");
    }

    #[tokio::test]
    async fn register_issues_the_shared_client_for_trusted_redirects_only() {
        let hub = MockServer::start().await;
        let ok = post_json(
            app_v2(dead_youtrack(), &hub),
            "/register",
            json!({"redirect_uris":["http://localhost:5000/callback"]}),
        )
        .await;
        assert_eq!(ok.status, 201);
        assert_eq!(ok.json["client_id"], "yt-mcp");
        assert_eq!(ok.json["redirect_uris"][0], "http://localhost:5000/callback");
        assert_eq!(ok.json["token_endpoint_auth_method"], "none");
        assert!(ok.json.get("client_secret").is_none());
        let bad = post_json(
            app_v2(dead_youtrack(), &hub),
            "/register",
            json!({"redirect_uris":["https://evil.example/cb"]}),
        )
        .await;
        assert_eq!(bad.status, 400);
        assert_eq!(bad.json["error"], "invalid_redirect_uri");
    }

    #[tokio::test]
    async fn register_rejects_malformed_metadata() {
        let hub = MockServer::start().await;
        for body in [json!({}), json!({"redirect_uris":[]}), json!({"redirect_uris":"x"})] {
            let r = post_json(app_v2(dead_youtrack(), &hub), "/register", body).await;
            assert_eq!(r.status, 400);
            assert_eq!(r.json["error"], "invalid_client_metadata");
        }
        let request = Request::builder()
            .method("POST")
            .uri("/register")
            .body(Body::from("not json"))
            .expect("request");
        let r = send(app_v2(dead_youtrack(), &hub), request).await;
        assert_eq!(r.status, 400);
        assert_eq!(r.json["error"], "invalid_client_metadata");
    }

    #[tokio::test]
    async fn without_v2_settings_there_are_no_oauth_routes() {
        let r = get(
            app(dead_youtrack(), "s3cret".into(), None, None),
            "/.well-known/oauth-protected-resource",
        )
        .await;
        assert_ne!(r.status, 200);
    }

    // --- OAuth v2: /authorize и /mcp/auth_callback ---

    use std::collections::HashMap;
    use url::Url;

    fn query_of(location: &str) -> (Url, HashMap<String, String>) {
        let url = Url::parse(location).expect("location is a url");
        let q = url.query_pairs().into_owned().collect();
        (url, q)
    }

    fn authorize_uri(redirect: &str, state: Option<&str>) -> String {
        let enc = |s: &str| url::form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>();
        let mut uri = format!(
            "/authorize?response_type=code&client_id=yt-mcp&redirect_uri={}\
             &code_challenge=CHAL&code_challenge_method=S256",
            enc(redirect)
        );
        if let Some(state) = state {
            uri.push_str(&format!("&state={state}"));
        }
        uri
    }

    /// Начинает вход и возвращает наш state, ушедший в Hub.
    async fn begin(app: &Router, redirect: &str, state: Option<&str>) -> String {
        let r = get(app.clone(), &authorize_uri(redirect, state)).await;
        assert_eq!(r.status, 302);
        query_of(r.location.as_deref().expect("location")).1["state"].clone()
    }

    #[tokio::test]
    async fn authorize_never_redirects_to_an_untrusted_uri() {
        let hub = MockServer::start().await;
        for bad in [
            "https://evil.example/cb",
            "http://localhost:5000/callback#frag",
            "http://u:p@localhost:5000/callback",
        ] {
            let r = get(app_v2(dead_youtrack(), &hub), &authorize_uri(bad, None)).await;
            assert_eq!(r.status, 400, "{bad}");
            assert!(r.location.is_none(), "{bad}");
        }
        let r = get(
            app_v2(dead_youtrack(), &hub),
            "/authorize?response_type=code&code_challenge=abc&code_challenge_method=S256",
        )
        .await;
        assert_eq!(r.status, 400);
        assert!(r.location.is_none());
    }

    #[tokio::test]
    async fn authorize_sends_the_client_challenge_to_the_hub_under_our_callback() {
        let hub = MockServer::start().await;
        let uri = "/authorize?response_type=code&client_id=yt-mcp\
            &redirect_uri=http%3A%2F%2Flocalhost%3A5000%2Fcallback&state=cs\
            &code_challenge=CHAL&code_challenge_method=S256";
        let r = get(app_v2(dead_youtrack(), &hub), uri).await;
        assert_eq!(r.status, 302);
        let (to, q) = query_of(r.location.as_deref().unwrap());
        assert_eq!(to.path(), "/hub/auth");
        assert_eq!(q["response_type"], "code");
        assert_eq!(q["client_id"], "hub-client");
        assert_eq!(q["redirect_uri"], "https://yt-mcp.dev/mcp/auth_callback");
        assert_eq!(q["code_challenge"], "CHAL");
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["access_type"], "offline");
        assert_eq!(q["scope"], "svc-1", "только serviceId YouTrack, без offline_access");
        assert_ne!(q["state"], "cs", "в Hub уходит наш state, не клиентский");
    }

    #[tokio::test]
    async fn authorize_errors_go_back_to_the_client_redirect() {
        let hub = MockServer::start().await;
        let base = "/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A5000%2Fcallback&state=cs";
        for (extra, error) in [
            (
                "&response_type=token&code_challenge=C&code_challenge_method=S256",
                "unsupported_response_type",
            ),
            ("&response_type=code&code_challenge=C&code_challenge_method=plain", "invalid_request"),
            ("&response_type=code&code_challenge_method=S256", "invalid_request"),
        ] {
            let r = get(app_v2(dead_youtrack(), &hub), &format!("{base}{extra}")).await;
            assert_eq!(r.status, 302, "{error}");
            let (to, q) = query_of(r.location.as_deref().unwrap());
            assert_eq!(to.as_str().split('?').next(), Some("http://localhost:5000/callback"));
            assert_eq!(q["error"], error);
            assert_eq!(q["state"], "cs");
        }
    }

    #[tokio::test]
    async fn authorize_reports_a_full_pending_map_as_temporarily_unavailable() {
        let hub = MockServer::start().await;
        let oauth = test_oauth(&hub, None);
        let redirect = Url::parse("http://localhost:1/callback").unwrap();
        for i in 0..10_000 {
            let pending =
                crate::oauth::Pending { client_redirect_uri: redirect.clone(), client_state: None };
            assert!(oauth.pending.insert(format!("k{i}"), pending).is_ok());
        }
        let app = app(dead_youtrack(), "s3cret".into(), None, Some(oauth));
        let r = get(app, &authorize_uri("http://localhost:5000/callback", Some("cs"))).await;
        assert_eq!(r.status, 302);
        let (_, q) = query_of(r.location.as_deref().unwrap());
        assert_eq!(q["error"], "temporarily_unavailable");
        assert_eq!(q["state"], "cs");
    }

    #[tokio::test]
    async fn the_callback_returns_the_code_to_the_client_once() {
        let hub = MockServer::start().await;
        let app = app_v2(dead_youtrack(), &hub);
        let ours = begin(&app, "http://localhost:5000/callback", Some("cs")).await;
        // Без Authorization: гейт уровня 1 на callback не действует.
        let uri = format!("/mcp/auth_callback?code=C1&state={ours}");
        let r = get(app.clone(), &uri).await;
        assert_eq!(r.status, 302);
        let (to, q) = query_of(r.location.as_deref().unwrap());
        assert_eq!(to.as_str().split('?').next(), Some("http://localhost:5000/callback"));
        assert_eq!(q["code"], "C1");
        assert_eq!(q["state"], "cs");
        let again = get(app, &uri).await;
        assert_eq!(again.status, 400);
        assert!(again.location.is_none());
    }

    #[tokio::test]
    async fn the_callback_rejects_an_unknown_state() {
        let hub = MockServer::start().await;
        let r = get(app_v2(dead_youtrack(), &hub), "/mcp/auth_callback?code=C1&state=nope").await;
        assert_eq!(r.status, 400);
        assert!(r.location.is_none());
        let r = get(app_v2(dead_youtrack(), &hub), "/mcp/auth_callback?code=C1").await;
        assert_eq!(r.status, 400);
    }

    #[tokio::test]
    async fn the_callback_keeps_the_client_query_and_omits_a_missing_state() {
        let hub = MockServer::start().await;
        let app = app_v2(dead_youtrack(), &hub);
        let ours = begin(&app, "http://localhost:7/cb2?x=1", None).await;
        let r = get(app, &format!("/mcp/auth_callback?code=C1&state={ours}")).await;
        assert_eq!(r.status, 302);
        let location = r.location.unwrap();
        let (_, q) = query_of(&location);
        assert_eq!(q["x"], "1");
        assert_eq!(q["code"], "C1");
        assert!(!location.contains("state="), "{location}");
    }

    #[tokio::test]
    async fn a_hub_error_is_passed_to_the_client() {
        let hub = MockServer::start().await;
        let app = app_v2(dead_youtrack(), &hub);
        let ours = begin(&app, "http://localhost:5000/callback", Some("cs")).await;
        let r = get(
            app,
            &format!(
                "/mcp/auth_callback?error=access_denied&error_description=no+way&state={ours}"
            ),
        )
        .await;
        assert_eq!(r.status, 302);
        let (_, q) = query_of(r.location.as_deref().unwrap());
        assert_eq!(q["error"], "access_denied");
        assert_eq!(q["error_description"], "no way");
        assert_eq!(q["state"], "cs");
        assert!(!q.contains_key("code"));
    }

    #[tokio::test]
    async fn the_mcp_endpoint_still_needs_the_bearer_when_oauth_is_on() {
        let hub = MockServer::start().await;
        let response = app_v2(dead_youtrack(), &hub)
            .oneshot(tool_call_request(None))
            .await
            .expect("service responds");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    // --- OAuth v2: /token ---

    use wiremock::matchers::{body_string_contains, header as header_is};

    /// Совпадает, если заголовка `Authorization` нет вовсе.
    struct NoAuthorization;

    impl wiremock::Match for NoAuthorization {
        fn matches(&self, request: &wiremock::Request) -> bool {
            !request.headers.contains_key("authorization")
        }
    }

    fn hub_tokens() -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_raw(
            r#"{"access_token":"AT","refresh_token":"RT","token_type":"Bearer"}"#,
            "application/json",
        )
    }

    #[tokio::test]
    async fn token_exchange_swaps_redirect_and_client_and_keeps_the_verifier() {
        let hub = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/hub/token"))
            .and(body_string_contains(
                "redirect_uri=https%3A%2F%2Fyt-mcp.dev%2Fmcp%2Fauth_callback",
            ))
            .and(body_string_contains("client_id=hub-client"))
            .and(body_string_contains("code_verifier=VER"))
            .and(body_string_contains("code=C1"))
            .and(header_is("authorization", "Basic aHViLWNsaWVudDpodWItczNjcmV0"))
            .respond_with(hub_tokens())
            .expect(1)
            .mount(&hub)
            .await;
        let r = post_form(
            app_v2(dead_youtrack(), &hub),
            "/token",
            "grant_type=authorization_code&code=C1&code_verifier=VER\
             &redirect_uri=http://localhost:5000/callback&client_id=yt-mcp",
        )
        .await;
        assert_eq!(r.status, 200);
        assert_eq!(r.json["access_token"], "AT");
        assert_eq!(r.cache_control.as_deref(), Some("no-store"));
        hub.verify().await;
        let sent = &hub.received_requests().await.expect("recorded")[0];
        let body = String::from_utf8_lossy(&sent.body);
        assert!(!body.contains("localhost"), "клиентский redirect_uri ушёл в Hub: {body}");
        assert!(!body.contains("yt-mcp&") && !body.ends_with("client_id=yt-mcp"), "{body}");
    }

    #[tokio::test]
    async fn a_public_hub_client_sends_no_basic_auth() {
        let hub = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/hub/token"))
            .and(NoAuthorization)
            .and(body_string_contains("client_id=hub-client"))
            .respond_with(hub_tokens())
            .expect(1)
            .mount(&hub)
            .await;
        let app = app(dead_youtrack(), "s3cret".into(), None, Some(test_oauth(&hub, None)));
        let r = post_form(app, "/token", "grant_type=authorization_code&code=C1").await;
        assert_eq!(r.status, 200);
        hub.verify().await;
    }

    #[tokio::test]
    async fn refresh_does_not_get_a_redirect_uri() {
        let hub = MockServer::start().await;
        Mock::given(method("POST")).respond_with(hub_tokens()).mount(&hub).await;
        let r = post_form(
            app_v2(dead_youtrack(), &hub),
            "/token",
            "grant_type=refresh_token&refresh_token=RT&redirect_uri=http://localhost/callback",
        )
        .await;
        assert_eq!(r.status, 200);
        let sent = &hub.received_requests().await.expect("recorded")[0];
        let body = String::from_utf8_lossy(&sent.body).into_owned();
        assert!(body.contains("refresh_token=RT"), "{body}");
        assert!(body.contains("client_id=hub-client"), "{body}");
        assert!(!body.contains("redirect_uri"), "{body}");
        assert!(sent.headers.contains_key("authorization"));
    }

    #[tokio::test]
    async fn hub_errors_pass_through_and_unknown_grants_stop_here() {
        let hub = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(400)
                    .set_body_raw(r#"{"error":"invalid_grant"}"#, "application/json"),
            )
            .mount(&hub)
            .await;
        let r = post_form(
            app_v2(dead_youtrack(), &hub),
            "/token",
            "grant_type=authorization_code&code=BAD",
        )
        .await;
        assert_eq!(r.status, 400);
        assert_eq!(r.json["error"], "invalid_grant");
        assert_eq!(r.cache_control.as_deref(), Some("no-store"));

        let before = hub.received_requests().await.expect("recorded").len();
        let r = post_form(app_v2(dead_youtrack(), &hub), "/token", "grant_type=password").await;
        assert_eq!(r.status, 400);
        assert_eq!(r.json["error"], "unsupported_grant_type");
        let r = post_form(app_v2(dead_youtrack(), &hub), "/token", "code=C1").await;
        assert_eq!(r.json["error"], "unsupported_grant_type");
        assert_eq!(hub.received_requests().await.expect("recorded").len(), before);
    }

    #[tokio::test]
    async fn an_unreachable_hub_is_a_502_without_details() {
        let hub = MockServer::start().await;
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            listener.local_addr().expect("addr").port()
        };
        let mut oauth =
            Arc::try_unwrap(test_oauth(&hub, Some("hub-s3cret"))).ok().expect("sole owner");
        oauth.hub.token_endpoint =
            format!("http://127.0.0.1:{port}/hub/token").parse().expect("url");
        let oauth = Arc::new(oauth);
        let app = app(dead_youtrack(), "s3cret".into(), None, Some(oauth));
        let r = post_form(app, "/token", "grant_type=refresh_token&refresh_token=RT").await;
        assert_eq!(r.status, 502);
        assert_eq!(r.json, json!({"error": "temporarily_unavailable"}));
    }

    // --- OAuth v2: гейт на /mcp ---

    fn v2_request(auth: Option<&str>) -> Request<Body> {
        tool_call_request_with(auth, &[("X-API-Version", "v2")])
    }

    async fn mock_whoami(yt: &MockServer, status: u16, expect: u64) {
        Mock::given(method("GET"))
            .and(path("/api/users/me"))
            .and(wiremock::matchers::header("authorization", "Bearer hub-token"))
            .respond_with(
                ResponseTemplate::new(status)
                    .set_body_json(serde_json::json!({"id":"1","login":"u"})),
            )
            .expect(expect)
            .mount(yt)
            .await;
    }

    fn www_authenticate(response: &Response) -> String {
        response
            .headers()
            .get(header::WWW_AUTHENTICATE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned()
    }

    #[tokio::test]
    async fn v2_without_a_token_gets_the_oauth_challenge() {
        let hub = MockServer::start().await;
        let response = app_v2(dead_youtrack(), &hub).oneshot(v2_request(None)).await.expect("ok");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let challenge = www_authenticate(&response);
        assert!(
            challenge.contains(
                r#"resource_metadata="https://yt-mcp.dev/.well-known/oauth-protected-resource/mcp""#
            ),
            "{challenge}"
        );
        assert!(challenge.contains(r#"scope="svc-1""#), "{challenge}");
        assert!(!challenge.contains("error="), "{challenge}");

        // Пустой токен = нет токена.
        let response =
            app_v2(dead_youtrack(), &hub).oneshot(v2_request(Some("Bearer "))).await.expect("ok");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(!www_authenticate(&response).contains("error="));
    }

    #[tokio::test]
    async fn v2_with_a_rejected_token_gets_invalid_token_and_never_reaches_the_transport() {
        let hub = MockServer::start().await;
        let youtrack = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/users/me"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({"error":"Unauthorized"})))
            .mount(&youtrack)
            .await;
        Mock::given(path("/api/admin/projects"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&youtrack)
            .await;

        let yt = Arc::new(Youtrack::new(youtrack.uri()).expect("youtrack builds"));
        let response = app_v2(yt, &hub).oneshot(v2_request(Some("Bearer bad"))).await.expect("ok");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(www_authenticate(&response).contains(r#"error="invalid_token""#));
        youtrack.verify().await;
    }

    #[tokio::test]
    async fn v2_with_a_sick_youtrack_is_502_not_401() {
        let hub = MockServer::start().await;
        let response = app_v2(dead_youtrack(), &hub)
            .oneshot(v2_request(Some("Bearer hub-token")))
            .await
            .expect("ok");
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn a_v2_tool_call_travels_with_the_hub_token_and_is_verified_once() {
        let hub = MockServer::start().await;
        let youtrack = MockServer::start().await;
        mock_whoami(&youtrack, 200, 1).await;
        Mock::given(method("GET"))
            .and(path("/api/admin/projects"))
            .and(wiremock::matchers::header("authorization", "Bearer hub-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .expect(2)
            .mount(&youtrack)
            .await;

        let yt = Arc::new(Youtrack::new(youtrack.uri()).expect("youtrack builds"));
        let app = app_v2(yt, &hub);
        for _ in 0..2 {
            let response =
                app.clone().oneshot(v2_request(Some("Bearer hub-token"))).await.expect("ok");
            let status = response.status();
            let body = axum::body::to_bytes(response.into_body(), 64 * 1024).await.expect("body");
            let body = String::from_utf8_lossy(&body);
            assert!(!body.contains("no YouTrack token"), "{body}");
            assert_eq!(status, StatusCode::OK, "{body}");
        }
        youtrack.verify().await;
    }

    /// Сервер без v2-настроек получает `X-API-Version: v2` и MCP_TOKEN: это
    /// v1, и в YouTrack обязан уйти `X-YouTrack-Token`, а не `s3cret`.
    #[tokio::test]
    async fn the_v2_header_is_ignored_when_v2_is_not_configured() {
        let youtrack = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/admin/projects"))
            .and(wiremock::matchers::header("authorization", "Bearer user-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .expect(1)
            .mount(&youtrack)
            .await;

        let yt = Arc::new(Youtrack::new(youtrack.uri()).expect("youtrack builds"));
        let request = tool_call_request_with(
            Some("Bearer s3cret"),
            &[("X-YouTrack-Token", "user-token"), ("X-API-Version", "v2")],
        );
        let response = app(yt, "s3cret".to_owned(), None, None).oneshot(request).await.expect("ok");
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024).await.expect("body");
        let body = String::from_utf8_lossy(&body);
        assert_eq!(status, StatusCode::OK, "{body}");
        youtrack.verify().await;
    }

    #[tokio::test]
    async fn v2_never_sends_the_mcp_token_to_youtrack() {
        let hub = MockServer::start().await;
        let youtrack = MockServer::start().await;
        Mock::given(path("/api/users/me"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&youtrack)
            .await;
        let yt = Arc::new(Youtrack::new(youtrack.uri()).expect("youtrack builds"));
        let response =
            app_v2(yt, &hub).oneshot(v2_request(Some("Bearer s3cret"))).await.expect("ok");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(www_authenticate(&response).contains(r#"error="invalid_token""#));
        youtrack.verify().await;
    }

    #[tokio::test]
    async fn v2_with_a_503_from_youtrack_is_502() {
        let hub = MockServer::start().await;
        let youtrack = MockServer::start().await;
        mock_whoami(&youtrack, 503, 1).await;
        let yt = Arc::new(Youtrack::new(youtrack.uri()).expect("youtrack builds"));
        let response =
            app_v2(yt, &hub).oneshot(v2_request(Some("Bearer hub-token"))).await.expect("ok");
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        youtrack.verify().await;
    }

    #[tokio::test]
    async fn v2_accepts_a_lowercase_bearer_scheme() {
        let hub = MockServer::start().await;
        let youtrack = MockServer::start().await;
        mock_whoami(&youtrack, 200, 1).await;
        Mock::given(path("/api/admin/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&youtrack)
            .await;
        let yt = Arc::new(Youtrack::new(youtrack.uri()).expect("youtrack builds"));
        let response =
            app_v2(yt, &hub).oneshot(v2_request(Some("bearer hub-token"))).await.expect("ok");
        assert_eq!(response.status(), StatusCode::OK);
        youtrack.verify().await;
    }

    #[tokio::test]
    async fn a_failed_check_is_not_cached() {
        let hub = MockServer::start().await;
        let youtrack = MockServer::start().await;
        let whoami = || Mock::given(method("GET")).and(path("/api/users/me"));
        whoami().respond_with(ResponseTemplate::new(502)).up_to_n_times(1).mount(&youtrack).await;
        whoami()
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":"1","login":"u"})))
            .mount(&youtrack)
            .await;
        Mock::given(path("/api/admin/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&youtrack)
            .await;
        let yt = Arc::new(Youtrack::new(youtrack.uri()).expect("youtrack builds"));
        let app = app_v2(yt, &hub);
        let first =
            app.clone().oneshot(v2_request(Some("Bearer hub-token"))).await.expect("ok").status();
        let second = app.oneshot(v2_request(Some("Bearer hub-token"))).await.expect("ok").status();
        assert_eq!(first, StatusCode::BAD_GATEWAY);
        assert_eq!(second, StatusCode::OK);
        let hits = youtrack
            .received_requests()
            .await
            .expect("recorded")
            .iter()
            .filter(|r| r.url.path() == "/api/users/me")
            .count();
        assert_eq!(hits, 2);
    }
}
