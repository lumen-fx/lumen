//! What `lumen-server` is told on its command line and in its environment.
//!
//! Every setting is a flag, and every flag has a `LUMEN_*` variable that
//! stands in for it; a flag wins over its variable. There is no config file:
//! what the app allows a render to do is in the site's `lumen.site.json`,
//! written by the build, and what is left is how this one deployment runs.

use std::net::IpAddr;
use std::path::PathBuf;
use std::time::Duration;

use crate::log::LogFormat;
use crate::proxy::{Cidr, Trust};
use crate::server::{HEALTH_PATH, LOOPBACK, Limits};

/// The port the server listens on when none is named.
pub const DEFAULT_PORT: u16 = 8080;

/// How long a render gets once it starts, unless told otherwise.
pub const DEFAULT_RENDER_TIMEOUT: Duration = Duration::from_secs(30);

/// The usage text `--help` prints.
pub const USAGE: &str = "lumen-server - serve a site `lumenc web` built

USAGE:
    lumen-server [OPTIONS] <SITE_DIR>
    lumen-server probe [OPTIONS]

SITE_DIR is what `lumenc web` wrote. A site built with --render ssr holds
lumen.site.json, and every page is rendered for the request that asks; any
other site is served as the files it is. Every flag can be set in the
environment instead, with the variable named beside it; a flag wins over its
variable.

OPTIONS:
    --dev                   Development mode: an error page says what went
                            wrong, one process, no render time limit, no
                            render queue bound, forwarding headers believed
                            from this machine, and the server stops when the
                            process that started it exits. Never face the
                            public with it [LUMEN_DEV=1]
    --bind ADDR             Address to listen on (default: 127.0.0.1)
                            [LUMEN_BIND]
    --port N                Port to listen on (default: 8080; 0 picks a free
                            one) [LUMEN_PORT]
    --base-path PATH        URL prefix a site without lumen.site.json is
                            served under (default: /); a rendered site takes
                            its own from lumen.site.json [LUMEN_BASE_PATH]
    --workers N             Worker processes sharing the port, each rendering
                            one page at a time (default: 1; Windows and --dev
                            run one) [LUMEN_WORKERS]
    --max-connections N     Connections served at once per worker; more wait
                            (default: 256) [LUMEN_MAX_CONNECTIONS]
    --queue-depth N         Requests that may wait for a render per worker;
                            past it a page is answered 503 (default: 2;
                            unbounded under --dev) [LUMEN_QUEUE_DEPTH]
    --header-timeout DUR    Time to send a request's headers (default: 10s)
                            [LUMEN_HEADER_TIMEOUT]
    --body-timeout DUR      Time to send a request's body (default: 30s)
                            [LUMEN_BODY_TIMEOUT]
    --write-timeout DUR     Time to take a response (default: 30s)
                            [LUMEN_WRITE_TIMEOUT]
    --keep-alive DUR        How long an idle connection stays open; 0 closes
                            after every response (default: 5s)
                            [LUMEN_KEEP_ALIVE]
    --render-timeout DUR    Time a render gets once it starts; past it the
                            page is answered 504 and the worker restarts
                            (default: 30s; none under --dev)
                            [LUMEN_RENDER_TIMEOUT]
    --max-renders N         Restart a worker after about N renders; 0 never
                            does (default: 0) [LUMEN_MAX_RENDERS]
    --shutdown-grace DUR    Time a stopping server gives the requests it is
                            answering (default: 30s) [LUMEN_SHUTDOWN_GRACE]
    --log-format FORMAT     text or json (default: text) [LUMEN_LOG_FORMAT]
    --health-path PREFIX    Where <PREFIX>/healthz and <PREFIX>/readyz answer
                            (default: /_lumen) [LUMEN_HEALTH_PATH]
    --trusted-proxy CIDR    A proxy whose X-Forwarded-For and
                            X-Forwarded-Proto are believed. Repeat for more;
                            the variable takes a comma-separated list
                            (default: none; under --dev on a loopback
                            address, this machine) [LUMEN_TRUSTED_PROXIES]
    --allow-host NAME       Under --dev, let a render ask this host for data
                            too, beside the ones lumen.toml [web.ssr]
                            allow_hosts lists. Repeat for more; the variable
                            takes a comma-separated list [LUMEN_ALLOW_HOSTS]
    -h, --help              Print this and exit
    -V, --version           Print the version and exit

A duration is a number with a unit: 500ms, 10s, 2m, 1h. A bare number is
seconds.

`lumen-server probe` asks a running server's liveness endpoint, reading the
same --bind, --port and --health-path, and exits 0 when it answers 200. It is
what a container's HEALTHCHECK runs.";

/// Everything `lumen-server` runs with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Development mode: detailed error pages, one process, and the
    /// defaults below that suit one developer on one machine.
    pub dev: bool,
    /// The built site.
    pub site: Option<PathBuf>,
    /// The URL prefix a site without a spec file is served under. `None`
    /// means `/`, or whatever the spec file says.
    pub base_path: Option<String>,
    /// The address to listen on.
    pub bind: IpAddr,
    /// The port to listen on.
    pub port: u16,
    /// Worker processes.
    pub workers: usize,
    /// Timeouts and the connection bound, per worker.
    pub limits: Limits,
    /// Requests that may wait for a render, per worker; `None` has no bound.
    pub queue_depth: Option<usize>,
    /// Time a render gets once it starts; `None` has no limit.
    pub render_timeout: Option<Duration>,
    /// Renders before a worker restarts; `None` never.
    pub max_renders: Option<u64>,
    /// How lines are written.
    pub log_format: LogFormat,
    /// Where the health endpoints answer.
    pub health_path: String,
    /// Whose forwarding headers are believed.
    pub trust: Trust,
    /// Hosts a render may ask for data beyond the site's own policy. Only
    /// `--dev` takes any.
    pub allow_hosts: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            dev: false,
            site: None,
            base_path: None,
            bind: LOOPBACK,
            port: DEFAULT_PORT,
            workers: 1,
            limits: Limits::default(),
            queue_depth: Some(2),
            render_timeout: Some(DEFAULT_RENDER_TIMEOUT),
            max_renders: None,
            log_format: LogFormat::Text,
            health_path: HEALTH_PATH.to_string(),
            trust: Trust::Nobody,
            allow_hosts: Vec::new(),
        }
    }
}

/// What the command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Serve the site.
    Serve(Config),
    /// Ask a running server whether it is alive.
    Probe(Config),
    /// Print the usage.
    Help,
    /// Print the version.
    Version,
}

/// Every setting: its flag, its variable, and what to do with a value.
struct Setting {
    flag: &'static str,
    var: &'static str,
    apply: fn(&mut Config, &str) -> Result<(), String>,
}

const SETTINGS: &[Setting] = &[
    Setting {
        flag: "--bind",
        var: "LUMEN_BIND",
        apply: |config, value| {
            config.bind = address(value)?;
            Ok(())
        },
    },
    Setting {
        flag: "--port",
        var: "LUMEN_PORT",
        apply: |config, value| {
            config.port = number(value)?;
            Ok(())
        },
    },
    Setting {
        flag: "--base-path",
        var: "LUMEN_BASE_PATH",
        apply: |config, value| {
            let value = value.trim();
            if !value.starts_with('/') {
                return Err(format!("a base path starts with `/`, got `{value}`"));
            }
            config.base_path = Some(value.to_string());
            Ok(())
        },
    },
    Setting {
        flag: "--workers",
        var: "LUMEN_WORKERS",
        apply: |config, value| {
            config.workers = at_least_one(value)?;
            Ok(())
        },
    },
    Setting {
        flag: "--max-connections",
        var: "LUMEN_MAX_CONNECTIONS",
        apply: |config, value| {
            config.limits.max_connections = at_least_one(value)?;
            Ok(())
        },
    },
    Setting {
        flag: "--queue-depth",
        var: "LUMEN_QUEUE_DEPTH",
        apply: |config, value| {
            config.queue_depth = Some(number(value)?);
            Ok(())
        },
    },
    Setting {
        flag: "--header-timeout",
        var: "LUMEN_HEADER_TIMEOUT",
        apply: |config, value| {
            config.limits.header_timeout = positive(value)?;
            Ok(())
        },
    },
    Setting {
        flag: "--body-timeout",
        var: "LUMEN_BODY_TIMEOUT",
        apply: |config, value| {
            config.limits.body_timeout = positive(value)?;
            Ok(())
        },
    },
    Setting {
        flag: "--write-timeout",
        var: "LUMEN_WRITE_TIMEOUT",
        apply: |config, value| {
            config.limits.write_timeout = positive(value)?;
            Ok(())
        },
    },
    Setting {
        flag: "--keep-alive",
        var: "LUMEN_KEEP_ALIVE",
        apply: |config, value| {
            config.limits.keep_alive = duration(value)?;
            Ok(())
        },
    },
    Setting {
        flag: "--render-timeout",
        var: "LUMEN_RENDER_TIMEOUT",
        apply: |config, value| {
            config.render_timeout = Some(positive(value)?);
            Ok(())
        },
    },
    Setting {
        flag: "--max-renders",
        var: "LUMEN_MAX_RENDERS",
        apply: |config, value| {
            let renders: u64 = number(value)?;
            config.max_renders = (renders > 0).then_some(renders);
            Ok(())
        },
    },
    Setting {
        flag: "--shutdown-grace",
        var: "LUMEN_SHUTDOWN_GRACE",
        apply: |config, value| {
            config.limits.shutdown_grace = duration(value)?;
            Ok(())
        },
    },
    Setting {
        flag: "--log-format",
        var: "LUMEN_LOG_FORMAT",
        apply: |config, value| {
            config.log_format = value.parse()?;
            Ok(())
        },
    },
    Setting {
        flag: "--health-path",
        var: "LUMEN_HEALTH_PATH",
        apply: |config, value| {
            let value = value.trim();
            if !value.starts_with('/') {
                return Err(format!("a health path starts with `/`, got `{value}`"));
            }
            config.health_path = value.trim_end_matches('/').to_string();
            Ok(())
        },
    },
    Setting {
        flag: "--trusted-proxy",
        var: "LUMEN_TRUSTED_PROXIES",
        apply: |config, value| {
            let mut blocks = match std::mem::take(&mut config.trust) {
                Trust::Only(blocks) => blocks,
                _ => Vec::new(),
            };
            for block in value.split(',').map(str::trim).filter(|b| !b.is_empty()) {
                blocks.push(block.parse::<Cidr>()?);
            }
            config.trust = if blocks.is_empty() {
                Trust::Nobody
            } else {
                Trust::Only(blocks)
            };
            Ok(())
        },
    },
    Setting {
        flag: "--allow-host",
        var: "LUMEN_ALLOW_HOSTS",
        apply: |config, value| {
            for host in value.split(',').map(str::trim).filter(|h| !h.is_empty()) {
                if host.contains(['/', ' ']) {
                    return Err(format!(
                        "`{host}` is not a host name; name the host alone, such as \
                         api.example.com"
                    ));
                }
                config.allow_hosts.push(host.to_ascii_lowercase());
            }
            Ok(())
        },
    },
];

/// The flags a command line may give more than once, whose list there
/// replaces the one their variable gave.
const REPEATABLE: &[&str] = &["--trusted-proxy", "--allow-host"];

/// The variable that turns on development mode.
const DEV_VAR: &str = "LUMEN_DEV";

/// Whether a `LUMEN_DEV` value turns development mode on.
fn dev_value(value: &str) -> Result<bool, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "" | "0" | "false" | "no" | "off" => Ok(false),
        "1" | "true" | "yes" | "on" => Ok(true),
        other => Err(format!("{DEV_VAR}: `{other}` is not 1 or 0")),
    }
}

/// The blocks this machine reaches itself from.
fn loopback_blocks() -> Vec<Cidr> {
    ["127.0.0.0/8", "::1/128"]
        .iter()
        .filter_map(|block| block.parse().ok())
        .collect()
}

/// The variable that stands in for the site directory.
const SITE_VAR: &str = "LUMEN_SITE";

/// Read the command line and the environment. `env` looks a variable up, so
/// a test hands in its own.
pub fn parse(
    args: impl IntoIterator<Item = String>,
    env: impl Fn(&str) -> Option<String>,
) -> Result<Command, String> {
    let mut args = args.into_iter().peekable();
    let probe = args.peek().is_some_and(|first| first == "probe");
    if probe {
        args.next();
    }

    let mut config = Config::default();
    // Which settings were given at all, so development mode fills in only
    // the ones nobody named.
    let mut given: Vec<&str> = Vec::new();
    if let Some(value) = env(DEV_VAR) {
        config.dev = dev_value(&value)?;
    }
    // The environment first, so a flag given for the same setting replaces it.
    for setting in SETTINGS {
        if let Some(value) = env(setting.var).filter(|value| !value.trim().is_empty()) {
            (setting.apply)(&mut config, &value)
                .map_err(|why| format!("{}: {why}", setting.var))?;
            given.push(setting.flag);
        }
    }
    if let Some(site) = env(SITE_VAR).filter(|site| !site.trim().is_empty()) {
        config.site = Some(PathBuf::from(site));
    }

    // A repeatable flag given on the command line replaces its variable's
    // list rather than adding to it.
    let mut replaced: Vec<&str> = Vec::new();
    let mut site_arg = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Command::Help),
            "-V" | "--version" => return Ok(Command::Version),
            "--dev" => {
                config.dev = true;
                continue;
            }
            _ => {}
        }
        if let Some(flag) = arg.strip_prefix("--") {
            let (name, inline) = match flag.split_once('=') {
                Some((name, value)) => (format!("--{name}"), Some(value.to_string())),
                None => (arg.clone(), None),
            };
            let Some(setting) = SETTINGS.iter().find(|setting| setting.flag == name) else {
                return Err(format!("unknown flag `{name}`"));
            };
            let value = match inline {
                Some(value) => value,
                None => args.next().ok_or_else(|| format!("{name} needs a value"))?,
            };
            if REPEATABLE.contains(&setting.flag) && !replaced.contains(&setting.flag) {
                match setting.flag {
                    "--trusted-proxy" => config.trust = Trust::Nobody,
                    _ => config.allow_hosts.clear(),
                }
                replaced.push(setting.flag);
            }
            (setting.apply)(&mut config, &value).map_err(|why| format!("{name}: {why}"))?;
            given.push(setting.flag);
        } else if site_arg.is_none() && !probe {
            site_arg = Some(PathBuf::from(arg));
        } else {
            return Err(format!("unexpected argument `{arg}`"));
        }
    }
    if site_arg.is_some() {
        config.site = site_arg;
    }
    if config.dev {
        develop(&mut config, &given)?;
    } else if !config.allow_hosts.is_empty() {
        return Err(
            "--allow-host widens what a render may reach for development, and only --dev takes \
             it. For a server facing the public, list the host in lumen.toml [web.ssr] \
             allow_hosts and build again"
                .to_string(),
        );
    }
    if probe {
        return Ok(Command::Probe(config));
    }
    if config.site.is_none() {
        return Err(format!(
            "name the site directory, or set {SITE_VAR}: the directory `lumenc web` wrote"
        ));
    }
    Ok(Command::Serve(config))
}

/// Fill in what development mode means for every setting nobody named.
fn develop(config: &mut Config, given: &[&str]) -> Result<(), String> {
    if config.workers > 1 {
        return Err(
            "--dev runs one process, and --workers asks for more; drop one of them".to_string(),
        );
    }
    // A developer waiting on a breakpoint or a slow upstream wants the page
    // when it comes, not a 504 and a process that exits under them.
    if !given.contains(&"--render-timeout") {
        config.render_timeout = None;
    }
    // One visitor reloading a page is never load worth turning away.
    if !given.contains(&"--queue-depth") {
        config.queue_depth = None;
    }
    // Only this machine reaches a loopback address, so whatever sits in
    // front of it on this machine and says the visitor came over TLS is
    // believed.
    if !given.contains(&"--trusted-proxy") && config.bind.is_loopback() {
        config.trust = Trust::Only(loopback_blocks());
    }
    Ok(())
}

/// An address this machine has, or `localhost`.
fn address(value: &str) -> Result<IpAddr, String> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("localhost") {
        return Ok(LOOPBACK);
    }
    value
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse()
        .map_err(|_| format!("`{value}` is not an address, such as 127.0.0.1 or 0.0.0.0"))
}

fn number<T: std::str::FromStr>(value: &str) -> Result<T, String> {
    value
        .trim()
        .parse()
        .map_err(|_| format!("`{}` is not a whole number in range", value.trim()))
}

fn at_least_one(value: &str) -> Result<usize, String> {
    let n: usize = number(value)?;
    if n == 0 {
        return Err("it takes 1 or more".to_string());
    }
    Ok(n)
}

/// A duration: `500ms`, `10s`, `2m`, `1h`, or a bare number of seconds.
pub fn duration(value: &str) -> Result<Duration, String> {
    let value = value.trim();
    let split = value
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(value.len());
    let (amount, unit) = value.split_at(split);
    let amount: f64 = amount
        .parse()
        .map_err(|_| format!("`{value}` is not a duration, such as 500ms, 10s or 2m"))?;
    let seconds = match unit.trim() {
        "" | "s" => amount,
        "ms" => amount / 1000.0,
        "m" => amount * 60.0,
        "h" => amount * 3600.0,
        other => {
            return Err(format!(
                "`{other}` is not a unit; a duration is in ms, s, m or h"
            ));
        }
    };
    Duration::try_from_secs_f64(seconds).map_err(|_| format!("`{value}` is out of range"))
}

fn positive(value: &str) -> Result<Duration, String> {
    let duration = duration(value)?;
    if duration.is_zero() {
        return Err("it has to be longer than zero".to_string());
    }
    Ok(duration)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn run(args: &[&str], env: &[(&str, &str)]) -> Result<Command, String> {
        let env: HashMap<String, String> = env
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect();
        parse(args.iter().map(|arg| arg.to_string()), |name| {
            env.get(name).cloned()
        })
    }

    fn serve(args: &[&str], env: &[(&str, &str)]) -> Config {
        match run(args, env) {
            Ok(Command::Serve(config)) => config,
            other => panic!("expected a server config, got {other:?}"),
        }
    }

    #[test]
    fn nothing_but_a_site_is_the_defaults() {
        let config = serve(&["dist/web"], &[]);
        assert_eq!(config.site, Some(PathBuf::from("dist/web")));
        assert_eq!(config.bind, LOOPBACK);
        assert_eq!(config.port, DEFAULT_PORT);
        assert_eq!(config.workers, 1);
        assert_eq!(config.limits, Limits::default());
        assert_eq!(config.queue_depth, Some(2));
        assert_eq!(config.render_timeout, Some(DEFAULT_RENDER_TIMEOUT));
        assert!(!config.dev);
        assert_eq!(config.base_path, None);
        assert_eq!(config.max_renders, None);
        assert_eq!(config.health_path, "/_lumen");
        assert_eq!(config.trust, Trust::Nobody);
    }

    #[test]
    fn every_flag_is_read_in_both_spellings() {
        let config = serve(
            &[
                "--bind",
                "0.0.0.0",
                "--port=9000",
                "--workers",
                "4",
                "--max-connections",
                "64",
                "--queue-depth",
                "0",
                "--header-timeout",
                "500ms",
                "--body-timeout=1m",
                "--write-timeout",
                "20",
                "--keep-alive",
                "0",
                "--render-timeout",
                "2.5s",
                "--max-renders",
                "1000",
                "--shutdown-grace",
                "1h",
                "--log-format",
                "json",
                "--health-path",
                "/ops/",
                "--trusted-proxy",
                "10.0.0.0/8",
                "--trusted-proxy=::1",
                "--base-path",
                "/docs",
                "site",
            ],
            &[],
        );
        assert_eq!(config.bind.to_string(), "0.0.0.0");
        assert_eq!(config.port, 9000);
        assert_eq!(config.workers, 4);
        assert_eq!(config.limits.max_connections, 64);
        assert_eq!(config.queue_depth, Some(0));
        assert_eq!(config.limits.header_timeout, Duration::from_millis(500));
        assert_eq!(config.limits.body_timeout, Duration::from_secs(60));
        assert_eq!(config.limits.write_timeout, Duration::from_secs(20));
        assert_eq!(config.limits.keep_alive, Duration::ZERO);
        assert_eq!(config.render_timeout, Some(Duration::from_millis(2500)));
        assert_eq!(config.max_renders, Some(1000));
        assert_eq!(config.limits.shutdown_grace, Duration::from_secs(3600));
        assert_eq!(config.log_format, LogFormat::Json);
        assert_eq!(config.health_path, "/ops");
        let Trust::Only(blocks) = &config.trust else {
            panic!("{:?}", config.trust);
        };
        assert_eq!(blocks.len(), 2);
        assert_eq!(config.base_path.as_deref(), Some("/docs"));
    }

    #[test]
    fn a_variable_stands_in_for_its_flag_and_the_flag_wins() {
        let env = [
            ("LUMEN_SITE", "/site"),
            ("LUMEN_BIND", "0.0.0.0"),
            ("LUMEN_PORT", "8081"),
            ("LUMEN_MAX_RENDERS", "50"),
            ("LUMEN_LOG_FORMAT", "json"),
            ("LUMEN_TRUSTED_PROXIES", "10.0.0.0/8, 192.168.0.0/16"),
        ];
        let config = serve(&[], &env);
        assert_eq!(config.site, Some(PathBuf::from("/site")));
        assert_eq!(config.port, 8081);
        assert_eq!(config.max_renders, Some(50));
        assert_eq!(config.log_format, LogFormat::Json);
        let Trust::Only(blocks) = &config.trust else {
            panic!("{:?}", config.trust);
        };
        assert_eq!(blocks.len(), 2);

        let config = serve(
            &["--port", "9", "--trusted-proxy", "127.0.0.1", "elsewhere"],
            &env,
        );
        assert_eq!(config.port, 9);
        assert_eq!(config.site, Some(PathBuf::from("elsewhere")));
        // The flag's list replaces the variable's rather than joining it.
        let Trust::Only(blocks) = &config.trust else {
            panic!("{:?}", config.trust);
        };
        assert_eq!(blocks.len(), 1);
        // An empty variable is no variable.
        assert_eq!(serve(&["s"], &[("LUMEN_PORT", "")]).port, DEFAULT_PORT);
    }

    #[test]
    fn a_value_that_does_not_read_is_refused_with_where_it_came_from() {
        let error = run(&["--port", "http"], &[]).expect_err("not a port");
        assert!(error.contains("--port"), "{error}");
        let error = run(&["s"], &[("LUMEN_WORKERS", "0")]).expect_err("no workers");
        assert!(error.contains("LUMEN_WORKERS"), "{error}");
        assert!(run(&["--render-timeout", "0", "s"], &[]).is_err());
        assert!(run(&["--header-timeout", "10 parsecs", "s"], &[]).is_err());
        assert!(run(&["--bind", "example.com", "s"], &[]).is_err());
        assert!(run(&["--log-format", "xml", "s"], &[]).is_err());
        assert!(run(&["--health-path", "ops", "s"], &[]).is_err());
        assert!(run(&["--trusted-proxy", "10.0.0.0/99", "s"], &[]).is_err());
        assert!(run(&["--max-requests", "5", "s"], &[]).is_err());
        assert!(run(&["--base-path", "docs", "s"], &[]).is_err());
        assert!(run(&["s"], &[("LUMEN_DEV", "maybe")]).is_err());
        assert!(run(&["--port"], &[]).is_err());
        assert!(run(&["a", "b"], &[]).is_err());
        assert!(run(&[], &[]).is_err(), "no site at all");
    }

    #[test]
    fn dev_fills_in_what_nobody_named_and_keeps_what_they_did() {
        let config = serve(&["--dev", "site"], &[]);
        assert!(config.dev);
        assert_eq!(config.bind, LOOPBACK);
        assert_eq!(config.workers, 1);
        assert_eq!(config.render_timeout, None);
        assert_eq!(config.queue_depth, None);
        let Trust::Only(blocks) = &config.trust else {
            panic!("{:?}", config.trust);
        };
        assert!(
            blocks
                .iter()
                .any(|b| b.contains("127.0.0.1".parse().expect("ip")))
        );
        assert!(
            blocks
                .iter()
                .any(|b| b.contains("::1".parse().expect("ip")))
        );
        assert!(
            !blocks
                .iter()
                .any(|b| b.contains("10.0.0.1".parse().expect("ip")))
        );

        let config = serve(
            &[
                "--render-timeout",
                "5s",
                "--queue-depth",
                "3",
                "--trusted-proxy",
                "10.0.0.0/8",
                "site",
            ],
            &[("LUMEN_DEV", "1")],
        );
        assert!(config.dev);
        assert_eq!(config.render_timeout, Some(Duration::from_secs(5)));
        assert_eq!(config.queue_depth, Some(3));
        let Trust::Only(blocks) = &config.trust else {
            panic!("{:?}", config.trust);
        };
        assert_eq!(blocks.len(), 1);
    }

    #[test]
    fn dev_on_an_address_others_reach_believes_no_proxy() {
        let config = serve(&["--dev", "--bind", "0.0.0.0", "site"], &[]);
        assert_eq!(config.trust, Trust::Nobody);
    }

    #[test]
    fn dev_is_off_unless_asked_for() {
        assert!(!serve(&["site"], &[("LUMEN_DEV", "0")]).dev);
        assert!(!serve(&["site"], &[("LUMEN_DEV", "")]).dev);
        assert!(serve(&["site"], &[("LUMEN_DEV", "true")]).dev);
    }

    #[test]
    fn dev_runs_one_process() {
        let error = run(&["--dev", "--workers", "4", "site"], &[]).expect_err("one process");
        assert!(error.contains("--workers"), "{error}");
    }

    #[test]
    fn allow_host_is_repeatable_and_only_for_dev() {
        let config = serve(
            &[
                "--dev",
                "--allow-host",
                "API.example.com",
                "--allow-host=cdn.example.com",
                "site",
            ],
            &[("LUMEN_ALLOW_HOSTS", "other.example.com")],
        );
        // The flag's list replaces the variable's.
        assert_eq!(config.allow_hosts, ["api.example.com", "cdn.example.com"]);
        let config = serve(
            &["--dev", "site"],
            &[("LUMEN_ALLOW_HOSTS", "a.example.com, b.example.com")],
        );
        assert_eq!(config.allow_hosts, ["a.example.com", "b.example.com"]);

        let error = run(&["--allow-host", "api.example.com", "site"], &[]).expect_err("dev only");
        assert!(error.contains("--dev"), "{error}");
        assert!(error.contains("allow_hosts"), "{error}");
        assert!(run(&["--dev", "--allow-host", "https://x.example", "site"], &[]).is_err());
    }

    #[test]
    fn probe_needs_no_site_and_reads_the_same_address() {
        let Ok(Command::Probe(config)) = run(&["probe"], &[("LUMEN_PORT", "8081")]) else {
            panic!("a probe");
        };
        assert_eq!(config.port, 8081);
        assert!(matches!(run(&["--help"], &[]), Ok(Command::Help)));
        assert!(matches!(run(&["-V"], &[]), Ok(Command::Version)));
    }

    #[test]
    fn a_duration_takes_a_unit_or_means_seconds() {
        assert_eq!(duration("250ms"), Ok(Duration::from_millis(250)));
        assert_eq!(duration("3"), Ok(Duration::from_secs(3)));
        assert_eq!(duration("2m"), Ok(Duration::from_secs(120)));
        assert_eq!(duration("1h"), Ok(Duration::from_secs(3600)));
        assert!(duration("-1s").is_err());
        assert!(duration("s").is_err());
    }
}
