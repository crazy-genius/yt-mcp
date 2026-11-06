use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use url::Url;

use self::config::{HubEndpoints, OAuthSettings};
use self::ttl::TtlMap;

pub mod config;
mod endpoints;
pub mod redirect;
pub mod ttl;
pub mod verify;

/// Незавершённый вход: куда вернуть клиента и что он прислал в `state`.
pub struct Pending {
    pub client_redirect_uri: Url,
    pub client_state: Option<String>,
}

/// Клиент для всех запросов к Hub, включая обнаружение: без редиректов (запрос с
/// секретом не должен уйти на другой адрес) и с таймаутом, заданным админом
/// (`MCP_OAUTH_UPSTREAM_TIMEOUT_SECS`); connect-таймаут не больше 10 с.
pub fn http_client(timeout: Duration) -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(timeout.min(Duration::from_secs(10)))
        .timeout(timeout)
        .build()?)
}

/// Состояние OAuth-прокси перед YouTrack Hub.
pub struct OAuth {
    pub settings: OAuthSettings,
    pub hub: HubEndpoints,
    /// Без редиректов: ответ Hub на обмен кода не должен уводить запрос
    /// (с секретом) на другой адрес.
    pub http: reqwest::Client,
    /// Вход, начатый в `/authorize`, до возврата из Hub.
    pub pending: TtlMap<Pending>,
    /// Токены, уже проверенные в Hub (кэш).
    pub verified: TtlMap<()>,
}

impl OAuth {
    pub fn new(settings: OAuthSettings, hub: HubEndpoints) -> anyhow::Result<Self> {
        let http = http_client(settings.upstream_timeout)?;
        Ok(Self {
            settings,
            hub,
            http,
            pending: TtlMap::new(Duration::from_secs(600), 10_000),
            verified: TtlMap::new(Duration::from_secs(60), 10_000),
        })
    }

    /// Адрес, который Hub вернёт браузеру после входа.
    pub fn callback_url(&self) -> String {
        format!("{}/mcp/auth_callback", self.settings.public_url)
    }

    /// Куда указывает `WWW-Authenticate: resource_metadata=...`.
    pub fn resource_metadata_url(&self) -> String {
        format!("{}/.well-known/oauth-protected-resource/mcp", self.settings.public_url)
    }
}

/// Все OAuth-маршруты. Висят вне гейта уровня 1 (см. `app`).
pub fn routes(oauth: Arc<OAuth>) -> Router {
    endpoints::routes().with_state(oauth)
}

/// 256 бит из CSPRNG ОС, base64url без паддинга.
pub fn random_state() -> String {
    let mut bytes = [0u8; 32];
    // Отказ ОС выдать случайность — не ошибка, с которой можно жить: слабый
    // state открыл бы CSRF на вход.
    getrandom::fill(&mut bytes).expect("OS random source is unavailable");
    URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::random_state;

    #[test]
    fn state_is_256_bits_and_never_repeats() {
        let (a, b) = (random_state(), random_state());
        assert_eq!(a.len(), 43, "32 байта в base64url без паддинга");
        assert_ne!(a, b);
    }
}
