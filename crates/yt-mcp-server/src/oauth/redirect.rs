//! Доверенные redirect URI клиентов (RFC 8252 §7.3 для loopback).

use anyhow::{Context, bail};
use url::Url;

/// Список redirect URI, на которые разрешено возвращать код авторизации.
#[derive(Debug)]
pub struct TrustedRedirects(Vec<Url>);

impl TrustedRedirects {
    /// Список через запятую; пробелы обрезаются, пустые элементы пропускаются.
    /// Невалидный URL или пустой итог — ошибка (процесс не должен стартовать).
    pub fn parse(raw: &str) -> anyhow::Result<Self> {
        let urls = raw
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| Url::parse(s).with_context(|| format!("invalid redirect URI: {s}")))
            .collect::<anyhow::Result<Vec<_>>>()?;
        if urls.is_empty() {
            bail!("the trusted redirect URI list is empty");
        }
        Ok(Self(urls))
    }

    /// Доверяем ли `candidate`. Фрагмент и userinfo — отказ. Для записи
    /// `http` + loopback порт кандидата не важен (клиент берёт эфемерный порт),
    /// всё остальное — точное совпадение.
    pub fn allows(&self, candidate: &str) -> bool {
        let Ok(candidate) = Url::parse(candidate) else {
            return false;
        };
        if candidate.fragment().is_some()
            || !candidate.username().is_empty()
            || candidate.password().is_some()
        {
            return false;
        }
        self.0.iter().any(|allowed| {
            if is_loopback_http(allowed) {
                let mut c = candidate.clone();
                // set_port не падает для http-URL с хостом
                c.set_port(allowed.port()).is_ok() && c == *allowed
            } else {
                candidate == *allowed
            }
        })
    }
}

fn is_loopback_http(url: &Url) -> bool {
    url.scheme() == "http" && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trusted() -> TrustedRedirects {
        TrustedRedirects::parse(
            "http://localhost/callback, http://127.0.0.1/callback, https://claude.ai/api/mcp/auth_callback",
        )
        .unwrap()
    }

    #[test]
    fn loopback_matches_any_port_but_the_exact_path() {
        assert!(trusted().allows("http://localhost:53682/callback"));
        assert!(trusted().allows("http://127.0.0.1:9/callback"));
        assert!(!trusted().allows("http://localhost:53682/other"));
        assert!(!trusted().allows("https://localhost:53682/callback"), "схема тоже точная");
    }

    #[test]
    fn a_non_loopback_entry_matches_only_exactly() {
        assert!(trusted().allows("https://claude.ai/api/mcp/auth_callback"));
        assert!(!trusted().allows("https://claude.ai:8443/api/mcp/auth_callback"));
        assert!(!trusted().allows("https://evil.example/api/mcp/auth_callback"));
    }

    #[test]
    fn fragment_and_userinfo_are_refused() {
        assert!(!trusted().allows("https://claude.ai/api/mcp/auth_callback#x"));
        assert!(!trusted().allows("http://u:p@localhost:1/callback"));
        assert!(!trusted().allows("not a url"));
    }

    #[test]
    fn an_empty_or_broken_list_is_a_startup_error() {
        assert!(TrustedRedirects::parse(" , ").is_err());
        assert!(TrustedRedirects::parse("http://ok/cb,::bad").is_err());
    }
}
