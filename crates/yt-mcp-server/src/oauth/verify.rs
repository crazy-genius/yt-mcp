//! Проверка Hub-токена на гейте `/mcp` в v2.

use yt_client::{Youtrack, YoutrackError};

use super::OAuth;

/// Что YouTrack думает о токене.
pub enum Verdict {
    Valid,
    /// Токен отвергнут: клиенту нужен новый.
    Invalid,
    /// Проверить не удалось (сеть, 5xx): это не вина токена, и 401 здесь
    /// заставил бы клиента зря гонять refresh.
    Unavailable,
}

/// `ApiError` yt-rs выдаёт только на 4xx, поэтому это «токен плохой».
pub fn classify(err: &YoutrackError) -> Verdict {
    match err {
        // ponytail: yt-rs не кладёт статус в `ApiError`, а плохой токен YouTrack
        // приходит именно им, поэтому любой `ApiError` считаем `Invalid`.
        // Потолок: 4xx вроде 404/429, разобранный как `ApiError`, даст ложный 401.
        // Апгрейд: сузить до 401/403, когда yt-rs начнёт отдавать статус.
        YoutrackError::ApiError(_) => Verdict::Invalid,
        YoutrackError::UnexpectedStatus { status, .. } if matches!(status.as_u16(), 401 | 403) => {
            Verdict::Invalid
        }
        _ => Verdict::Unavailable,
    }
}

/// Проверка через `whoami()` (поля `user_fields`, а не `fields=id`) — осознанно:
/// готовый метод вместо нового. Успех кэшируется на 60 с; переполнение кэша
/// не ошибка, просто не кэшируем.
pub async fn verify(youtrack: &Youtrack, oauth: &OAuth, token: &str) -> Verdict {
    // ponytail: ключ кэша — сам токен, он лежит в памяти процесса открытым текстом.
    // Потолок: дамп памяти. Апгрейд: ключ — SHA-256 токена.
    if oauth.verified.contains(token) {
        return Verdict::Valid;
    }
    // Сбой `as_user` — это неверный базовый URL (`Uri`), а не плохой токен.
    let service = match youtrack.as_user(token) {
        Ok(service) => service,
        Err(err) => return classify(&err),
    };
    match service.whoami().await {
        Ok(_) => {
            let _ = oauth.verified.insert(token.to_owned(), ());
            Verdict::Valid
        }
        Err(err) => classify(&err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    #[test]
    fn classify_separates_a_bad_token_from_a_sick_youtrack() {
        let s = |code| YoutrackError::UnexpectedStatus { status: code, body: String::new() };
        assert!(matches!(classify(&s(StatusCode::UNAUTHORIZED)), Verdict::Invalid));
        assert!(matches!(classify(&s(StatusCode::FORBIDDEN)), Verdict::Invalid));
        assert!(matches!(classify(&s(StatusCode::BAD_GATEWAY)), Verdict::Unavailable));
        assert!(matches!(classify(&s(StatusCode::NOT_FOUND)), Verdict::Unavailable));
        assert!(matches!(classify(&s(StatusCode::TOO_MANY_REQUESTS)), Verdict::Unavailable));
    }
}
