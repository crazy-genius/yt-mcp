//! Настройки OAuth v2 из окружения и обнаружение endpoint-ов YouTrack Hub.

use std::fmt;
use std::time::Duration;

use anyhow::{Context, bail};
use serde::Deserialize;
use url::Url;

use super::redirect::TrustedRedirects;

/// Таймаут запросов к Hub по умолчанию.
pub const DEFAULT_UPSTREAM_TIMEOUT: Duration = Duration::from_secs(8);

/// Настройки v2. Секрет Hub-клиента не выводится ни в `Debug`, ни в ошибки.
pub struct OAuthSettings {
    /// Публичный адрес сервера без завершающего `/`.
    pub public_url: String,
    pub hub_client_id: String,
    /// `None` — публичный клиент (без секрета).
    pub hub_client_secret: Option<String>,
    pub redirects: TrustedRedirects,
    /// Общий таймаут запроса к Hub (1..=60 с); задаётся `MCP_OAUTH_UPSTREAM_TIMEOUT_SECS`.
    pub upstream_timeout: Duration,
}

impl fmt::Debug for OAuthSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthSettings")
            .field("public_url", &self.public_url)
            .field("hub_client_id", &self.hub_client_id)
            .field("hub_client_secret", &self.hub_client_secret.as_ref().map(|_| "<redacted>"))
            .field("redirects", &self.redirects)
            .field("upstream_timeout", &self.upstream_timeout)
            .finish()
    }
}

/// Читает настройки через `get` (в тестах — замыкание над `HashMap`).
///
/// Пустое и пробельное значение — «не задано». Ни одной из трёх обязательных
/// переменных — v2 выключен (`Ok(None)`); часть — ошибка с именем недостающей.
/// `HUB_CLIENT_SECRET` не участвует в решении «включён ли v2».
pub fn settings_from(
    get: impl Fn(&str) -> Option<String>,
) -> anyhow::Result<Option<OAuthSettings>> {
    let var = |name: &str| get(name).map(|v| v.trim().to_owned()).filter(|v| !v.is_empty());
    let public_url = var("MCP_PUBLIC_URL");
    let hub_client_id = var("HUB_CLIENT_ID");
    let redirects = var("MCP_OAUTH_REDIRECT_URIS");
    let secret = var("HUB_CLIENT_SECRET");

    if public_url.is_none() && hub_client_id.is_none() && redirects.is_none() {
        return Ok(None);
    }
    let need = |v: Option<String>, name: &str| {
        v.with_context(|| format!("{name} is required when OAuth v2 is configured"))
    };
    let public_url = checked_public_url(&need(public_url, "MCP_PUBLIC_URL")?)?;
    let hub_client_id = need(hub_client_id, "HUB_CLIENT_ID")?;
    let redirects = TrustedRedirects::parse(&need(redirects, "MCP_OAUTH_REDIRECT_URIS")?)
        .context("MCP_OAUTH_REDIRECT_URIS")?;

    let upstream_timeout = match var("MCP_OAUTH_UPSTREAM_TIMEOUT_SECS") {
        None => DEFAULT_UPSTREAM_TIMEOUT,
        Some(raw) => match raw.parse::<u64>() {
            Ok(secs) if (1..=60).contains(&secs) => Duration::from_secs(secs),
            _ => bail!("MCP_OAUTH_UPSTREAM_TIMEOUT_SECS must be an integer 1..=60, got: {raw}"),
        },
    };

    Ok(Some(OAuthSettings {
        public_url,
        hub_client_id,
        hub_client_secret: secret,
        redirects,
        upstream_timeout,
    }))
}

/// `https`, либо `http` только для loopback (локальная разработка).
fn checked_public_url(raw: &str) -> anyhow::Result<String> {
    let url = Url::parse(raw).with_context(|| format!("MCP_PUBLIC_URL is not a URL: {raw}"))?;
    let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    match url.scheme() {
        "https" => {}
        "http" if loopback => {}
        _ => bail!("MCP_PUBLIC_URL must be https (http is allowed only for loopback): {raw}"),
    }
    // Маршруты висят в корне, а issuer/resource/callback клеятся строкой: путь, query,
    // fragment и userinfo дали бы битые адреса.
    if url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        bail!("MCP_PUBLIC_URL must be an origin without path, query, fragment or userinfo: {raw}");
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

/// `https`, либо `http` только для loopback.
fn ensure_https_endpoint(url: &Url, name: &str) -> anyhow::Result<()> {
    let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    anyhow::ensure!(
        url.scheme() == "https" || (url.scheme() == "http" && loopback),
        "Hub {name} must be https (http is allowed only for loopback): {url}"
    );
    Ok(())
}

/// Endpoint-ы Hub, найденные через YouTrack.
#[derive(Debug)]
pub struct HubEndpoints {
    pub authorization_endpoint: Url,
    pub token_endpoint: Url,
    /// `serviceId` YouTrack в Hub — значение параметра `scope`.
    pub scope: String,
}

#[derive(Deserialize)]
struct YoutrackConfig {
    ring: Ring,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Ring {
    url: String,
    service_id: String,
}

#[derive(Deserialize)]
struct HubMetadata {
    authorization_endpoint: Url,
    token_endpoint: Url,
}

/// Находит Hub: `/api/config` YouTrack отдаёт его адрес и `serviceId`, а
/// метаданные OAuth (RFC 8414) — сам Hub. Любой сбой — ошибка с URL запроса.
pub async fn discover(http: &reqwest::Client, youtrack_url: &str) -> anyhow::Result<HubEndpoints> {
    let base = youtrack_url.trim_end_matches('/');
    let config_url = format!("{base}/api/config?fields=ring(url,serviceId)");
    let config: YoutrackConfig = get_json(http, &config_url).await?;

    let hub = Url::parse(youtrack_url)
        .with_context(|| format!("invalid YouTrack URL: {youtrack_url}"))?
        .join(&config.ring.url)
        .with_context(|| format!("invalid Hub URL from {config_url}: {}", config.ring.url))?;
    let meta_url =
        format!("{}/.well-known/oauth-authorization-server", hub.as_str().trim_end_matches('/'));
    let meta: HubMetadata = get_json(http, &meta_url).await?;

    ensure_https_endpoint(&meta.authorization_endpoint, "authorization_endpoint")?;
    ensure_https_endpoint(&meta.token_endpoint, "token_endpoint")?;
    Ok(HubEndpoints {
        authorization_endpoint: meta.authorization_endpoint,
        token_endpoint: meta.token_endpoint,
        scope: config.ring.service_id,
    })
}

async fn get_json<T: serde::de::DeserializeOwned>(
    http: &reqwest::Client,
    url: &str,
) -> anyhow::Result<T> {
    let response = http.get(url).send().await.with_context(|| format!("GET {url} failed"))?;
    let status = response.status();
    if !status.is_success() {
        bail!("GET {url} returned {status}");
    }
    response.json().await.with_context(|| format!("GET {url}: unexpected response body"))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    const FULL: [(&str, &str); 3] = [
        ("MCP_PUBLIC_URL", "https://yt-mcp.dev"),
        ("HUB_CLIENT_ID", "hub-client"),
        ("MCP_OAUTH_REDIRECT_URIS", "http://localhost/callback"),
    ];

    /// Полный набор, поверх которого перезаписываются отдельные ключи.
    fn env(over: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let mut map: HashMap<String, String> = HashMap::new();
        for (k, v) in FULL.iter().chain(over) {
            map.insert((*k).into(), (*v).into());
        }
        move |k| map.get(k).cloned()
    }

    fn only(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> =
            pairs.iter().map(|(k, v)| ((*k).into(), (*v).into())).collect();
        move |k| map.get(k).cloned()
    }

    #[test]
    fn v2_is_off_when_nothing_is_set_and_an_error_when_half_is_set() {
        assert!(settings_from(only(&[])).unwrap().is_none());
        // секрет сам по себе v2 не включает
        assert!(settings_from(only(&[("HUB_CLIENT_SECRET", "x")])).unwrap().is_none());
        let err = settings_from(only(&[("MCP_PUBLIC_URL", "https://yt-mcp.dev")])).unwrap_err();
        assert!(err.to_string().contains("HUB_CLIENT_ID"), "{err}");
    }

    #[test]
    fn the_upstream_timeout_defaults_to_8_seconds_and_is_bounded() {
        let s = settings_from(env(&[])).unwrap().unwrap();
        assert_eq!(s.upstream_timeout, Duration::from_secs(8));
        let s = settings_from(env(&[("MCP_OAUTH_UPSTREAM_TIMEOUT_SECS", "20")])).unwrap().unwrap();
        assert_eq!(s.upstream_timeout, Duration::from_secs(20));
        let s = settings_from(env(&[("MCP_OAUTH_UPSTREAM_TIMEOUT_SECS", " ")])).unwrap().unwrap();
        assert_eq!(s.upstream_timeout, Duration::from_secs(8));
        for bad in ["0", "61", "abc", "-1"] {
            let err = settings_from(env(&[("MCP_OAUTH_UPSTREAM_TIMEOUT_SECS", bad)])).unwrap_err();
            assert!(err.to_string().contains("MCP_OAUTH_UPSTREAM_TIMEOUT_SECS"), "{bad}: {err}");
        }
    }

    #[test]
    fn an_empty_secret_means_a_public_client() {
        let s = settings_from(env(&[("HUB_CLIENT_SECRET", "  ")])).unwrap().unwrap();
        assert!(s.hub_client_secret.is_none());
    }

    #[test]
    fn the_public_url_must_be_https_except_loopback() {
        assert!(settings_from(env(&[("MCP_PUBLIC_URL", "http://yt-mcp.dev")])).is_err());
        let s =
            settings_from(env(&[("MCP_PUBLIC_URL", "http://127.0.0.1:8080/")])).unwrap().unwrap();
        assert_eq!(s.public_url, "http://127.0.0.1:8080");
    }

    #[test]
    fn the_public_url_must_be_a_bare_origin() {
        for bad in ["https://host/yt", "https://host/?x=1", "https://host/#f", "https://u:p@host"] {
            assert!(settings_from(env(&[("MCP_PUBLIC_URL", bad)])).is_err(), "{bad}");
        }
        let s = settings_from(env(&[("MCP_PUBLIC_URL", "https://yt-mcp.dev/")])).unwrap().unwrap();
        assert_eq!(s.public_url, "https://yt-mcp.dev");
    }

    #[tokio::test]
    async fn discover_refuses_a_plain_http_hub_endpoint() {
        let body = serde_json::json!({
            "authorization_endpoint": "https://hub.example/auth",
            "token_endpoint": "http://hub.example/token",
        });
        let mock = hub_mock(ResponseTemplate::new(200).set_body_json(body)).await;
        let err = discover(&reqwest::Client::new(), &mock.uri()).await.unwrap_err();
        assert!(err.to_string().contains("token_endpoint"), "{err}");
    }

    #[test]
    fn the_secret_never_shows_in_debug() {
        let s = settings_from(env(&[("HUB_CLIENT_SECRET", "hub-s3cret")])).unwrap().unwrap();
        assert!(!format!("{s:?}").contains("hub-s3cret"));
    }

    async fn hub_mock(metadata: ResponseTemplate) -> MockServer {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/config"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    serde_json::json!({"ring": {"url": "/hub", "serviceId": "svc-1"}}),
                ),
            )
            .mount(&mock)
            .await;
        Mock::given(method("GET"))
            .and(path("/hub/.well-known/oauth-authorization-server"))
            .respond_with(metadata)
            .mount(&mock)
            .await;
        mock
    }

    #[tokio::test]
    async fn discover_reads_the_scope_and_the_hub_endpoints() {
        let mock = MockServer::start().await;
        let body = serde_json::json!({
            "authorization_endpoint": format!("{}/hub/auth", mock.uri()),
            "token_endpoint": format!("{}/hub/token", mock.uri()),
        });
        Mock::given(method("GET"))
            .and(path("/api/config"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    serde_json::json!({"ring": {"url": "/hub", "serviceId": "svc-1"}}),
                ),
            )
            .mount(&mock)
            .await;
        Mock::given(method("GET"))
            .and(path("/hub/.well-known/oauth-authorization-server"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&mock)
            .await;

        let hub = discover(&reqwest::Client::new(), &mock.uri()).await.unwrap();
        assert_eq!(hub.scope, "svc-1");
        assert_eq!(hub.authorization_endpoint.path(), "/hub/auth");
        assert_eq!(hub.token_endpoint.path(), "/hub/token");
    }

    #[tokio::test]
    async fn discover_fails_loudly_when_the_hub_has_no_metadata() {
        let mock = hub_mock(ResponseTemplate::new(404)).await;
        let err = discover(&reqwest::Client::new(), &mock.uri()).await.unwrap_err();
        assert!(err.to_string().contains("oauth-authorization-server"), "{err}");
    }
}
