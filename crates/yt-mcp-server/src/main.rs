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
use yt_mcp_core::mcp::YoutrackMCPServer;

/// Конфигурация процесса. Системного YouTrack-токена здесь нет: доступ к
/// YouTrack всегда пер-пользовательский и приезжает заголовком
/// `X-YouTrack-Token` на каждый вызов тула.
struct Config {
    youtrack_url: String,
    mcp_token: String,
    bind: String,
    allowed_hosts: Option<Vec<String>>,
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

async fn gate(State(expected): State<Arc<String>>, request: Request, next: Next) -> Response {
    if authorized(request.headers(), &expected) {
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
fn app(youtrack: Arc<Youtrack>, mcp_token: String, allowed_hosts: Option<Vec<String>>) -> Router {
    // Дефолт `rmcp` — только loopback-хосты. Задан `MCP_ALLOWED_HOSTS` —
    // список заменяется целиком; не задан — дефолт остаётся как есть.
    let config = match allowed_hosts {
        Some(hosts) => StreamableHttpServerConfig::default().with_allowed_hosts(hosts),
        None => StreamableHttpServerConfig::default(),
    };
    let mcp = StreamableHttpService::new(
        move || Ok(YoutrackMCPServer::new(youtrack.clone())),
        LocalSessionManager::default().into(),
        config,
    );

    Router::new()
        .nest_service("/mcp", mcp)
        .layer(middleware::from_fn_with_state(Arc::new(mcp_token), gate))
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

    let app = app(youtrack, config.mcp_token, config.allowed_hosts);

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
        Router::new()
            .route("/mcp", axum::routing::get(|| async { "reached" }))
            .layer(middleware::from_fn_with_state(Arc::new("s3cret".to_owned()), gate))
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
    fn tool_call_request(auth: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header(header::HOST, "127.0.0.1")
            .header("X-YouTrack-Token", "user-token")
            .header("MCP-Protocol-Version", "2026-07-28")
            .header("Mcp-Method", "tools/call")
            .header("Mcp-Name", "youtrack_list_projects")
            .header(header::ACCEPT, "application/json, text/event-stream")
            .header(header::CONTENT_TYPE, "application/json");
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
        let response = app(dead_youtrack(), "s3cret".to_owned(), None)
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
        let response = app(yt, "s3cret".to_owned(), None)
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
}
