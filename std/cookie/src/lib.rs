//! Cookies a script reads and writes, and that ride along with the app's
//! HTTP requests.
//!
//! Install [`CookiePlugin`] and the app gains the `cookie` namespace, in every
//! host:
//!
//! ```text
//! cookie::get(name) -> string or null
//! cookie::set(name, value, options) -> bool
//! cookie::remove(name, options)
//! cookie::keys() -> string[]
//! ```
//!
//! The surface is declared once, in the module's web half
//! (`web/lumen-addon.toml`), which a web build ships as the page's
//! `document.cookie`. On the desktop it is a cookie jar:
//!
//! - every request a script makes with `http()` or `fetch()` carries the
//!   cookies whose domain, path and `Secure` flag match its URL, and every
//!   `Set-Cookie` the reply holds goes into the jar, with the rules a browser
//!   applies (a server sets cookies for its own domain and its parents only,
//!   and a `Secure` one only over HTTPS). A request with
//!   `credentials: "omit"` neither carries nor stores any.
//! - the script sees every cookie that is not `HttpOnly`, as a page does.
//! - a cookie the script sets goes to the requests of the `domain` option,
//!   and its subdomains; without `domain` there is no page for it to belong
//!   to, so it stays in the jar for the script alone.
//! - cookies with a `max_age` (or a server's `Expires` / `Max-Age`) are kept
//!   in `cookies.json` under the app's data directory; the rest last for the
//!   process.
//!
//! ```toml
//! [dependencies]
//! lumen-cookie = { bundled = true }
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod jar;

pub use jar::{Cookie, Jar, Target};

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use lumen_module::lumen_core::app::{App, Plugin};
use lumen_module::lumen_core::app_paths;
use lumen_module::lumen_script::{
    Credentials, HttpHook, HttpHooks, HttpRequest, HttpResponse, ScriptFnAppExt, ScriptFnBody,
    ScriptResult, ScriptValue,
};

/// The web half's descriptor, which is where this module's functions are
/// declared.
pub const DESCRIPTOR: &str = include_str!("../web/lumen-addon.toml");

/// The module's name, as an app declares it.
pub const NAME: &str = "lumen-cookie";

/// The file the lasting cookies are kept in, under the app's data directory.
pub const FILE_NAME: &str = "cookies.json";

/// The options `set` and `remove` take, as a page's `document.cookie` takes
/// them.
const OPTIONS: [&str; 6] = [
    "max_age",
    "path",
    "domain",
    "same_site",
    "secure",
    "partitioned",
];

/// Cookies for a Lumen app: install it and the `cookie` functions exist, and
/// the app's HTTP requests carry and keep cookies.
///
/// Ships as the bundled `lumen-cookie` runtime module, and works the same
/// added as an ordinary plugin in a static build.
#[derive(Debug, Clone, Default)]
pub struct CookiePlugin {
    file: Option<PathBuf>,
}

impl CookiePlugin {
    /// Keep the lasting cookies in `file` rather than in the app's data
    /// directory.
    #[must_use]
    pub fn at(file: impl Into<PathBuf>) -> Self {
        Self {
            file: Some(file.into()),
        }
    }
}

impl Plugin for CookiePlugin {
    fn build(self, app: &mut App) {
        // Resolved at the first use rather than here: the data directory
        // follows the app id, which the app publishes before it runs a
        // script.
        let file = self.file;
        let jar = Arc::new(Mutex::new(Jar::persistent(move || {
            file.unwrap_or_else(|| app_paths::data_dir().join(FILE_NAME))
        })));
        match script_fns(&jar) {
            Ok(fns) => {
                app.add_script_fns(fns);
            }
            Err(reason) => eprintln!("lumen-runtime: {reason}"),
        }
        app.world
            .get_resource_or_insert_with(HttpHooks::default)
            .add(Arc::new(JarHook(jar)));
    }
}

/// The jar, as every script request sees it.
struct JarHook(Arc<Mutex<Jar>>);

impl HttpHook for JarHook {
    fn before_send(&self, request: &mut HttpRequest) {
        if request.credentials == Credentials::Omit {
            return;
        }
        let Some(target) = Target::parse(&request.url) else {
            return;
        };
        let Some(header) = lock(&self.0).header_for(&target) else {
            return;
        };
        match request
            .headers
            .iter_mut()
            .find(|(name, _)| name.eq_ignore_ascii_case("cookie"))
        {
            Some((_, value)) => {
                value.push_str("; ");
                value.push_str(&header);
            }
            None => request.headers.push(("Cookie".to_string(), header)),
        }
    }

    fn on_reply(&self, request: &HttpRequest, response: &HttpResponse) {
        if request.credentials == Credentials::Omit {
            return;
        }
        let Some(target) = Target::parse(&request.url) else {
            return;
        };
        let mut jar = lock(&self.0);
        for (name, value) in &response.headers {
            if name.eq_ignore_ascii_case("set-cookie") {
                jar.store_from(&target, value);
            }
        }
    }
}

fn lock(jar: &Mutex<Jar>) -> std::sync::MutexGuard<'_, Jar> {
    jar.lock().unwrap_or_else(|e| e.into_inner())
}

/// The four functions, bound to `jar`.
fn script_fns(jar: &Arc<Mutex<Jar>>) -> Result<Vec<lumen_module::lumen_script::ScriptFn>, String> {
    let get: ScriptFnBody = {
        let jar = Arc::clone(jar);
        Arc::new(move |cx| {
            Ok(lock(&jar)
                .get(&cx.str_arg(0))
                .map_or(ScriptValue::Unit, ScriptValue::Str))
        })
    };
    let set: ScriptFnBody = {
        let jar = Arc::clone(jar);
        Arc::new(move |cx| {
            let options = options(cx.arg_ref(2))?;
            set_cookie(&mut lock(&jar), cx.str_arg(0), cx.str_arg(1), &options)
        })
    };
    let remove: ScriptFnBody = {
        let jar = Arc::clone(jar);
        Arc::new(move |cx| {
            let options = options(cx.arg_ref(1))?;
            let (domain, path) = (domain_of(&options), path_of(&options));
            lock(&jar).remove(&cx.str_arg(0), &domain, &path);
            Ok(ScriptValue::Unit)
        })
    };
    let keys: ScriptFnBody = {
        let jar = Arc::clone(jar);
        Arc::new(move |_| {
            Ok(ScriptValue::Array(
                lock(&jar)
                    .keys()
                    .into_iter()
                    .map(ScriptValue::Str)
                    .collect(),
            ))
        })
    };
    lumen_module::web_half_fns(
        NAME,
        DESCRIPTOR,
        vec![
            ("get".into(), get),
            ("set".into(), set),
            ("remove".into(), remove),
            ("keys".into(), keys),
        ],
    )
}

/// The options map, checked: a misspelled option is an error rather than a
/// cookie that quietly lacks it.
fn options(value: &ScriptValue) -> Result<HashMap<String, ScriptValue>, String> {
    let options = match value {
        ScriptValue::Map(map) => map.clone(),
        ScriptValue::Unit => HashMap::new(),
        _ => return Err("cookie options must be a map".to_string()),
    };
    if let Some(key) = options.keys().find(|k| !OPTIONS.contains(&k.as_str())) {
        return Err(format!(
            "unknown cookie option `{key}`; the options are {}",
            OPTIONS.join(", ")
        ));
    }
    if let Some(same_site) = options.get("same_site") {
        let spelled = same_site.stringify().to_ascii_lowercase();
        if !["lax", "strict", "none"].contains(&spelled.as_str()) {
            return Err(format!(
                "same_site `{}` is not lax, strict or none",
                same_site.stringify()
            ));
        }
    }
    Ok(options)
}

fn path_of(options: &HashMap<String, ScriptValue>) -> String {
    options
        .get("path")
        .map(ScriptValue::stringify)
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| "/".to_string())
}

fn domain_of(options: &HashMap<String, ScriptValue>) -> String {
    options
        .get("domain")
        .map(|d| d.stringify().trim_start_matches('.').to_ascii_lowercase())
        .unwrap_or_default()
}

/// An option given as a flag: `true`, or the text "true", since a script's
/// map literal may hold only one type of value.
fn flag(options: &HashMap<String, ScriptValue>, key: &str) -> bool {
    match options.get(key) {
        Some(ScriptValue::Bool(b)) => *b,
        Some(other) => other.stringify() == "true",
        None => false,
    }
}

/// Seconds, from a number or the text of one, whole seconds taken.
fn seconds(value: &ScriptValue) -> Option<i64> {
    let float = match value {
        ScriptValue::I64(n) => return Some(*n),
        ScriptValue::F64(f) => *f,
        other => other.stringify().trim().parse::<f64>().ok()?,
    };
    // Past the range an i64 holds is as good as never expiring or already
    // gone; the cast saturates to say so.
    float.is_finite().then_some(float.trunc() as i64)
}

/// `cookie::set`: keep the cookie, and answer whether the jar now holds what
/// was asked for.
fn set_cookie(
    jar: &mut Jar,
    name: String,
    value: String,
    options: &HashMap<String, ScriptValue>,
) -> ScriptResult {
    let expires = match options.get("max_age") {
        None | Some(ScriptValue::Unit) => None,
        Some(given) => {
            let seconds = seconds(given).ok_or_else(|| {
                format!("max_age `{}` is not a number of seconds", given.stringify())
            })?;
            Some(if seconds <= 0 {
                i64::MIN
            } else {
                jar::now().saturating_add(seconds)
            })
        }
    };
    let expiring = expires == Some(i64::MIN);
    jar.put(Cookie {
        name: name.clone(),
        value: value.clone(),
        domain: domain_of(options),
        host_only: false,
        path: path_of(options),
        expires,
        secure: flag(options, "secure"),
        http_only: false,
    });
    let now = jar.get(&name);
    Ok(ScriptValue::Bool(if expiring {
        now.is_none()
    } else {
        now.as_deref() == Some(value.as_str())
    }))
}

lumen_module::lumen_module!("lumen-cookie", |_config: lumen_module::ModuleConfig| {
    CookiePlugin::default()
});

#[cfg(test)]
mod tests {
    use super::*;

    fn jar() -> Arc<Mutex<Jar>> {
        let file = std::env::temp_dir().join(format!(
            "lumen-cookie-lib-{}-{}.json",
            std::process::id(),
            {
                static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
                SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            }
        ));
        let _ = std::fs::remove_file(&file);
        Arc::new(Mutex::new(Jar::persistent(move || file)))
    }

    /// One surface on every target: every function the web half declares
    /// has a desktop body.
    #[test]
    fn the_desktop_half_answers_every_function_the_web_half_declares() {
        let fns = script_fns(&jar()).expect("every function has a body");
        let declared =
            lumen_module::describe_web_half(NAME, DESCRIPTOR).expect("the descriptor reads");
        let names: Vec<&str> = fns.iter().map(|f| f.name.as_str()).collect();
        let expected: Vec<&str> = declared.functions.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, expected);
    }

    #[test]
    fn a_script_cookie_with_a_domain_rides_on_that_domain_s_requests() {
        let jar = jar();
        let map = |pairs: &[(&str, ScriptValue)]| {
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_string(), v.clone()))
                .collect::<HashMap<_, _>>()
        };
        let set = |name: &str, opts: HashMap<String, ScriptValue>| {
            set_cookie(&mut lock(&jar), name.to_string(), "v".to_string(), &opts).unwrap()
        };
        assert_eq!(
            set(
                "api",
                map(&[("domain", ScriptValue::Str("example.com".into()))])
            ),
            ScriptValue::Bool(true)
        );
        assert_eq!(set("local", HashMap::new()), ScriptValue::Bool(true));
        let hook = JarHook(Arc::clone(&jar));
        let mut request = HttpRequest {
            method: "GET".into(),
            url: "https://api.example.com/x".into(),
            ..Default::default()
        };
        hook.before_send(&mut request);
        assert_eq!(
            request.headers,
            [("Cookie".to_string(), "api=v".to_string())]
        );

        let mut omitted = HttpRequest {
            url: "https://api.example.com/x".into(),
            credentials: Credentials::Omit,
            ..Default::default()
        };
        hook.before_send(&mut omitted);
        assert!(omitted.headers.is_empty());

        // An expiring set removes the cookie and says so.
        assert_eq!(
            set("local", map(&[("max_age", ScriptValue::I64(0))])),
            ScriptValue::Bool(true)
        );
        assert_eq!(lock(&jar).keys(), ["api"]);
    }

    #[test]
    fn a_misspelled_option_is_refused() {
        let mut bad = HashMap::new();
        bad.insert("maxage".to_string(), ScriptValue::I64(1));
        let err = options(&ScriptValue::Map(bad)).unwrap_err();
        assert!(err.contains("unknown cookie option `maxage`"), "{err}");
        let mut site = HashMap::new();
        site.insert("same_site".to_string(), ScriptValue::Str("sideways".into()));
        assert!(options(&ScriptValue::Map(site)).is_err());
    }
}
