//! Медиа (картинки) для vision-режима: разбор входных данных, кэш в
//! временном каталоге и сериализация в сообщения апстрима.
//!
//! Модуль ничего не знает про HTTP и БД — только про байты, файлы и
//! формат сообщений OpenAI/Ollama. Поэтому его можно тестировать без
//! сети и без Postgres.
//!
//! Жизненный цикл картинки зависит от `save_media` в запросе:
//! * `save_media = false` (временный чат) — картинка лежит в кэше
//!   [`VisionStore::root`] и когда-нибудь вытесняется по TTL;
//! * `save_media = true` (постоянный чат) — base64 дополнительно
//!   пишется в БД, и картинка переживает перезапуск и чистку кэша.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Максимальный размер картинки в байтах (до декодирования base64).
pub const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;
/// Сколько живёт файл в кэше по умолчанию.
pub const DEFAULT_MEDIA_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// Подкаталог временного каталога, где лежат картинки.
pub const MEDIA_DIR_NAME: &str = "obscape-media";

/// MIME-типы, которые принимаем. Всё остальное — ошибка запроса.
const ALLOWED_MIME: [&str; 4] = ["image/png", "image/jpeg", "image/webp", "image/gif"];

/// Ошибки разбора/хранения медиа. Наружу уходят как HTTP 400.
#[derive(Debug)]
pub enum VisionError {
    /// Входные данные не являются base64/data-URL картинки.
    Invalid(String),
    /// Картинка больше [`MAX_IMAGE_BYTES`].
    TooLarge(usize),
    /// Ошибка работы с кэшем на диске.
    Io(std::io::Error),
}

impl std::fmt::Display for VisionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VisionError::Invalid(m) => write!(f, "invalid image: {m}"),
            VisionError::TooLarge(n) => {
                write!(f, "image is too large ({n} bytes, max {MAX_IMAGE_BYTES})")
            }
            VisionError::Io(e) => write!(f, "media cache error: {e}"),
        }
    }
}

impl std::error::Error for VisionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            VisionError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for VisionError {
    fn from(e: std::io::Error) -> Self {
        VisionError::Io(e)
    }
}

/// Разобранная картинка: содержимое, MIME и детерминированный `id`.
///
/// `id` — hex sha256 от байтов картинки, поэтому одна и та же картинка
/// в кэше лежит одним файлом, а `save_media = false` не плодит дубли.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisionInput {
    pub id: String,
    pub mime: String,
    pub bytes: Vec<u8>,
}

impl VisionInput {
    /// Разобрать `image` из запроса: data-URL (`data:image/png;base64,...`)
    /// или голый base64 (тогда MIME определяется по сигнатуре файла).
    ///
    /// Размер проверяется до декодирования: base64 на четверть длиннее
    /// исходных байтов, поэтому лимит по длине строки — [`MAX_IMAGE_BYTES`]
    /// с запасом на 4/3.
    pub fn parse(raw: &str) -> Result<Self, VisionError> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(VisionError::Invalid("empty image".into()));
        }

        // Отсекаем по длине до декодирования, чтобы не держать в памяти
        // гигабайтную строку.
        let b64_limit = MAX_IMAGE_BYTES / 3 * 4 + 16;
        if raw.len() > b64_limit {
            return Err(VisionError::TooLarge(raw.len()));
        }

        let (mime_hint, payload) = match raw.strip_prefix("data:") {
            Some(rest) => {
                let (meta, data) = rest
                    .split_once(',')
                    .ok_or_else(|| VisionError::Invalid("malformed data URL".into()))?;
                if !meta.ends_with(";base64") {
                    return Err(VisionError::Invalid("data URL is not base64".into()));
                }
                let mime = meta.trim_end_matches(";base64").trim().to_ascii_lowercase();
                (Some(mime), data)
            }
            None => (None, raw),
        };

        let bytes = BASE64
            .decode(payload.trim())
            .map_err(|e| VisionError::Invalid(format!("bad base64: {e}")))?;
        if bytes.is_empty() {
            return Err(VisionError::Invalid("empty image".into()));
        }
        if bytes.len() > MAX_IMAGE_BYTES {
            return Err(VisionError::TooLarge(bytes.len()));
        }

        let sniffed = sniff_mime(&bytes)
            .map(|m| m.to_string())
            .ok_or_else(|| VisionError::Invalid("unsupported image format".into()))?;
        // Если MIME задан явно — он должен совпадать с сигнатурой файла,
        // иначе data-URL противоречит содержимому.
        if let Some(hint) = mime_hint
            && !hint.is_empty()
            && hint != sniffed
            && !(hint == "image/jpg" && sniffed == "image/jpeg")
        {
            return Err(VisionError::Invalid(format!(
                "data URL says {hint}, but the bytes look like {sniffed}"
            )));
        }

        Ok(VisionInput {
            id: content_id(&bytes),
            mime: sniffed,
            bytes,
        })
    }

    /// data-URL для `image_url` в OpenAI-совместимом запросе.
    pub fn to_data_url(&self) -> String {
        format!("data:{};base64,{}", self.mime, BASE64.encode(&self.bytes))
    }
}

/// Детерминированный id — hex sha256 от содержимого.
pub fn content_id(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// MIME по «магическим» байтам. `None` — не поддерживаемый формат.
pub fn sniff_mime(bytes: &[u8]) -> Option<&'static str> {
    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if bytes.starts_with(PNG) {
        return Some("image/png");
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some("image/jpeg");
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    None
}

/// Проверка, что MIME можно отдавать наружу как `Content-Type`.
pub fn is_allowed_mime(mime: &str) -> bool {
    ALLOWED_MIME.contains(&mime)
}

/// Кэш картинок на диске. Корень по умолчанию —
/// `<временный каталог>/obscape-media`, права файлов `0600`.
#[derive(Debug, Clone)]
pub struct VisionStore {
    root: PathBuf,
    ttl: Duration,
}

impl Default for VisionStore {
    fn default() -> Self {
        Self::new(std::env::temp_dir().join(MEDIA_DIR_NAME), DEFAULT_MEDIA_TTL)
    }
}

impl VisionStore {
    /// Кэш в конкретном каталоге (нужно тестам).
    pub fn new(root: PathBuf, ttl: Duration) -> Self {
        VisionStore { root, ttl }
    }

    /// Корень кэша. Полезно для логов и диагностики.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// TTL файлов кэша.
    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// Путь файла для картинки с данным `id`.
    pub fn path_for(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }

    /// Сохранить картинку в кэш и вернуть путь. Запись атомарная:
    /// временный файл + `rename`, чтобы читатель никогда не увидел
    /// недописанный файл.
    pub fn store(&self, img: &VisionInput) -> Result<PathBuf, VisionError> {
        std::fs::create_dir_all(&self.root)?;
        let path = self.path_for(&img.id);
        if path.exists() {
            return Ok(path);
        }
        let tmp = self.root.join(format!(".{}.tmp", img.id));
        {
            let mut f = std::fs::File::create(&tmp)?;
            restrict_permissions(&f)?;
            f.write_all(&img.bytes)?;
            f.flush()?;
        }
        std::fs::rename(&tmp, &path)?;
        Ok(path)
    }

    /// Прочитать картинку из кэша. `None` — файла нет (вытеснен по TTL
    /// или временный чат пережил чистку).
    pub fn load(&self, id: &str) -> Option<Vec<u8>> {
        if !is_safe_id(id) {
            return None;
        }
        std::fs::read(self.path_for(id)).ok()
    }

    /// Сохранить, если ещё не в кэше, и вернуть `id`.
    pub fn put(&self, img: &VisionInput) -> Result<String, VisionError> {
        self.store(img)?;
        Ok(img.id.clone())
    }

    /// Удалить файлы старше TTL. Вызывается лениво при записи и на
    /// старте сервера: фоновый таймер не нужен, а незакрытых дескрипторов
    /// у нас нет, поэтому удаление безопасно даже для читаемых файлов
    /// на Unix.
    ///
    /// Возвращает количество удалённых файлов.
    pub fn evict_expired(&self) -> std::io::Result<usize> {
        let entries = match std::fs::read_dir(&self.root) {
            Ok(e) => e,
            // Каталога ещё нет — чистить нечего.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e),
        };
        let mut removed = 0;
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            let expired = meta
                .modified()
                .ok()
                .and_then(|m| m.elapsed().ok())
                .is_some_and(|age| age > self.ttl);
            if expired && std::fs::remove_file(entry.path()).is_ok() {
                removed += 1;
            }
        }
        Ok(removed)
    }
}

/// `id` попадает в имя файла, поэтому разрешаем только то, что генерируем
/// сами (hex sha256). Защита от `../` в запросе `/v1/media/{id}`.
pub fn is_safe_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Права `0600` на картинку: в каталоге лежат пользовательские данные.
fn restrict_permissions(file: &std::fs::File) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        let _ = file;
    }
    Ok(())
}

/// Картинка, лежащая в БД (`messages.media`, при `save_media = true`).
///
/// Хранится как отдельный объект, чтобы строка в БД знала MIME и могла
/// отдать файл, даже если кэш уже вычищен.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredMedia {
    pub id: String,
    pub mime: String,
    /// base64 без префикса data-URL.
    pub data: String,
}

impl StoredMedia {
    /// Собрать запись для БД из разобранной картинки.
    pub fn from_input(img: &VisionInput) -> Self {
        StoredMedia {
            id: img.id.clone(),
            mime: img.mime.clone(),
            data: BASE64.encode(&img.bytes),
        }
    }

    /// Вернуть исходные байты.
    pub fn decode(&self) -> Result<Vec<u8>, VisionError> {
        BASE64
            .decode(&self.data)
            .map_err(|e| VisionError::Invalid(format!("bad base64 in stored media: {e}")))
    }

    /// data-URL — то же представление, что и у [`VisionInput`].
    pub fn to_data_url(&self) -> String {
        format!("data:{};base64,{}", self.mime, self.data)
    }
}

/// Часть содержимого сообщения для апстрима. Для текстовых сообщений —
/// [`LlmContent::Text`], для vision — массив частей `text` + `image_url`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LlmContent {
    Text { text: String },
    ImageUrl { image_url: LlmImageUrl },
}

/// Обёртка над URL картинки (data-URL или ссылка на `/v1/media/{id}`).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LlmImageUrl {
    pub url: String,
}

impl LlmContent {
    /// Текстовая часть.
    pub fn text(text: impl Into<String>) -> Self {
        LlmContent::Text { text: text.into() }
    }

    /// Картинка как data-URL.
    pub fn image_data_url(data_url: impl Into<String>) -> Self {
        LlmContent::ImageUrl {
            image_url: LlmImageUrl {
                url: data_url.into(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD as B64;

    /// Минимальный валидный PNG (сигнатура + пустые данные): для наших
    /// проверок важна только сигнатура.
    fn png_bytes() -> Vec<u8> {
        let mut v = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        v.extend_from_slice(b"fake-image-body");
        v
    }

    fn temp_store() -> (VisionStore, PathBuf) {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("obscape-vision-{}-{nanos}", std::process::id()));
        (VisionStore::new(dir.clone(), Duration::from_secs(60)), dir)
    }

    #[test]
    fn parses_data_url() {
        let b64 = B64.encode(png_bytes());
        let img = VisionInput::parse(&format!("data:image/png;base64,{b64}")).unwrap();
        assert_eq!(img.mime, "image/png");
        assert_eq!(img.bytes, png_bytes());
        assert_eq!(img.id.len(), 64);
        assert!(img.to_data_url().starts_with("data:image/png;base64,"));
    }

    #[test]
    fn parses_bare_base64_and_sniffs_mime() {
        let img = VisionInput::parse(&B64.encode(png_bytes())).unwrap();
        assert_eq!(img.mime, "image/png");
    }

    #[test]
    fn id_is_stable_and_content_addressed() {
        let a = VisionInput::parse(&B64.encode(png_bytes())).unwrap();
        let b = VisionInput::parse(&B64.encode(png_bytes())).unwrap();
        assert_eq!(a.id, b.id);
    }

    #[test]
    fn rejects_broken_base64() {
        let err = VisionInput::parse("data:image/png;base64,не-base64!!!").unwrap_err();
        assert!(matches!(err, VisionError::Invalid(_)), "{err}");
    }

    #[test]
    fn rejects_unknown_format() {
        let err = VisionInput::parse(&B64.encode(b"just some text")).unwrap_err();
        assert!(matches!(err, VisionError::Invalid(_)), "{err}");
    }

    #[test]
    fn rejects_mime_mismatch() {
        let b64 = B64.encode(png_bytes());
        let err = VisionInput::parse(&format!("data:image/jpeg;base64,{b64}")).unwrap_err();
        assert!(err.to_string().contains("bytes look like"), "{err}");
    }

    #[test]
    fn rejects_oversized_payload() {
        let huge = "A".repeat(MAX_IMAGE_BYTES / 3 * 4 + 32);
        let err = VisionInput::parse(&format!("data:image/png;base64,{huge}")).unwrap_err();
        assert!(matches!(err, VisionError::TooLarge(_)), "{err}");
    }

    #[test]
    fn rejects_empty() {
        assert!(VisionInput::parse("   ").is_err());
    }

    #[test]
    fn store_load_round_trip() {
        let (store, dir) = temp_store();
        let img = VisionInput::parse(&B64.encode(png_bytes())).unwrap();
        let path = store.store(&img).unwrap();
        assert_eq!(path, store.path_for(&img.id));
        assert_eq!(store.load(&img.id).unwrap(), png_bytes());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        // Второй вызов — идемпотентен, файл тот же.
        assert_eq!(store.store(&img).unwrap(), path);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_missing_and_unsafe_ids() {
        let (store, dir) = temp_store();
        assert!(store.load("deadbeef").is_none());
        assert!(store.load("../etc/passwd").is_none());
        assert!(!is_safe_id("../etc/passwd"));
        assert!(is_safe_id("abc123"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn evict_removes_expired_only() {
        let (store, dir) = temp_store();
        let img = VisionInput::parse(&B64.encode(png_bytes())).unwrap();
        store.store(&img).unwrap();
        // TTL 60 c — свежий файл остаётся.
        assert_eq!(store.evict_expired().unwrap(), 0);
        assert!(store.load(&img.id).is_some());

        // Кэш без каталога — не ошибка.
        let empty = VisionStore::new(dir.join("nope"), Duration::from_secs(1));
        assert_eq!(empty.evict_expired().unwrap(), 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn stored_media_round_trips_through_base64() {
        let img = VisionInput::parse(&B64.encode(png_bytes())).unwrap();
        let stored = StoredMedia::from_input(&img);
        assert_eq!(stored.id, img.id);
        assert_eq!(stored.mime, "image/png");
        assert_eq!(stored.decode().unwrap(), png_bytes());
        assert_eq!(stored.to_data_url(), img.to_data_url());

        // JSON-строкой переживает запись/чтение в БД.
        let json = serde_json::to_string(&stored).unwrap();
        let back: StoredMedia = serde_json::from_str(&json).unwrap();
        assert_eq!(back, stored);
    }

    #[test]
    fn llm_content_serializes_as_openai_parts() {
        let text = serde_json::to_value(LlmContent::text("hi")).unwrap();
        assert_eq!(text, serde_json::json!({"type": "text", "text": "hi"}));

        let image =
            serde_json::to_value(LlmContent::image_data_url("data:image/png;base64,AAA")).unwrap();
        assert_eq!(
            image,
            serde_json::json!({
                "type": "image_url",
                "image_url": {"url": "data:image/png;base64,AAA"}
            })
        );
    }

    #[test]
    fn sniff_mime_covers_known_formats() {
        assert_eq!(sniff_mime(&[0xff, 0xd8, 0xff, 0xe0]), Some("image/jpeg"));
        assert_eq!(sniff_mime(b"RIFF1234WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff_mime(b"GIF89a..."), Some("image/gif"));
        assert_eq!(sniff_mime(b"nope"), None);
    }

    #[test]
    fn allowed_mime_whitelist() {
        assert!(is_allowed_mime("image/png"));
        assert!(is_allowed_mime("image/gif"));
        assert!(!is_allowed_mime("image/svg+xml"));
        assert!(!is_allowed_mime(""));
    }
}
