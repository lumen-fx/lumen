//! The cookie jar: what servers and the script set, what a request carries,
//! and the file the lasting cookies are kept in.
//!
//! Matching follows RFC 6265: a cookie goes to its domain (and that domain's
//! subdomains unless a server set it without `Domain`), under its path, over
//! HTTPS only when it is `Secure`, and not after it expires.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use lumen_module::lumen_core::warn_line;
use serde::{Deserialize, Serialize};

/// One cookie.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cookie {
    /// Its name.
    pub name: String,
    /// Its value.
    pub value: String,
    /// The domain it is sent to, lowercase and without a leading dot. Empty
    /// for one the script set without a domain, which no request carries.
    pub domain: String,
    /// Sent to [`Self::domain`] alone, not to its subdomains.
    pub host_only: bool,
    /// The path it is sent under.
    pub path: String,
    /// When it expires, in seconds since the Unix epoch. `None` lasts as long
    /// as the process and is never written to the file.
    pub expires: Option<i64>,
    /// Sent over HTTPS only.
    pub secure: bool,
    /// Hidden from the script, as a page hides it from `document.cookie`.
    pub http_only: bool,
}

impl Cookie {
    fn expired(&self, now: i64) -> bool {
        self.expires.is_some_and(|at| at <= now)
    }

    fn same_slot(&self, other: &Cookie) -> bool {
        self.name == other.name && self.domain == other.domain && self.path == other.path
    }
}

/// The parts of a request URL a cookie is matched against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// `https` or not.
    pub secure: bool,
    /// The host, lowercase.
    pub host: String,
    /// The path, starting with `/`, without the query.
    pub path: String,
}

impl Target {
    /// Read an absolute `http://` or `https://` URL. `None` for anything else,
    /// which no cookie goes to.
    pub fn parse(url: &str) -> Option<Self> {
        let (scheme, rest) = url.split_once("://")?;
        let secure = match scheme.to_ascii_lowercase().as_str() {
            "https" => true,
            "http" => false,
            _ => return None,
        };
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let authority = &rest[..end];
        let authority = authority.rsplit_once('@').map_or(authority, |(_, a)| a);
        let host = if let Some(bracketed) = authority.strip_prefix('[') {
            bracketed.split(']').next().unwrap_or_default()
        } else {
            authority.split(':').next().unwrap_or_default()
        };
        if host.is_empty() {
            return None;
        }
        let tail = &rest[end..];
        let path = if tail.starts_with('/') {
            tail.split(['?', '#']).next().unwrap_or("/")
        } else {
            "/"
        };
        Some(Self {
            secure,
            host: host.to_ascii_lowercase(),
            path: path.to_string(),
        })
    }
}

/// Every cookie the app holds.
pub struct Jar {
    cookies: Vec<Cookie>,
    file: Option<PathBuf>,
    locate: Option<Box<dyn FnOnce() -> PathBuf + Send>>,
}

impl Jar {
    /// A jar kept in the file `locate` names, read on first use.
    #[must_use]
    pub fn persistent(locate: impl FnOnce() -> PathBuf + Send + 'static) -> Self {
        Self {
            cookies: Vec::new(),
            file: None,
            locate: Some(Box::new(locate)),
        }
    }

    /// Every cookie that has not expired, in the order they were set.
    pub fn cookies(&mut self) -> &[Cookie] {
        self.load();
        let now = now();
        self.cookies.retain(|c| !c.expired(now));
        &self.cookies
    }

    /// The value of the first cookie named `name` the script can see.
    pub fn get(&mut self, name: &str) -> Option<String> {
        self.cookies()
            .iter()
            .find(|c| c.name == name && !c.http_only)
            .map(|c| c.value.clone())
    }

    /// The name of every cookie the script can see, once each.
    pub fn keys(&mut self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for cookie in self.cookies() {
            if !cookie.http_only && !names.contains(&cookie.name) {
                names.push(cookie.name.clone());
            }
        }
        names
    }

    /// Keep `cookie`, in place of one with the same name, domain and path. An
    /// expired one removes that one instead.
    pub fn put(&mut self, cookie: Cookie) {
        self.load();
        let now = now();
        self.cookies
            .retain(|c| !c.same_slot(&cookie) && !c.expired(now));
        if !cookie.expired(now) {
            self.cookies.push(cookie);
        }
        self.save();
    }

    /// Remove the cookie in the slot `name`, `domain`, `path`.
    pub fn remove(&mut self, name: &str, domain: &str, path: &str) {
        self.load();
        let before = self.cookies.len();
        self.cookies
            .retain(|c| !(c.name == name && c.domain == domain && c.path == path));
        if self.cookies.len() != before {
            self.save();
        }
    }

    /// The `Cookie` header a request to `target` carries, if any cookie goes
    /// there: longer paths first, then the order they were set.
    pub fn header_for(&mut self, target: &Target) -> Option<String> {
        let mut matching: Vec<&Cookie> = self
            .cookies()
            .iter()
            .filter(|c| {
                !c.domain.is_empty()
                    && domain_matches(&target.host, &c.domain, c.host_only)
                    && path_matches(&target.path, &c.path)
                    && (!c.secure || target.secure)
            })
            .collect();
        if matching.is_empty() {
            return None;
        }
        matching.sort_by_key(|c| std::cmp::Reverse(c.path.len()));
        Some(
            matching
                .iter()
                .map(|c| format!("{}={}", c.name, c.value))
                .collect::<Vec<_>>()
                .join("; "),
        )
    }

    /// Keep what a `Set-Cookie` header from `target` says, when the header
    /// is one a browser would take from there.
    pub fn store_from(&mut self, target: &Target, header: &str) {
        if let Some(cookie) = parse_set_cookie(target, header, now()) {
            self.put(cookie);
        }
    }

    fn load(&mut self) {
        let Some(locate) = self.locate.take() else {
            return;
        };
        let file = locate();
        self.cookies = read(&file);
        self.file = Some(file);
    }

    /// Write the lasting cookies to the file.
    fn save(&self) {
        let Some(file) = &self.file else {
            return;
        };
        let lasting: Vec<&Cookie> = self
            .cookies
            .iter()
            .filter(|c| c.expires.is_some())
            .collect();
        if let Err(e) = write(file, &lasting) {
            warn_line!("lumen-cookie: write {}: {e}", file.display());
        }
    }
}

/// The seconds since the Unix epoch.
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// Whether a request to `host` carries a cookie for `domain`.
fn domain_matches(host: &str, domain: &str, host_only: bool) -> bool {
    if host == domain {
        return true;
    }
    !host_only
        && host.ends_with(domain)
        && host[..host.len() - domain.len()].ends_with('.')
        && host.parse::<std::net::IpAddr>().is_err()
}

/// Whether a request for `path` carries a cookie set for `cookie_path`.
fn path_matches(path: &str, cookie_path: &str) -> bool {
    path == cookie_path
        || (path.starts_with(cookie_path)
            && (cookie_path.ends_with('/') || path[cookie_path.len()..].starts_with('/')))
}

/// The path a cookie set without `Path` takes: the request path up to its
/// last `/`.
fn default_path(path: &str) -> String {
    match path.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(at) => path[..at].to_string(),
    }
}

/// Read one `Set-Cookie` header received from `target` at `now`.
fn parse_set_cookie(target: &Target, header: &str, now: i64) -> Option<Cookie> {
    let mut parts = header.split(';');
    let (name, value) = parts.next()?.split_once('=')?;
    let (name, value) = (name.trim(), value.trim());
    if name.is_empty() {
        return None;
    }
    let mut cookie = Cookie {
        name: name.to_string(),
        value: value.to_string(),
        domain: target.host.clone(),
        host_only: true,
        path: default_path(&target.path),
        expires: None,
        secure: false,
        http_only: false,
    };
    let mut max_age: Option<i64> = None;
    for attribute in parts {
        let (key, val) = attribute
            .split_once('=')
            .map_or((attribute.trim(), ""), |(k, v)| (k.trim(), v.trim()));
        match key.to_ascii_lowercase().as_str() {
            "domain" => {
                let domain = val.trim_start_matches('.').to_ascii_lowercase();
                if domain.is_empty() {
                    continue;
                }
                // A server sets cookies for itself and its parents, never for
                // a sibling or a stranger.
                if !domain_matches(&target.host, &domain, false) {
                    return None;
                }
                cookie.domain = domain;
                cookie.host_only = false;
            }
            "path" if val.starts_with('/') => cookie.path = val.to_string(),
            "max-age" => max_age = val.parse::<i64>().ok(),
            "expires" => {
                if max_age.is_none()
                    && let Some(at) = parse_http_date(val)
                {
                    cookie.expires = Some(at);
                }
            }
            "secure" => cookie.secure = true,
            "httponly" => cookie.http_only = true,
            _ => {}
        }
    }
    if let Some(seconds) = max_age {
        cookie.expires = Some(if seconds <= 0 {
            i64::MIN
        } else {
            now.saturating_add(seconds)
        });
    }
    // A `Secure` cookie from plain HTTP is refused, as browsers refuse it.
    if cookie.secure && !target.secure {
        return None;
    }
    Some(cookie)
}

/// An HTTP date, `Sun, 06 Nov 1994 08:49:37 GMT` and the forms cookies use
/// (dashes between the date's parts, a two-digit year), as seconds since the
/// Unix epoch.
fn parse_http_date(text: &str) -> Option<i64> {
    let text = text.split_once(',').map_or(text, |(_, rest)| rest);
    let tokens: Vec<&str> = text.split([' ', '-']).filter(|t| !t.is_empty()).collect();
    let (mut day, mut month, mut year, mut time) = (None, None, None, None);
    for token in tokens {
        if token.contains(':') {
            let mut hms = token.split(':').map(|p| p.parse::<i64>().ok());
            let (h, m, s) = (
                hms.next()??,
                hms.next()??,
                hms.next().flatten().unwrap_or(0),
            );
            time = Some(h * 3600 + m * 60 + s);
        } else if let Some(m) = month_number(token) {
            month = Some(m);
        } else if let Ok(n) = token.parse::<i64>() {
            if day.is_none() && token.len() <= 2 && year.is_none() {
                day = Some(n);
            } else {
                year = Some(match n {
                    0..=69 => n + 2000,
                    70..=99 => n + 1900,
                    _ => n,
                });
            }
        }
    }
    let (day, month, year, time) = (day?, month?, year?, time?);
    Some(days_from_civil(year, month, day) * 86_400 + time)
}

fn month_number(token: &str) -> Option<i64> {
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let lower = token.to_ascii_lowercase();
    MONTHS
        .iter()
        .position(|m| lower.starts_with(m))
        .map(|i| i as i64 + 1)
}

/// Days from 1970-01-01 to the given date in the proleptic Gregorian calendar.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn read(file: &Path) -> Vec<Cookie> {
    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            warn_line!("lumen-cookie: read {}: {e}", file.display());
            return Vec::new();
        }
    };
    serde_json::from_str(&text).unwrap_or_else(|e| {
        warn_line!(
            "lumen-cookie: {} does not read ({e}); starting with no cookies",
            file.display()
        );
        Vec::new()
    })
}

fn write(file: &Path, cookies: &[&Cookie]) -> std::io::Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(cookies).map_err(std::io::Error::other)?;
    let partial = file.with_extension("json.partial");
    std::fs::write(&partial, text)?;
    std::fs::rename(&partial, file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(url: &str) -> Target {
        Target::parse(url).expect("a URL")
    }

    fn jar() -> Jar {
        let file = std::env::temp_dir().join(format!(
            "lumen-cookie-jar-{}-{}.json",
            std::process::id(),
            {
                static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
                SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            }
        ));
        let _ = std::fs::remove_file(&file);
        Jar::persistent(move || file)
    }

    #[test]
    fn a_url_reads_into_its_scheme_host_and_path() {
        assert_eq!(
            target("https://User@API.example.com:8443/v1/items?x=1#top"),
            Target {
                secure: true,
                host: "api.example.com".to_string(),
                path: "/v1/items".to_string(),
            }
        );
        assert_eq!(target("http://[::1]:80").path, "/");
        assert_eq!(target("http://[::1]:80").host, "::1");
        assert!(Target::parse("ws://a/").is_none());
        assert!(Target::parse("not a url").is_none());
    }

    #[test]
    fn a_cookie_goes_to_its_domain_and_path_only() {
        let mut jar = jar();
        let site = target("https://www.example.com/app/login");
        jar.store_from(&site, "sid=1; Path=/app; Domain=example.com; Secure");
        jar.store_from(&site, "host=2");
        jar.store_from(&site, "theme=dark; Path=/");

        assert_eq!(
            jar.header_for(&target("https://api.example.com/app/x"))
                .as_deref(),
            Some("sid=1")
        );
        // Host-only, and the default path is the request's directory.
        assert_eq!(
            jar.header_for(&target("https://www.example.com/app/other"))
                .as_deref(),
            Some("sid=1; host=2; theme=dark")
        );
        assert_eq!(
            jar.header_for(&target("https://www.example.com/"))
                .as_deref(),
            Some("theme=dark")
        );
        // Secure stays off plain HTTP; `/application` is not under `/app`.
        assert_eq!(
            jar.header_for(&target("http://www.example.com/app/x"))
                .as_deref(),
            Some("host=2; theme=dark")
        );
        assert_eq!(
            jar.header_for(&target("https://www.example.com/application"))
                .as_deref(),
            Some("theme=dark")
        );
        assert_eq!(jar.header_for(&target("https://example.org/")), None);
    }

    #[test]
    fn a_server_cannot_set_a_cookie_for_another_domain_or_a_secure_one_over_http() {
        let mut jar = jar();
        jar.store_from(&target("https://a.example.com/"), "x=1; Domain=other.com");
        jar.store_from(&target("http://a.example.com/"), "y=1; Secure");
        assert!(jar.keys().is_empty());
    }

    #[test]
    fn expiry_removes_and_lasting_cookies_reach_the_file() {
        let file = std::env::temp_dir().join(format!(
            "lumen-cookie-jar-{}-lasting.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&file);
        let site = target("https://example.com/");
        {
            let mut jar = Jar::persistent({
                let file = file.clone();
                move || file
            });
            jar.store_from(&site, "keep=1; Max-Age=3600");
            jar.store_from(&site, "session=1");
            jar.store_from(&site, "old=1; Expires=Thu, 01 Jan 1970 00:00:01 GMT");
            assert_eq!(jar.keys(), ["keep", "session"]);
        }
        let mut again = Jar::persistent({
            let file = file.clone();
            move || file
        });
        assert_eq!(again.keys(), ["keep"]);
        again.store_from(&site, "keep=; Max-Age=0");
        assert!(again.keys().is_empty());
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn an_http_only_cookie_is_sent_and_hidden_from_the_script() {
        let mut jar = jar();
        let site = target("https://example.com/");
        jar.store_from(&site, "secret=s; HttpOnly");
        assert_eq!(jar.get("secret"), None);
        assert!(jar.keys().is_empty());
        assert_eq!(jar.header_for(&site).as_deref(), Some("secret=s"));
    }

    #[test]
    fn http_dates_read_in_the_forms_servers_send() {
        assert_eq!(
            parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT"),
            Some(784_111_777)
        );
        assert_eq!(
            parse_http_date("Sunday, 06-Nov-94 08:49:37 GMT"),
            Some(784_111_777)
        );
        assert_eq!(parse_http_date("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
        assert_eq!(parse_http_date("soon"), None);
    }
}
