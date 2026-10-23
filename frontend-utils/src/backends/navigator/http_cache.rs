//! A small on-disk HTTP cache for `GET` requests.
//!
//! Flash Player relied on the browser's cache, so content that downloads many
//! resources at startup (UI SWFs, map tiles, sprites) loads them from disk on
//! later runs. Without a browser, Ruffle would download everything every time.
//!
//! Freshness follows RFC 9111 for responses without `Cache-Control`:
//! - URLs whose file name ends in a content hash (`name_1a2b3c4d.ext`) never
//!   change, so they are served from disk without asking the server;
//! - other responses are fresh for 10% of the time since `Last-Modified`
//!   (heuristic freshness), capped at a week, and are then revalidated with
//!   `If-None-Match` / `If-Modified-Since`;
//! - requests carrying a cache-busting `rnd` query parameter are not cached,
//!   unless the URL is content-hashed (the parameter is then irrelevant);
//! - only static resources (by file extension) are cached, never endpoints
//!   such as `/login` that answer dynamically to `GET`.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use url::Url;

const HEADER_MAGIC: &str = "ruffle-http-cache-v1";
const MAX_HEURISTIC_FRESHNESS: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// Eviction scans the whole directory, so it only runs every this many stores.
const EVICTION_INTERVAL: u32 = 64;
/// File extensions of static resources that may be cached.
const STATIC_EXTENSIONS: &[&str] = &[
    "swf", "bswf", "spl", "png", "jpg", "jpeg", "gif", "bmp", "mp3", "wav", "xml", "dat", "zip",
    "txt", "ttf", "otf",
];

/// A cached response body and the headers needed to serve it.
pub struct Cached {
    pub body: Vec<u8>,
    pub content_type: Option<String>,
}

/// What the cache knows about a URL.
pub enum Lookup {
    /// Can be used without contacting the server.
    Fresh(Cached),
    /// Must be revalidated with the server using these validators.
    Stale {
        cached: Cached,
        etag: Option<String>,
        last_modified: Option<String>,
    },
    Miss,
}

/// Headers and timestamps stored next to a cached body.
struct Entry {
    key: String,
    content_type: Option<String>,
    etag: Option<String>,
    last_modified: Option<String>,
    stored_at: u64,
    immutable: bool,
}

pub struct HttpCache {
    dir: PathBuf,
    max_bytes: u64,
    stores_since_eviction: std::cell::Cell<u32>,
}

impl HttpCache {
    pub fn new(dir: impl Into<PathBuf>, max_bytes: u64) -> io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        Ok(Self {
            dir,
            max_bytes,
            stores_since_eviction: std::cell::Cell::new(0),
        })
    }

    /// Returns the cache key for a URL and whether its content is immutable,
    /// or `None` when the URL should not be cached.
    pub fn key_for(url: &Url) -> Option<(String, bool)> {
        if !matches!(url.scheme(), "http" | "https") || !is_static_resource(url.path()) {
            return None;
        }
        if is_content_hashed(url.path()) {
            let mut key = url.clone();
            key.set_query(None);
            key.set_fragment(None);
            return Some((key.to_string(), true));
        }
        if url.query_pairs().any(|(name, _)| name == "rnd") {
            return None;
        }
        let mut key = url.clone();
        key.set_fragment(None);
        Some((key.to_string(), false))
    }

    pub fn lookup(&self, url: &Url) -> Lookup {
        self.lookup_at(url, now())
    }

    fn lookup_at(&self, url: &Url, now: u64) -> Lookup {
        let Some((key, _)) = Self::key_for(url) else {
            return Lookup::Miss;
        };
        let Ok((entry, body)) = read_entry(&self.path_for(&key)) else {
            return Lookup::Miss;
        };
        if entry.key != key {
            // Hash collision with another URL.
            return Lookup::Miss;
        }
        let fresh = entry.immutable || now < entry.stored_at + freshness_lifetime(&entry);
        let cached = Cached {
            body,
            content_type: entry.content_type,
        };
        if fresh {
            Lookup::Fresh(cached)
        } else {
            Lookup::Stale {
                cached,
                etag: entry.etag,
                last_modified: entry.last_modified,
            }
        }
    }

    /// Stores a successful response. Responses without validators are only
    /// stored for content-hashed URLs, since they could never be revalidated.
    pub fn store(
        &self,
        url: &Url,
        body: &[u8],
        content_type: Option<String>,
        etag: Option<String>,
        last_modified: Option<String>,
    ) {
        self.store_at(url, body, content_type, etag, last_modified, now());
    }

    fn store_at(
        &self,
        url: &Url,
        body: &[u8],
        content_type: Option<String>,
        etag: Option<String>,
        last_modified: Option<String>,
        now: u64,
    ) {
        let Some((key, immutable)) = Self::key_for(url) else {
            return;
        };
        if !immutable && etag.is_none() && last_modified.is_none() {
            return;
        }
        let entry = Entry {
            key,
            content_type,
            etag,
            last_modified,
            stored_at: now,
            immutable,
        };
        if let Err(e) = write_entry(&self.path_for(&entry.key), &entry, body) {
            tracing::warn!("Failed to write HTTP cache entry for {url}: {e}");
            return;
        }
        let stores = self.stores_since_eviction.get() + 1;
        if stores >= EVICTION_INTERVAL {
            self.stores_since_eviction.set(0);
            self.evict();
        } else {
            self.stores_since_eviction.set(stores);
        }
    }

    /// Marks a stale entry as fresh again after the server answered `304`.
    pub fn refresh(&self, url: &Url) {
        let Some((key, _)) = Self::key_for(url) else {
            return;
        };
        let path = self.path_for(&key);
        if let Ok((mut entry, body)) = read_entry(&path)
            && entry.key == key
        {
            entry.stored_at = now();
            let _ = write_entry(&path, &entry, &body);
        }
    }

    /// Removes the least recently written entries until the cache fits.
    fn evict(&self) {
        let Ok(dir) = fs::read_dir(&self.dir) else {
            return;
        };
        let mut files: Vec<(SystemTime, u64, PathBuf)> = dir
            .flatten()
            .filter_map(|entry| {
                let metadata = entry.metadata().ok()?;
                Some((metadata.modified().ok()?, metadata.len(), entry.path()))
            })
            .collect();
        let mut total: u64 = files.iter().map(|(_, len, _)| len).sum();
        files.sort_by_key(|(modified, _, _)| *modified);
        for (_, len, path) in files {
            if total <= self.max_bytes {
                break;
            }
            if fs::remove_file(&path).is_ok() {
                total -= len;
            }
        }
    }

    fn path_for(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{:016x}", fnv1a(key.as_bytes())))
    }
}

fn is_static_resource(path: &str) -> bool {
    let file = path.rsplit('/').next().unwrap_or_default();
    file.rsplit_once('.').is_some_and(|(_, extension)| {
        STATIC_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str())
    })
}

/// `name_1a2b3c4d.swf`: an 8+ hex digit content hash before the extension.
fn is_content_hashed(path: &str) -> bool {
    let file = path.rsplit('/').next().unwrap_or_default();
    let stem = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
    stem.rsplit_once('_').is_some_and(|(_, hash)| {
        hash.len() >= 8 && hash.bytes().all(|b| b.is_ascii_hexdigit())
    })
}

fn freshness_lifetime(entry: &Entry) -> u64 {
    let Some(modified) = entry.last_modified.as_deref().and_then(parse_http_date) else {
        return 0;
    };
    let age = entry.stored_at.saturating_sub(modified);
    (age / 10).min(MAX_HEURISTIC_FRESHNESS.as_secs())
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Stable across Rust versions, unlike `DefaultHasher`.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |hash, &b| {
        (hash ^ u64::from(b)).wrapping_mul(0x100000001b3)
    })
}

fn write_entry(path: &Path, entry: &Entry, body: &[u8]) -> io::Result<()> {
    let header = format!(
        "{HEADER_MAGIC}\n{}\n{}\n{}\n{}\n{}\n{}\n",
        entry.key,
        entry.content_type.as_deref().unwrap_or_default(),
        entry.etag.as_deref().unwrap_or_default(),
        entry.last_modified.as_deref().unwrap_or_default(),
        entry.stored_at,
        u8::from(entry.immutable),
    );
    // Write to a temporary file and rename, so readers never see a partial entry.
    let temp = path.with_extension("tmp");
    let mut file = fs::File::create(&temp)?;
    file.write_all(header.as_bytes())?;
    file.write_all(body)?;
    drop(file);
    fs::rename(temp, path)
}

fn read_entry(path: &Path) -> io::Result<(Entry, Vec<u8>)> {
    let mut data = Vec::new();
    fs::File::open(path)?.read_to_end(&mut data)?;
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "corrupt HTTP cache entry");
    let mut fields = Vec::with_capacity(7);
    let mut rest = data.as_slice();
    for _ in 0..7 {
        let end = rest.iter().position(|&b| b == b'\n').ok_or_else(invalid)?;
        fields.push(String::from_utf8(rest[..end].to_vec()).map_err(|_| invalid())?);
        rest = &rest[end + 1..];
    }
    if fields[0] != HEADER_MAGIC {
        return Err(invalid());
    }
    let optional = |s: &String| (!s.is_empty()).then(|| s.clone());
    let entry = Entry {
        key: fields[1].clone(),
        content_type: optional(&fields[2]),
        etag: optional(&fields[3]),
        last_modified: optional(&fields[4]),
        stored_at: fields[5].parse().map_err(|_| invalid())?,
        immutable: fields[6] == "1",
    };
    Ok((entry, rest.to_vec()))
}

/// Parses an IMF-fixdate (`Sun, 06 Nov 1994 08:49:37 GMT`) into Unix seconds.
fn parse_http_date(value: &str) -> Option<u64> {
    let mut parts = value.split_whitespace();
    let _weekday = parts.next()?;
    let day: u64 = parts.next()?.parse().ok()?;
    let month = match parts.next()? {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let year: i64 = parts.next()?.parse().ok()?;
    let mut time = parts.next()?.split(':');
    let hour: u64 = time.next()?.parse().ok()?;
    let minute: u64 = time.next()?.parse().ok()?;
    let second: u64 = time.next()?.parse().ok()?;
    if parts.next()? != "GMT" {
        return None;
    }
    // Days from the civil date (Howard Hinnant's algorithm).
    let (y, m) = if month <= 2 {
        (year - 1, month + 9)
    } else {
        (year, month - 3)
    };
    let era = y.div_euclid(400);
    let year_of_era = y - era * 400;
    let day_of_year = (153 * m + 2) / 5 + day as i64 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146097 + day_of_era - 719468;
    u64::try_from(days).ok().map(|days| days * 86400 + hour * 3600 + minute * 60 + second)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn keys() {
        let (key, immutable) =
            HttpCache::key_for(&url("https://a.example/ui/core_cc0bfa38.swf?rnd=0.5")).unwrap();
        assert_eq!(key, "https://a.example/ui/core_cc0bfa38.swf");
        assert!(immutable);
        assert_eq!(
            HttpCache::key_for(&url("https://a.example/map/tile_10_5.jpg")),
            Some(("https://a.example/map/tile_10_5.jpg".to_string(), false))
        );
        assert_eq!(HttpCache::key_for(&url("https://a.example/config.xml?rnd=0.3")), None);
        assert_eq!(HttpCache::key_for(&url("file:///tmp/a_12345678.swf")), None);
        assert_eq!(HttpCache::key_for(&url("https://a.example/login")), None);
        assert_eq!(HttpCache::key_for(&url("https://a.example/api/user.jsp")), None);
    }

    #[test]
    fn parses_http_dates() {
        assert_eq!(parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT"), Some(784111777));
        assert_eq!(parse_http_date("Mon, 19 Oct 2026 08:20:41 GMT"), Some(1792398041));
        assert_eq!(parse_http_date("garbage"), None);
    }

    #[test]
    fn immutable_entries_are_always_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let cache = HttpCache::new(dir.path(), u64::MAX).unwrap();
        let u = url("https://a.example/ui/core_cc0bfa38.swf");
        cache.store_at(&u, b"swf", Some("application/x-shockwave-flash".into()), None, None, 1000);
        match cache.lookup_at(&u, 10_000_000) {
            Lookup::Fresh(cached) => {
                assert_eq!(cached.body, b"swf");
                assert_eq!(cached.content_type.as_deref(), Some("application/x-shockwave-flash"));
            }
            _ => panic!("expected a fresh entry"),
        }
    }

    #[test]
    fn heuristic_freshness_then_revalidation() {
        let dir = tempfile::tempdir().unwrap();
        let cache = HttpCache::new(dir.path(), u64::MAX).unwrap();
        let u = url("https://a.example/map/tile_10_5.jpg");
        let modified = 784111777; // 10 days before `stored`
        let stored = modified + 10 * 86400;
        cache.store_at(
            &u,
            b"jpg",
            None,
            Some("\"abc\"".to_string()),
            Some("Sun, 06 Nov 1994 08:49:37 GMT".to_string()),
            stored,
        );
        // Fresh for 10% of its age: one day.
        assert!(matches!(cache.lookup_at(&u, stored + 86399), Lookup::Fresh(_)));
        match cache.lookup_at(&u, stored + 86401) {
            Lookup::Stale { cached, etag, .. } => {
                assert_eq!(cached.body, b"jpg");
                assert_eq!(etag.as_deref(), Some("\"abc\""));
            }
            _ => panic!("expected a stale entry"),
        }
    }

    #[test]
    fn skips_unvalidatable_and_evicts_oldest() {
        let dir = tempfile::tempdir().unwrap();
        let cache = HttpCache::new(dir.path(), 300).unwrap();
        let plain = url("https://a.example/plain.bin");
        cache.store_at(&plain, b"x", None, None, None, 1);
        assert!(matches!(cache.lookup_at(&plain, 1), Lookup::Miss));

        for i in 0..EVICTION_INTERVAL {
            let u = url(&format!("https://a.example/f_{i:08x}.bin"));
            cache.store_at(&u, &[0; 100], None, None, None, 1);
        }
        let total: u64 = fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.metadata().unwrap().len())
            .sum();
        assert!(total <= 300, "cache should be trimmed, has {total} bytes");
    }
}
