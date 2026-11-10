//! CSS animations in the IR: the `animation` shorthand and its longhands,
//! parsed into the declarations an element carries, and the `@keyframes`
//! blocks they name.
//!
//! The IR only knows the syntax. What runs an animation is an optional
//! engine capability that reads these declarations off the element and the
//! keyframe blocks off the stylesheet; without it they parse and do nothing.
//!
//! A `@keyframes` body stays browser CSS, carried verbatim for the web
//! target. [`parse_keyframes`] reads it for the desktop: each frame's
//! declarations under the browser property names, sorted into the ones the
//! desktop animates and the ones it does not.

use crate::css::{DEFAULT_EASING, parse_duration_ms, parse_easing};
use crate::layout_ir::{EasingIr, ParseError};
use crate::values::bad;

/// `animation-direction`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum AnimationDirectionIr {
    /// `normal`.
    #[default]
    Normal,
    /// `reverse`.
    Reverse,
    /// `alternate`.
    Alternate,
    /// `alternate-reverse`.
    AlternateReverse,
}

/// `animation-fill-mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum AnimationFillModeIr {
    /// `none`.
    #[default]
    None,
    /// `forwards`.
    Forwards,
    /// `backwards`.
    Backwards,
    /// `both`.
    Both,
}

/// `animation-play-state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum AnimationPlayStateIr {
    /// `running`.
    #[default]
    Running,
    /// `paused`.
    Paused,
}

/// `animation-composition`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum AnimationCompositionIr {
    /// `replace`.
    #[default]
    Replace,
    /// `add`.
    Add,
    /// `accumulate`.
    Accumulate,
}

/// What a `scroll()` or `view()` timeline follows along.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum ScrollAxisIr {
    /// `block`: the vertical axis.
    #[default]
    Block,
    /// `inline`: the horizontal axis.
    Inline,
    /// `y`.
    Y,
    /// `x`.
    X,
}

/// Which scroll container a `scroll()` timeline follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum ScrollerIr {
    /// `nearest`: the nearest ancestor scroll container.
    #[default]
    Nearest,
    /// `root`: the document's scroll container.
    Root,
    /// `self`: the element itself.
    SelfElement,
}

/// `animation-timeline`.
#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub enum AnimationTimelineIr {
    /// `auto`: the document's clock.
    #[default]
    Auto,
    /// `none`: no timeline; the animation does not run.
    None,
    /// `scroll(<scroller> <axis>)`: the scroll position of a container,
    /// start to end.
    Scroll(ScrollerIr, ScrollAxisIr),
    /// `view(<axis>)`: the element's own way through its scroll container's
    /// view, from entering at one edge to leaving at the other.
    View(ScrollAxisIr),
}

/// Every `animation-*` longhand an element declares. Each is a comma list;
/// `None` is a longhand nothing set, which reads as its initial value. The
/// `animation` shorthand sets all of them.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AnimationDeclsIr {
    /// `animation-name`; `none` stays in the list as written.
    pub name: Option<Vec<String>>,
    /// `animation-duration`, in milliseconds.
    pub duration: Option<Vec<u32>>,
    /// `animation-timing-function`.
    pub timing: Option<Vec<EasingIr>>,
    /// `animation-delay`, in milliseconds; negative starts part-way in.
    pub delay: Option<Vec<i32>>,
    /// `animation-iteration-count`; `infinite` is `f32::INFINITY`.
    pub iteration_count: Option<Vec<f32>>,
    /// `animation-direction`.
    pub direction: Option<Vec<AnimationDirectionIr>>,
    /// `animation-fill-mode`.
    pub fill_mode: Option<Vec<AnimationFillModeIr>>,
    /// `animation-play-state`.
    pub play_state: Option<Vec<AnimationPlayStateIr>>,
    /// `animation-composition`.
    pub composition: Option<Vec<AnimationCompositionIr>>,
    /// `animation-timeline`.
    pub timeline: Option<Vec<AnimationTimelineIr>>,
}

impl AnimationDeclsIr {
    /// Whether no longhand is set.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Lay `top` over these: every longhand it sets wins.
    pub fn overlay(&mut self, top: &AnimationDeclsIr) {
        fn put<T: Clone>(slot: &mut Option<T>, top: &Option<T>) {
            if top.is_some() {
                slot.clone_from(top);
            }
        }
        put(&mut self.name, &top.name);
        put(&mut self.duration, &top.duration);
        put(&mut self.timing, &top.timing);
        put(&mut self.delay, &top.delay);
        put(&mut self.iteration_count, &top.iteration_count);
        put(&mut self.direction, &top.direction);
        put(&mut self.fill_mode, &top.fill_mode);
        put(&mut self.play_state, &top.play_state);
        put(&mut self.composition, &top.composition);
        put(&mut self.timeline, &top.timeline);
    }

    /// Apply one `animation` or `animation-*` declaration. `Ok(false)` for
    /// a name that is not one of them.
    pub fn apply(&mut self, ctx: &str, name: &str, value: &str) -> Result<bool, ParseError> {
        let list = || {
            crate::css::split_top_level_commas(value)
                .into_iter()
                .map(str::trim)
                .collect::<Vec<&str>>()
        };
        match name {
            "animation" => {
                // The shorthand sets every longhand it names and resets
                // `animation-timeline`; `animation-composition` is not part
                // of it.
                let entries = list()
                    .into_iter()
                    .map(|e| parse_shorthand_entry(ctx, name, e))
                    .collect::<Result<Vec<_>, _>>()?;
                *self = AnimationDeclsIr {
                    name: Some(entries.iter().map(|e| e.name.clone()).collect()),
                    duration: Some(entries.iter().map(|e| e.duration).collect()),
                    timing: Some(entries.iter().map(|e| e.timing.clone()).collect()),
                    delay: Some(entries.iter().map(|e| e.delay).collect()),
                    iteration_count: Some(entries.iter().map(|e| e.iteration_count).collect()),
                    direction: Some(entries.iter().map(|e| e.direction).collect()),
                    fill_mode: Some(entries.iter().map(|e| e.fill_mode).collect()),
                    play_state: Some(entries.iter().map(|e| e.play_state).collect()),
                    composition: self.composition.take(),
                    timeline: Some(vec![AnimationTimelineIr::Auto]),
                };
            }
            "animation-name" => {
                self.name = Some(
                    list()
                        .into_iter()
                        .map(|n| parse_name(ctx, name, n))
                        .collect::<Result<_, _>>()?,
                )
            }
            "animation-duration" => {
                self.duration = Some(
                    list()
                        .into_iter()
                        .map(|d| match d {
                            "auto" => Ok(0),
                            d => parse_duration_ms(ctx, name, d),
                        })
                        .collect::<Result<_, _>>()?,
                )
            }
            "animation-timing-function" => {
                self.timing = Some(
                    list()
                        .into_iter()
                        .map(|e| parse_easing(ctx, name, e))
                        .collect::<Result<_, _>>()?,
                )
            }
            "animation-delay" => {
                self.delay = Some(
                    list()
                        .into_iter()
                        .map(|d| parse_signed_ms(ctx, name, d))
                        .collect::<Result<_, _>>()?,
                )
            }
            "animation-iteration-count" => {
                self.iteration_count = Some(
                    list()
                        .into_iter()
                        .map(|c| parse_count(ctx, name, c))
                        .collect::<Result<_, _>>()?,
                )
            }
            "animation-direction" => {
                self.direction = Some(
                    list()
                        .into_iter()
                        .map(|d| {
                            direction(d).ok_or_else(|| {
                                bad(ctx, name, value, format!("unknown direction '{d}'"))
                            })
                        })
                        .collect::<Result<_, _>>()?,
                )
            }
            "animation-fill-mode" => {
                self.fill_mode = Some(
                    list()
                        .into_iter()
                        .map(|f| {
                            fill_mode(f).ok_or_else(|| {
                                bad(ctx, name, value, format!("unknown fill mode '{f}'"))
                            })
                        })
                        .collect::<Result<_, _>>()?,
                )
            }
            "animation-play-state" => {
                self.play_state = Some(
                    list()
                        .into_iter()
                        .map(|p| {
                            play_state(p).ok_or_else(|| {
                                bad(ctx, name, value, format!("unknown play state '{p}'"))
                            })
                        })
                        .collect::<Result<_, _>>()?,
                )
            }
            "animation-composition" => {
                let parsed: Vec<AnimationCompositionIr> = list()
                    .into_iter()
                    .map(|c| match c {
                        "replace" => Ok(AnimationCompositionIr::Replace),
                        "add" => Ok(AnimationCompositionIr::Add),
                        "accumulate" => Ok(AnimationCompositionIr::Accumulate),
                        other => Err(bad(
                            ctx,
                            name,
                            value,
                            format!("unknown composition '{other}'"),
                        )),
                    })
                    .collect::<Result<_, _>>()?;
                if parsed.iter().any(|c| *c != AnimationCompositionIr::Replace) {
                    tracing::warn!(
                        target: "lumenc::css",
                        "animation-composition: '{value}' - the desktop composes every \
                         animation with 'replace'; the web target runs it as written"
                    );
                }
                self.composition = Some(parsed);
            }
            "animation-timeline" => {
                self.timeline = Some(
                    list()
                        .into_iter()
                        .map(|t| parse_timeline(ctx, name, t))
                        .collect::<Result<_, _>>()?,
                )
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// The animations these declarations run, one per `animation-name`
    /// entry that names a keyframe block; every other list cycles over the
    /// names the way CSS repeats it.
    pub fn effective(&self) -> Vec<AnimationIr> {
        let Some(names) = &self.name else {
            return Vec::new();
        };
        fn at<T: Clone>(list: &Option<Vec<T>>, i: usize, initial: T) -> T {
            match list {
                Some(l) if !l.is_empty() => l[i % l.len()].clone(),
                _ => initial,
            }
        }
        names
            .iter()
            .enumerate()
            .filter(|(_, n)| n.as_str() != "none")
            .map(|(i, n)| AnimationIr {
                name: n.clone(),
                duration_ms: at(&self.duration, i, 0),
                timing: at(&self.timing, i, DEFAULT_EASING),
                delay_ms: at(&self.delay, i, 0),
                iteration_count: at(&self.iteration_count, i, 1.0),
                direction: at(&self.direction, i, AnimationDirectionIr::Normal),
                fill_mode: at(&self.fill_mode, i, AnimationFillModeIr::None),
                play_state: at(&self.play_state, i, AnimationPlayStateIr::Running),
                composition: at(&self.composition, i, AnimationCompositionIr::Replace),
                timeline: at(&self.timeline, i, AnimationTimelineIr::Auto),
            })
            .collect()
    }
}

/// One animation an element runs: an `animation-name` entry with the
/// longhands that apply to it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AnimationIr {
    /// The `@keyframes` block it plays.
    pub name: String,
    /// One iteration's length, in milliseconds.
    pub duration_ms: u32,
    /// The timing function across each iteration.
    pub timing: EasingIr,
    /// How long it waits before starting, in milliseconds; negative starts
    /// part-way in.
    pub delay_ms: i32,
    /// How many iterations it runs; `f32::INFINITY` for `infinite`.
    pub iteration_count: f32,
    /// Which way each iteration runs.
    pub direction: AnimationDirectionIr,
    /// Whether it applies before it starts and after it ends.
    pub fill_mode: AnimationFillModeIr,
    /// Whether it is running or paused.
    pub play_state: AnimationPlayStateIr,
    /// How its values combine with what lies under them.
    pub composition: AnimationCompositionIr,
    /// What its progress follows.
    pub timeline: AnimationTimelineIr,
}

/// One entry of an `animation` shorthand, every longhand resolved.
struct ShorthandEntry {
    name: String,
    duration: u32,
    timing: EasingIr,
    delay: i32,
    iteration_count: f32,
    direction: AnimationDirectionIr,
    fill_mode: AnimationFillModeIr,
    play_state: AnimationPlayStateIr,
}

/// One comma entry of the `animation` shorthand. Terms come in any order:
/// the first time is the duration and a second the delay; a keyword that
/// could be a value of a longhand not yet given is that value, and what is
/// left is the name, as CSS disambiguates it.
fn parse_shorthand_entry(ctx: &str, name: &str, entry: &str) -> Result<ShorthandEntry, ParseError> {
    let mut out = ShorthandEntry {
        name: "none".into(),
        duration: 0,
        timing: DEFAULT_EASING,
        delay: 0,
        iteration_count: 1.0,
        direction: AnimationDirectionIr::Normal,
        fill_mode: AnimationFillModeIr::None,
        play_state: AnimationPlayStateIr::Running,
    };
    let (mut duration, mut delay, mut timing, mut count) = (false, false, false, false);
    let (mut dir, mut fill, mut play, mut named) = (false, false, false, false);
    for term in crate::css::split_top_level_whitespace(entry) {
        if is_time(term) {
            if !duration {
                out.duration = parse_duration_ms(ctx, name, term)?;
                duration = true;
            } else if !delay {
                out.delay = parse_signed_ms(ctx, name, term)?;
                delay = true;
            } else {
                return Err(bad(ctx, name, entry, format!("a third time '{term}'")));
            }
            continue;
        }
        if !timing && let Ok(e) = parse_easing(ctx, name, term) {
            out.timing = e;
            timing = true;
            continue;
        }
        if !count && (term == "infinite" || term.parse::<f32>().is_ok()) {
            out.iteration_count = parse_count(ctx, name, term)?;
            count = true;
            continue;
        }
        if !dir && let Some(d) = direction(term) {
            out.direction = d;
            dir = true;
            continue;
        }
        if !fill && let Some(f) = fill_mode(term) {
            out.fill_mode = f;
            fill = true;
            continue;
        }
        if !play && let Some(p) = play_state(term) {
            out.play_state = p;
            play = true;
            continue;
        }
        if named {
            return Err(bad(ctx, name, entry, format!("unexpected '{term}'")));
        }
        out.name = parse_name(ctx, name, term)?;
        named = true;
    }
    // A spring with no duration runs for as long as it takes to settle.
    if !duration && let Some(settle) = out.timing.settle_ms() {
        out.duration = settle;
    }
    Ok(out)
}

fn is_time(term: &str) -> bool {
    let digits = term
        .strip_suffix("ms")
        .or_else(|| term.strip_suffix('s'))
        .unwrap_or("");
    !digits.is_empty() && digits.parse::<f32>().is_ok()
}

/// A keyframe name: an identifier or a quoted string.
fn parse_name(ctx: &str, name: &str, value: &str) -> Result<String, ParseError> {
    let v = value.trim();
    if let Some(q) = v
        .strip_prefix('"')
        .and_then(|r| r.strip_suffix('"'))
        .or_else(|| v.strip_prefix('\'').and_then(|r| r.strip_suffix('\'')))
    {
        return Ok(q.to_string());
    }
    let ident = v
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || !c.is_ascii());
    if v.is_empty() || !ident || v.starts_with(|c: char| c.is_ascii_digit()) {
        return Err(bad(ctx, name, value, "expected a keyframes name".into()));
    }
    Ok(v.to_string())
}

/// A time that may be negative, in milliseconds.
fn parse_signed_ms(ctx: &str, name: &str, value: &str) -> Result<i32, ParseError> {
    let v = value.trim();
    match v.strip_prefix('-') {
        Some(rest) => Ok(-(parse_duration_ms(ctx, name, rest)? as i32)),
        None => Ok(parse_duration_ms(ctx, name, v)? as i32),
    }
}

fn parse_count(ctx: &str, name: &str, value: &str) -> Result<f32, ParseError> {
    match value.trim() {
        "infinite" => Ok(f32::INFINITY),
        v => match v.parse::<f32>() {
            Ok(n) if n >= 0.0 => Ok(n),
            _ => Err(bad(
                ctx,
                name,
                value,
                "expected a count of zero or more, or 'infinite'".into(),
            )),
        },
    }
}

fn direction(term: &str) -> Option<AnimationDirectionIr> {
    Some(match term {
        "normal" => AnimationDirectionIr::Normal,
        "reverse" => AnimationDirectionIr::Reverse,
        "alternate" => AnimationDirectionIr::Alternate,
        "alternate-reverse" => AnimationDirectionIr::AlternateReverse,
        _ => return None,
    })
}

fn fill_mode(term: &str) -> Option<AnimationFillModeIr> {
    Some(match term {
        "none" => AnimationFillModeIr::None,
        "forwards" => AnimationFillModeIr::Forwards,
        "backwards" => AnimationFillModeIr::Backwards,
        "both" => AnimationFillModeIr::Both,
        _ => return None,
    })
}

fn play_state(term: &str) -> Option<AnimationPlayStateIr> {
    Some(match term {
        "running" => AnimationPlayStateIr::Running,
        "paused" => AnimationPlayStateIr::Paused,
        _ => return None,
    })
}

fn axis(term: &str) -> Option<ScrollAxisIr> {
    Some(match term {
        "block" => ScrollAxisIr::Block,
        "inline" => ScrollAxisIr::Inline,
        "y" | "vertical" => ScrollAxisIr::Y,
        "x" | "horizontal" => ScrollAxisIr::X,
        _ => return None,
    })
}

/// `animation-timeline`: `auto`, `none`, `scroll(...)` or `view(...)`.
fn parse_timeline(ctx: &str, name: &str, value: &str) -> Result<AnimationTimelineIr, ParseError> {
    let v = value.trim();
    let args = |f: &str| {
        v.strip_prefix(f)
            .and_then(|r| r.trim_start().strip_prefix('('))
            .and_then(|r| r.strip_suffix(')'))
            .map(|a| a.split_whitespace().collect::<Vec<&str>>())
    };
    match v {
        "auto" => return Ok(AnimationTimelineIr::Auto),
        "none" => return Ok(AnimationTimelineIr::None),
        _ => {}
    }
    if let Some(terms) = args("scroll") {
        let (mut scroller, mut ax) = (ScrollerIr::Nearest, ScrollAxisIr::Block);
        for t in terms {
            match t {
                "nearest" => scroller = ScrollerIr::Nearest,
                "root" => scroller = ScrollerIr::Root,
                "self" => scroller = ScrollerIr::SelfElement,
                t => {
                    ax = axis(t).ok_or_else(|| {
                        bad(ctx, name, value, format!("unknown scroll() term '{t}'"))
                    })?
                }
            }
        }
        return Ok(AnimationTimelineIr::Scroll(scroller, ax));
    }
    if let Some(terms) = args("view") {
        let mut ax = ScrollAxisIr::Block;
        for t in terms {
            match axis(t) {
                Some(a) => ax = a,
                None => {
                    return Err(bad(
                        ctx,
                        name,
                        value,
                        format!("view() insets are not supported ('{t}')"),
                    ));
                }
            }
        }
        return Ok(AnimationTimelineIr::View(ax));
    }
    Err(bad(
        ctx,
        name,
        value,
        "expected 'auto', 'none', 'scroll()' or 'view()' (named timelines are not supported)"
            .into(),
    ))
}

/// The browser property names a keyframe animates on the desktop, each
/// with the property the cascade files it under. Lumen's own short names
/// (`bg`, `radius`) are not browser CSS and have no place in a keyframe
/// body, which the web target writes out as authored.
pub const KEYFRAME_PROPERTIES: &[&str] = &[
    "opacity",
    "background",
    "background-color",
    "color",
    "border-color",
    "box-shadow",
    "border-radius",
    "border-top-left-radius",
    "border-top-right-radius",
    "border-bottom-right-radius",
    "border-bottom-left-radius",
    "outline-color",
    "transform",
    "translate",
    "rotate",
    "scale",
    "transform-origin",
];

/// One declaration in a keyframe, with where it sits in the block's body.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyframeDeclaration {
    /// The property, lowercased, as written.
    pub name: String,
    /// The value as written.
    pub value: String,
    /// Byte offset of the declaration in the body.
    pub at: usize,
}

/// One keyframe: the offsets its selector names and what it declares.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyframeIr {
    /// Offsets in `[0, 1]`: `from` is `0`, `to` is `1`.
    pub offsets: Vec<f32>,
    /// The declarations the desktop animates, in source order.
    pub declarations: Vec<KeyframeDeclaration>,
    /// The frame's own `animation-timing-function`, which times the way
    /// from this frame to the next.
    pub timing: Option<EasingIr>,
}

/// A note about a keyframe body: something the desktop does not animate,
/// or could not read. Never fatal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyframeWarning {
    /// Byte offset of what it is about, in the body.
    pub at: usize,
    /// What is wrong.
    pub message: String,
}

/// Parse a `@keyframes` body for the desktop: its frames in source order,
/// and a warning for each declaration the desktop does not animate. A
/// malformed frame is skipped with a warning, as CSS skips it.
pub fn parse_keyframes(body: &str) -> (Vec<KeyframeIr>, Vec<KeyframeWarning>) {
    let mut frames = Vec::new();
    let mut warnings = Vec::new();
    let mut rest = body;
    let base = body.as_ptr() as usize;
    let offset_of = |s: &str| s.as_ptr() as usize - base;
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        let Some(open) = rest.find('{') else {
            warnings.push(KeyframeWarning {
                at: offset_of(rest),
                message: "a keyframe selector with no block".into(),
            });
            break;
        };
        let selector = &rest[..open];
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            warnings.push(KeyframeWarning {
                at: offset_of(rest),
                message: "a keyframe block that is not closed".into(),
            });
            break;
        };
        let block = &after[..close];
        rest = &after[close + 1..];
        let offsets: Result<Vec<f32>, String> = selector
            .split(',')
            .map(|s| match s.trim().to_ascii_lowercase().as_str() {
                "from" => Ok(0.0),
                "to" => Ok(1.0),
                p => p
                    .strip_suffix('%')
                    .and_then(|n| n.trim().parse::<f32>().ok())
                    .filter(|n| (0.0..=100.0).contains(n))
                    .map(|n| n / 100.0)
                    .ok_or_else(|| format!("'{}' is not a keyframe selector", s.trim())),
            })
            .collect();
        let offsets = match offsets {
            Ok(o) => o,
            Err(message) => {
                warnings.push(KeyframeWarning {
                    at: offset_of(selector.trim_start()),
                    message: format!("{message}; the frame is skipped"),
                });
                continue;
            }
        };
        let mut frame = KeyframeIr {
            offsets,
            declarations: Vec::new(),
            timing: None,
        };
        for decl in block.split(';') {
            let trimmed = decl.trim_start();
            if trimmed.trim().is_empty() {
                continue;
            }
            let at = offset_of(trimmed);
            let Some((name, value)) = trimmed.split_once(':') else {
                warnings.push(KeyframeWarning {
                    at,
                    message: format!("'{}' is not a declaration", trimmed.trim()),
                });
                continue;
            };
            let name = name.trim().to_ascii_lowercase();
            let value = value
                .trim()
                .trim_end_matches("!important")
                .trim()
                .to_string();
            if name == "animation-timing-function" {
                match parse_easing("@keyframes", &name, &value) {
                    Ok(e) => frame.timing = Some(e),
                    Err(e) => warnings.push(KeyframeWarning {
                        at,
                        message: e.to_string(),
                    }),
                }
                continue;
            }
            if KEYFRAME_PROPERTIES.contains(&name.as_str()) {
                frame
                    .declarations
                    .push(KeyframeDeclaration { name, value, at });
                continue;
            }
            let message = if is_lumen_short_name(&name) {
                format!(
                    "'{name}' is Lumen's name, not browser CSS: a keyframe body is written for \
                     the browser, which ignores it, so the desktop skips it too"
                )
            } else {
                format!("'{name}' does not animate on the desktop; the web target still runs it")
            };
            warnings.push(KeyframeWarning { at, message });
        }
        frames.push(frame);
    }
    (frames, warnings)
}

/// What the desktop reports about the `@keyframes` blocks of a parsed
/// stylesheet: each warning [`parse_keyframes`] raises, at its byte offset
/// in the source the stylesheet was parsed from, with the block it is in.
pub fn keyframe_diagnostics(at_rules: &[crate::css::AtRule]) -> Vec<(usize, String)> {
    at_rules
        .iter()
        .filter(|r| r.name == "keyframes")
        .flat_map(|r| {
            let (_, warnings) = parse_keyframes(&r.body);
            warnings.into_iter().map(move |w| {
                (
                    r.at + w.at,
                    format!(
                        "css warning: @keyframes {} - {}",
                        r.prelude.trim(),
                        w.message
                    ),
                )
            })
        })
        .collect()
}

/// Lumen's own spellings of animatable properties, which a browser does
/// not know.
fn is_lumen_short_name(name: &str) -> bool {
    matches!(name, "bg" | "text-color" | "radius" | "shadow")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decls(pairs: &[(&str, &str)]) -> AnimationDeclsIr {
        let mut d = AnimationDeclsIr::default();
        for (n, v) in pairs {
            assert!(d.apply("x", n, v).unwrap(), "{n}");
        }
        d
    }

    #[test]
    fn the_shorthand_sorts_its_terms_the_css_way() {
        let d = decls(&[("animation", "spin 2s linear infinite")]);
        let a = &d.effective()[0];
        assert_eq!(a.name, "spin");
        assert_eq!(a.duration_ms, 2000);
        assert_eq!(a.timing, EasingIr::Linear);
        assert_eq!(a.iteration_count, f32::INFINITY);

        let d = decls(&[(
            "animation",
            "1s ease-in -500ms 3 alternate both paused slide, 200ms pulse",
        )]);
        let list = d.effective();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "slide");
        assert_eq!(list[0].delay_ms, -500);
        assert_eq!(list[0].iteration_count, 3.0);
        assert_eq!(list[0].direction, AnimationDirectionIr::Alternate);
        assert_eq!(list[0].fill_mode, AnimationFillModeIr::Both);
        assert_eq!(list[0].play_state, AnimationPlayStateIr::Paused);
        assert_eq!(list[1].name, "pulse");
        assert_eq!(list[1].timing, DEFAULT_EASING);
    }

    #[test]
    fn a_spring_entry_with_no_duration_takes_the_settle_time() {
        let d = decls(&[("animation", "pop spring(1 170 26)")]);
        let a = &d.effective()[0];
        assert!(a.duration_ms > 100, "{}", a.duration_ms);
        assert_eq!(Some(a.duration_ms), a.timing.settle_ms());
    }

    #[test]
    fn a_keyword_is_a_longhand_value_before_it_is_a_name() {
        // `alternate` is the direction; the name is what is left.
        let d = decls(&[("animation", "alternate 1s reverse")]);
        let a = &d.effective()[0];
        assert_eq!(a.direction, AnimationDirectionIr::Alternate);
        assert_eq!(a.name, "reverse");
    }

    #[test]
    fn longhands_cycle_over_the_names_and_none_runs_nothing() {
        let d = decls(&[
            ("animation-name", "a, none, b"),
            ("animation-duration", "1s, 2s"),
        ]);
        let list = d.effective();
        assert_eq!(list.len(), 2);
        assert_eq!((list[0].name.as_str(), list[0].duration_ms), ("a", 1000));
        assert_eq!((list[1].name.as_str(), list[1].duration_ms), ("b", 1000));
        let mut hover = AnimationDeclsIr::default();
        hover.apply("x", "animation-play-state", "paused").unwrap();
        let mut merged = d.clone();
        merged.overlay(&hover);
        assert!(
            merged
                .effective()
                .iter()
                .all(|a| a.play_state == AnimationPlayStateIr::Paused)
        );
    }

    #[test]
    fn timelines_parse() {
        let d = decls(&[
            ("animation-name", "fade"),
            ("animation-timeline", "scroll(root x), view()"),
        ]);
        assert_eq!(
            d.timeline,
            Some(vec![
                AnimationTimelineIr::Scroll(ScrollerIr::Root, ScrollAxisIr::X),
                AnimationTimelineIr::View(ScrollAxisIr::Block),
            ])
        );
        let mut bad = AnimationDeclsIr::default();
        assert!(bad.apply("x", "animation-timeline", "--named").is_err());
    }

    #[test]
    fn keyframe_bodies_read_browser_names_and_say_what_they_skip() {
        let body = "from { opacity: 0; transform: translateY(8px); filter: blur(2px) }\n\
                    50%, 75% { bg: #fff; animation-timing-function: ease-in }\n\
                    to { opacity: 1 }";
        let (frames, warnings) = parse_keyframes(body);
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0].offsets, vec![0.0]);
        let names: Vec<&str> = frames[0]
            .declarations
            .iter()
            .map(|d| d.name.as_str())
            .collect();
        assert_eq!(names, ["opacity", "transform"]);
        assert_eq!(frames[1].offsets, vec![0.5, 0.75]);
        assert!(frames[1].declarations.is_empty());
        assert_eq!(
            frames[1].timing,
            Some(EasingIr::CubicBezier(0.42, 0.0, 1.0, 1.0))
        );
        assert_eq!(warnings.len(), 2);
        assert!(warnings[0].message.contains("'filter' does not animate"));
        assert_eq!(&body[warnings[0].at..warnings[0].at + 6], "filter");
        assert!(warnings[1].message.contains("'bg' is Lumen's name"));
    }

    #[test]
    fn a_bad_selector_skips_its_frame() {
        let (frames, warnings) = parse_keyframes("120% { opacity: 1 } to { opacity: 0 }");
        assert_eq!(frames.len(), 1);
        assert_eq!(warnings.len(), 1);
    }
}
