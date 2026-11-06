//! Хранилище в памяти с TTL и потолком числа записей.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Потолок достигнут: новую запись принять нельзя.
#[derive(Debug)]
pub struct Full;

/// Карта «ключ → значение» с временем жизни записи и потолком `cap`.
/// Нужна для незавершённых входов и кэша токенов: без потолка аноним мог бы
/// забить память запросами, которые никогда не будут завершены.
pub struct TtlMap<V> {
    inner: Mutex<HashMap<String, (Instant, V)>>,
    ttl: Duration,
    cap: usize,
}

impl<V> TtlMap<V> {
    pub fn new(ttl: Duration, cap: usize) -> Self {
        Self { inner: Mutex::new(HashMap::new()), ttl, cap }
    }

    /// Запись свежая, пока `inserted.elapsed() < ttl`; при `ttl = 0` протухает сразу.
    fn fresh(&self, inserted: &Instant) -> bool {
        inserted.elapsed() < self.ttl
    }

    /// Кладёт запись. Перед вставкой выкидывает протухшие; если и после этого
    /// записей `cap` или больше — `Err(Full)`.
    // ponytail: чистка O(n) на каждую вставку, потолок — `cap` (10 000 записей).
    // Апгрейд — фоновая чистка или крейт с TTL.
    pub fn insert(&self, key: String, value: V) -> Result<(), Full> {
        let mut map = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        map.retain(|_, (inserted, _)| self.fresh(inserted));
        if map.len() >= self.cap {
            return Err(Full);
        }
        map.insert(key, (Instant::now(), value));
        Ok(())
    }

    /// Забирает запись (одноразово), только если она ещё свежая.
    pub fn take(&self, key: &str) -> Option<V> {
        let mut map = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let (inserted, value) = map.remove(key)?;
        self.fresh(&inserted).then_some(value)
    }

    /// Есть ли свежая запись; ничего не удаляет.
    pub fn contains(&self, key: &str) -> bool {
        let map = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        map.get(key).is_some_and(|(inserted, _)| self.fresh(inserted))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn take_is_one_shot() {
        let m = TtlMap::new(Duration::from_secs(60), 10);
        m.insert("k".into(), 1).unwrap();
        assert_eq!(m.take("k"), Some(1));
        assert_eq!(m.take("k"), None);
    }

    #[test]
    fn an_expired_entry_is_gone() {
        let m = TtlMap::new(Duration::ZERO, 10);
        m.insert("k".into(), 1).unwrap();
        assert!(!m.contains("k"));
        assert_eq!(m.take("k"), None);
    }

    #[test]
    fn the_cap_refuses_a_new_entry_but_expired_ones_free_room() {
        let m = TtlMap::new(Duration::from_secs(60), 1);
        m.insert("a".into(), 1).unwrap();
        assert!(m.insert("b".into(), 2).is_err());
        let z = TtlMap::new(Duration::ZERO, 1);
        z.insert("a".into(), 1).unwrap();
        assert!(z.insert("b".into(), 2).is_ok(), "протухшая запись не должна держать место");
    }
}
