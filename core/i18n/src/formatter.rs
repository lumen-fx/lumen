//! Locale-aware number / date / time / datetime formatting.
//!
//! Thin wrappers around ICU4X 2.x, each built the first time something
//! formats with it:
//!
//! - Numbers - [`icu_decimal::DecimalFormatter`] (the `ryu` feature is
//!   on, so floats convert via `RoundTrip`).
//! - Dates / times / date-times - [`icu_datetime::DateTimeFormatter`]
//!   parameterized with `fieldsets::YMD::medium()` etc. The Gregorian
//!   calendar is implicit; non-Gregorian calendars (e.g. `ja-u-ca-japanese`)
//!   come along automatically when the locale tag asks for them.
//! - Currency - CLDR-driven via [`icu::experimental::dimension::currency::formatter::CurrencyFormatter`]
//!   (behind the `icu` umbrella's `unstable` feature). A
//!   `CurrencyFormatter` is bound to one ISO-4217 code, so
//!   [`LocaleFormatter`] builds them on demand and keeps them in a
//!   per-locale cache.
//! - Relative time - CLDR-driven via [`icu::experimental::relativetime::RelativeTimeFormatter`],
//!   one formatter per unit.
//!
//! [`format_spec`] is the one place a spec string becomes formatted text.
//! Markup's `format` attribute carries the spec verbatim and the scripts'
//! `format_*` builtins build one, so both front doors end at the same
//! dispatch and every spec has one meaning.

use std::array;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::str::FromStr;
use std::sync::{Mutex, OnceLock, PoisonError};

use fixed_decimal::FloatPrecision;
use icu::calendar::Date;
use icu::datetime::DateTimeFormatter;
use icu::datetime::fieldsets;
use icu::datetime::input::{DateTime as IcuDateTime, Time as IcuTime};
use icu::decimal::DecimalFormatter;
use icu::decimal::input::Decimal;
use icu::decimal::options::DecimalFormatterOptions;
use icu::experimental::dimension::currency::CurrencyType;
use icu::experimental::dimension::currency::formatter::{
    CurrencyFormatter, CurrencyFormatterPreferences,
};
use icu::experimental::dimension::currency::options::CurrencyFormatterOptions;
use icu::experimental::relativetime::{
    RelativeTimeFormatter, RelativeTimeFormatterOptions, options::Numeric,
};
use icu::locale::Locale;
use thiserror::Error;
use unic_langid::LanguageIdentifier;

/// Errors surfaced by [`LocaleFormatter`] construction or formatting.
#[derive(Debug, Error)]
pub enum FormatterError {
    /// ICU4X failed to load locale data (e.g. requested a calendar the
    /// compiled-in data doesn't include).
    #[error("icu4x load error: {0}")]
    Load(String),
    /// Input value wouldn't fit ICU4X's fixed-decimal limits.
    #[error("decimal limit error: {0}")]
    Decimal(String),
    /// Date constructor rejected the y/m/d triple (e.g. Feb 30).
    #[error("date error: {0}")]
    Date(String),
}

/// Ymd date input for [`LocaleFormatter::format_date`] etc. Plain
/// triple to keep the surface area Send + Sync without pulling
/// `chrono` / `time` into the dependency graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct YmdHms {
    /// Proleptic-Gregorian year (positive AD, negative BC).
    pub year: i32,
    /// 1-12.
    pub month: u8,
    /// 1-31.
    pub day: u8,
    /// 0-23.
    pub hour: u8,
    /// 0-59.
    pub minute: u8,
    /// 0-60 (leap second allowed by ICU4X).
    pub second: u8,
}

impl YmdHms {
    /// Convenience for callers without a time component (sets H/M/S to 0).
    pub fn date(year: i32, month: u8, day: u8) -> Self {
        Self {
            year,
            month,
            day,
            hour: 0,
            minute: 0,
            second: 0,
        }
    }

    /// Convenience for callers with a full time.
    pub fn datetime(year: i32, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> Self {
        Self {
            year,
            month,
            day,
            hour,
            minute,
            second,
        }
    }
}

impl FromStr for YmdHms {
    type Err = FormatterError;

    /// Parse `YYYY-MM-DD`, optionally followed by `T` or a space and
    /// `HH:MM[:SS]`.
    ///
    /// A fractional-seconds part and a trailing zone designator (`Z`,
    /// `+HH:MM`, `-HH:MM`) are accepted and discarded, so an RFC-3339
    /// timestamp from an API parses. `YmdHms` carries no zone and no
    /// formatter converts one, so the calendar fields are formatted
    /// exactly as they were written.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        let (date, clock) = match s.find(['T', 't', ' ']) {
            Some(at) => (&s[..at], Some(s[at + 1..].trim())),
            None => (s, None),
        };
        let ymd: Vec<&str> = date.split('-').collect();
        let [year, month, day] = ymd[..] else {
            return Err(FormatterError::Date(format!("not a YYYY-MM-DD date: {s}")));
        };
        let mut out = Self::date(
            year.parse().map_err(|_| bad_field("year", s))?,
            month.parse().map_err(|_| bad_field("month", s))?,
            day.parse().map_err(|_| bad_field("day", s))?,
        );
        let Some(clock) = clock else {
            return Ok(out);
        };
        // Everything from the zone designator on is dropped, then the
        // fractional part: neither reaches the calendar fields.
        let zoneless = match clock.find(['Z', 'z', '+']).or_else(|| clock.rfind('-')) {
            Some(at) => &clock[..at],
            None => clock,
        };
        let hms: Vec<&str> = zoneless
            .split('.')
            .next()
            .unwrap_or(zoneless)
            .trim()
            .split(':')
            .collect();
        let (hour, minute, second) = match hms[..] {
            [h, m] => (h, m, "0"),
            [h, m, sec] => (h, m, sec),
            _ => return Err(FormatterError::Date(format!("not an HH:MM[:SS] time: {s}"))),
        };
        out.hour = hour.parse().map_err(|_| bad_field("hour", s))?;
        out.minute = minute.parse().map_err(|_| bad_field("minute", s))?;
        out.second = second.parse().map_err(|_| bad_field("second", s))?;
        Ok(out)
    }
}

fn bad_field(field: &str, input: &str) -> FormatterError {
    FormatterError::Date(format!("bad {field} in {input}"))
}

/// The units [`LocaleFormatter::format_relative`] picks between, in
/// CLDR threshold order. Each one indexes its own cell in the
/// formatter's relative-time cache.
#[derive(Clone, Copy, Debug)]
enum RelUnit {
    Second,
    Minute,
    Hour,
    Day,
    Month,
    Year,
}

impl RelUnit {
    /// How many cells the relative-time cache holds, one per variant.
    const COUNT: usize = 6;

    /// Which cell this unit's formatter lives in.
    fn slot(self) -> usize {
        self as usize
    }
}

/// ICU4X's spelling of en-US, the locale a formatter falls back to when
/// the compiled-in CLDR data does not cover the one that was asked for.
fn en_us() -> Locale {
    "en-US".parse().expect("en-US is valid")
}

/// Holds ICU4X formatters for one locale. [`crate::I18nPlugin`] builds
/// one at startup and installs it behind the opaque handle
/// [`lumen_core::i18n::AppI18n`] carries.
///
/// Construction holds the locale and nothing else. Each formatter is
/// built the first time something formats with it and then kept, so an
/// app that formats only numbers never pays for the date-time or
/// relative-time data, and an app that formats nothing pays for none of
/// it.
pub struct LocaleFormatter {
    /// Active locale, as it was asked for.
    pub lang: LanguageIdentifier,
    /// ICU4X's spelling of `lang`, kept so each formatter can be built
    /// the first time something asks for it.
    loc: Locale,
    /// Decimal formatter.
    decimal: OnceLock<DecimalFormatter>,
    /// Date-only formatter (medium length).
    date: OnceLock<DateTimeFormatter<fieldsets::YMD>>,
    /// Time-only formatter (medium length).
    time: OnceLock<DateTimeFormatter<fieldsets::T>>,
    /// Date + time formatter (medium length).
    datetime: OnceLock<DateTimeFormatter<fieldsets::YMDT>>,
    /// Per-unit relative-time formatters: ICU4X 2.x `icu_experimental`
    /// exposes per-unit constructors rather than a single multi-unit
    /// formatter. The unit set is closed and known here, so this cache is
    /// an array of cells and takes no lock, which is why it does not look
    /// like the currency cache below.
    relative: [OnceLock<RelativeTimeFormatter>; RelUnit::COUNT],
    /// Currency preferences for `lang`, kept so per-code formatters can
    /// be built after construction.
    currency_prefs: CurrencyFormatterPreferences,
    /// Currency formatters keyed by ISO-4217 code. ICU4X binds the code
    /// at construction, so one formatter cannot serve every currency, and
    /// the set of codes markup can name is open, so this cache needs a
    /// map and a lock where the relative-time one above does not. It
    /// keeps construction to once per (locale, code) rather than once per
    /// [`LocaleFormatter::format_currency`] call.
    currency: Mutex<HashMap<CurrencyType, CurrencyFormatter<DecimalFormatter>>>,
}

impl LocaleFormatter {
    /// Formatters for `lang`, each built when something first formats
    /// with it. Falls back to en-US when ICU4X will not parse the tag;
    /// use [`Self::try_new`] to see that failure instead.
    pub fn new(lang: LanguageIdentifier) -> Self {
        Self::try_new(lang.clone()).unwrap_or_else(|e| {
            tracing::warn!(?e, %lang, "LocaleFormatter::try_new failed, falling back to en-US");
            let fallback: LanguageIdentifier = "en-US".parse().expect("en-US is valid");
            Self::try_new(fallback).expect("en-US always loads")
        })
    }

    /// Fallible constructor. Returns [`FormatterError::Load`] when ICU4X
    /// will not parse `lang`, which is the only failure left here now
    /// that each formatter loads its data on first use. A locale ICU
    /// parses but has no data for falls back to en-US one formatter at a
    /// time, at the call that needs it.
    pub fn try_new(lang: LanguageIdentifier) -> Result<Self, FormatterError> {
        // `unic_langid::LanguageIdentifier` (Fluent's tag type) and
        // `icu::locale::Locale` (ICU4X's tag type) are not the same
        // Rust type, but both parse the same BCP-47 string. We round-
        // trip through the textual form to bridge them - cheap (one
        // alloc) and keeps the rest of the crate single-typed on the
        // Fluent tag.
        let loc: Locale = lang
            .to_string()
            .parse()
            .map_err(|e| FormatterError::Load(format!("locale parse: {e}")))?;
        // Currency formatters are built per ISO-4217 code on first use;
        // all we need up front is the locale's preferences.
        let currency_prefs = CurrencyFormatterPreferences::from(loc.clone());
        Ok(Self {
            lang,
            loc,
            decimal: OnceLock::new(),
            date: OnceLock::new(),
            time: OnceLock::new(),
            datetime: OnceLock::new(),
            relative: array::from_fn(|_| OnceLock::new()),
            currency_prefs,
            currency: Mutex::new(HashMap::new()),
        })
    }

    /// The decimal formatter, built on the first call.
    fn decimal(&self) -> &DecimalFormatter {
        self.decimal.get_or_init(|| {
            Self::build_decimal(&self.loc).unwrap_or_else(|e| {
                tracing::warn!(?e, lang = %self.lang, "decimal data missing, using en-US");
                Self::build_decimal(&en_us()).expect("en-US always loads")
            })
        })
    }

    /// The date-only formatter, built on the first call.
    fn date(&self) -> &DateTimeFormatter<fieldsets::YMD> {
        self.date.get_or_init(|| {
            Self::build_date(&self.loc).unwrap_or_else(|e| {
                tracing::warn!(?e, lang = %self.lang, "date data missing, using en-US");
                Self::build_date(&en_us()).expect("en-US always loads")
            })
        })
    }

    /// The time-only formatter, built on the first call.
    fn time(&self) -> &DateTimeFormatter<fieldsets::T> {
        self.time.get_or_init(|| {
            Self::build_time(&self.loc).unwrap_or_else(|e| {
                tracing::warn!(?e, lang = %self.lang, "time data missing, using en-US");
                Self::build_time(&en_us()).expect("en-US always loads")
            })
        })
    }

    /// The date + time formatter, built on the first call.
    fn datetime(&self) -> &DateTimeFormatter<fieldsets::YMDT> {
        self.datetime.get_or_init(|| {
            Self::build_datetime(&self.loc).unwrap_or_else(|e| {
                tracing::warn!(?e, lang = %self.lang, "datetime data missing, using en-US");
                Self::build_datetime(&en_us()).expect("en-US always loads")
            })
        })
    }

    /// `unit`'s relative-time formatter, built on the first call for that
    /// unit. An app that only ever formats hours loads the hour data and
    /// nothing else.
    fn relative(&self, unit: RelUnit) -> &RelativeTimeFormatter {
        self.relative[unit.slot()].get_or_init(|| {
            Self::build_relative(&self.loc, unit).unwrap_or_else(|e| {
                tracing::warn!(
                    ?e,
                    ?unit,
                    lang = %self.lang,
                    "relative-time data missing, using en-US"
                );
                Self::build_relative(&en_us(), unit).expect("en-US always loads")
            })
        })
    }

    fn build_decimal(loc: &Locale) -> Result<DecimalFormatter, FormatterError> {
        DecimalFormatter::try_new(loc.clone().into(), DecimalFormatterOptions::default())
            .map_err(|e| FormatterError::Load(format!("decimal: {e}")))
    }

    fn build_date(loc: &Locale) -> Result<DateTimeFormatter<fieldsets::YMD>, FormatterError> {
        DateTimeFormatter::try_new(loc.clone().into(), fieldsets::YMD::medium())
            .map_err(|e| FormatterError::Load(format!("date: {e}")))
    }

    fn build_time(loc: &Locale) -> Result<DateTimeFormatter<fieldsets::T>, FormatterError> {
        DateTimeFormatter::try_new(loc.clone().into(), fieldsets::T::medium())
            .map_err(|e| FormatterError::Load(format!("time: {e}")))
    }

    fn build_datetime(loc: &Locale) -> Result<DateTimeFormatter<fieldsets::YMDT>, FormatterError> {
        DateTimeFormatter::try_new(loc.clone().into(), fieldsets::YMDT::medium())
            .map_err(|e| FormatterError::Load(format!("datetime: {e}")))
    }

    fn build_relative(
        loc: &Locale,
        unit: RelUnit,
    ) -> Result<RelativeTimeFormatter, FormatterError> {
        // `RelativeTimeFormatterOptions` is non-exhaustive since
        // icu_experimental 0.5: build from Default, then set the field we
        // care about.
        let mut opts = RelativeTimeFormatterOptions::default();
        opts.numeric = Numeric::Always;
        let prefs = loc.clone().into();
        let built = match unit {
            RelUnit::Second => RelativeTimeFormatter::try_new_long_second(prefs, opts),
            RelUnit::Minute => RelativeTimeFormatter::try_new_long_minute(prefs, opts),
            RelUnit::Hour => RelativeTimeFormatter::try_new_long_hour(prefs, opts),
            RelUnit::Day => RelativeTimeFormatter::try_new_long_day(prefs, opts),
            RelUnit::Month => RelativeTimeFormatter::try_new_long_month(prefs, opts),
            RelUnit::Year => RelativeTimeFormatter::try_new_long_year(prefs, opts),
        };
        built.map_err(|e| FormatterError::Load(format!("relative {unit:?}: {e}")))
    }

    /// Format `n` per locale rules. en-US: `12,345.67`; de-DE:
    /// `12.345,67`; fr-FR: `12 345,67` (with a NBSP grouping).
    pub fn format_number(&self, n: f64) -> String {
        match Decimal::try_from_f64(n, FloatPrecision::RoundTrip) {
            Ok(d) => self.decimal().format(&d).to_string(),
            Err(e) => {
                tracing::warn!(?e, n, "Decimal::try_from_f64 failed");
                n.to_string()
            }
        }
    }

    /// Format a pre-built [`Decimal`] (use when caller already has
    /// fixed-precision input and wants to avoid the float round-trip).
    pub fn format_decimal(&self, d: &Decimal) -> String {
        self.decimal().format(d).to_string()
    }

    /// Format `amount` as a CLDR-driven currency string. `currency` is
    /// a 3-character ISO-4217 code (`"USD"`, `"EUR"`, `"JPY"`, ...,
    /// case-insensitive); the formatter picks the symbol position and
    /// spacing the locale wants (en-US: `$1,234.56`; de-DE: `1.234,56`
    /// with a trailing euro sign; ja-JP: a leading yen sign) and rounds
    /// to the currency's own fraction digits (two for USD and EUR, none
    /// for JPY).
    ///
    /// The short (standard) symbol is used, not the narrow symbol and
    /// not the ISO code. An unusable code falls back to
    /// `<number> <code>`.
    pub fn format_currency(&self, amount: f64, currency: &str) -> String {
        let d = match Decimal::try_from_f64(amount, FloatPrecision::RoundTrip) {
            Ok(d) => d,
            Err(_) => return amount.to_string(),
        };
        let code = match CurrencyType::try_from_str(currency) {
            Ok(c) => c,
            Err(_) => return format!("{} {}", self.format_number(amount), currency),
        };
        let mut cache = self.currency.lock().unwrap_or_else(PoisonError::into_inner);
        let formatter = match cache.entry(code) {
            Entry::Occupied(slot) => slot.into_mut(),
            Entry::Vacant(slot) => {
                let built = CurrencyFormatter::try_new_symbol(
                    self.currency_prefs,
                    code,
                    CurrencyFormatterOptions::default(),
                );
                match built {
                    Ok(f) => slot.insert(f),
                    Err(e) => {
                        tracing::warn!(?e, currency, "currency formatter load failed");
                        return format!("{} {}", self.format_number(amount), currency);
                    }
                }
            }
        };
        formatter.format_fixed_decimal(&d).to_string()
    }

    /// Format the date part of `when`. Medium length (e.g.
    /// `Jun 15, 2024` for en-US, `15.06.2024` for de-DE).
    pub fn format_date(&self, when: YmdHms) -> Result<String, FormatterError> {
        let d = Date::try_new_iso(when.year, when.month, when.day)
            .map_err(|e| FormatterError::Date(format!("{e:?}")))?;
        Ok(self.date().format(&d).to_string())
    }

    /// Format the time part of `when`. Medium length (e.g. `3:45:00 PM` /
    /// `15:45:00`).
    pub fn format_time(&self, when: YmdHms) -> Result<String, FormatterError> {
        let d = Date::try_new_iso(when.year, when.month, when.day)
            .map_err(|e| FormatterError::Date(format!("{e:?}")))?;
        let t = IcuTime::try_new(when.hour, when.minute, when.second, 0)
            .map_err(|e| FormatterError::Date(format!("time: {e:?}")))?;
        let dt = IcuDateTime { date: d, time: t };
        Ok(self.time().format(&dt).to_string())
    }

    /// Format full date + time. Medium length on both halves.
    pub fn format_datetime(&self, when: YmdHms) -> Result<String, FormatterError> {
        let d = Date::try_new_iso(when.year, when.month, when.day)
            .map_err(|e| FormatterError::Date(format!("{e:?}")))?;
        let t = IcuTime::try_new(when.hour, when.minute, when.second, 0)
            .map_err(|e| FormatterError::Date(format!("time: {e:?}")))?;
        let dt = IcuDateTime { date: d, time: t };
        Ok(self.datetime().format(&dt).to_string())
    }

    /// Format `secs_from_now` as a CLDR-driven relative-time string.
    /// `"2 hours ago"` / `"in 5 minutes"` per locale (CLDR includes
    /// the right past / future grammar for every locale, plural
    /// categories, etc).
    ///
    /// Picks the unit by magnitude using CLDR thresholds (45 s ->
    /// seconds; 45 min -> minutes; 24 h -> hours; 30 d -> days; 12 mo ->
    /// months; else years). The matching per-unit formatter handles
    /// localization. Past is negative, future is positive.
    pub fn format_relative(&self, secs_from_now: i64) -> String {
        let abs_secs = secs_from_now.unsigned_abs();
        let (count, unit) = match abs_secs {
            0..=44 => (secs_from_now, RelUnit::Second),
            45..=2_699 => (secs_from_now / 60, RelUnit::Minute),
            2_700..=86_399 => (secs_from_now / 3_600, RelUnit::Hour),
            86_400..=2_591_999 => (secs_from_now / 86_400, RelUnit::Day),
            2_592_000..=31_535_999 => (secs_from_now / 2_592_000, RelUnit::Month),
            _ => (secs_from_now / 31_536_000, RelUnit::Year),
        };
        let d = match Decimal::try_from_f64(count as f64, FloatPrecision::Integer) {
            Ok(d) => d,
            Err(_) => return secs_from_now.to_string(),
        };
        self.relative(unit).format(d).to_string()
    }
}

/// Format `value` the way `spec` asks for, in `fmt`'s locale.
///
/// The spec set, closed:
///
/// - `number` - `value` is a decimal number.
/// - `currency:<code>` - `value` is a decimal amount, `<code>` an
///   ISO-4217 currency code (`currency:EUR`).
/// - `date` / `time` / `datetime` - `value` is `YYYY-MM-DD`, optionally
///   with a time; see [`YmdHms`]'s `FromStr` for what it accepts and
///   what it discards.
/// - `relative` - `value` is whole seconds from now, past negative.
///
/// A spec outside that set, or a value the spec cannot parse, is `None`.
/// Every caller shows the value unchanged then, the way an unresolved
/// translation key shows itself: a mistyped spec must not blank an
/// element, and neither must data that is briefly the wrong shape.
pub fn format_spec(fmt: &LocaleFormatter, spec: &str, value: &str) -> Option<String> {
    let value = value.trim();
    match spec.trim() {
        "number" => Some(fmt.format_number(value.parse().ok()?)),
        "date" => fmt.format_date(value.parse().ok()?).ok(),
        "time" => fmt.format_time(value.parse().ok()?).ok(),
        "datetime" => fmt.format_datetime(value.parse().ok()?).ok(),
        "relative" => Some(fmt.format_relative(value.parse().ok()?)),
        other => {
            let code = other.strip_prefix("currency:")?.trim();
            if code.is_empty() {
                return None;
            }
            Some(fmt.format_currency(value.parse().ok()?, code))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::thread;

    use super::*;

    fn lang(s: &str) -> LanguageIdentifier {
        s.parse().expect("test lang parses")
    }

    #[test]
    fn number_en_us_uses_comma_grouping_dot_decimal() {
        let f = LocaleFormatter::new(lang("en-US"));
        let s = f.format_number(12_345.67);
        assert!(s.contains("12,345"), "en-US should group with comma: {s}");
        assert!(s.contains('.'), "en-US should use dot as decimal: {s}");
    }

    #[test]
    fn number_de_de_uses_dot_grouping_comma_decimal() {
        let f = LocaleFormatter::new(lang("de-DE"));
        let s = f.format_number(12_345.67);
        assert!(s.contains("12.345"), "de-DE should group with dot: {s}");
        assert!(s.contains(','), "de-DE should use comma as decimal: {s}");
    }

    #[test]
    fn date_en_us_and_de_de_differ() {
        let en = LocaleFormatter::new(lang("en-US"));
        let de = LocaleFormatter::new(lang("de-DE"));
        let when = YmdHms::date(2024, 6, 15);
        let en_s = en.format_date(when).unwrap();
        let de_s = de.format_date(when).unwrap();
        assert_ne!(en_s, de_s, "en-US and de-DE date strings should differ");
        // de-DE should put the day first; en-US should put it later.
        assert!(
            de_s.starts_with("15"),
            "de-DE date should start with day: {de_s}"
        );
    }

    #[test]
    fn currency_uses_locale_symbol_position() {
        // en-US prepends the symbol with no gap; de-DE appends with a
        // NBSP. Both contain the digits.
        let en = LocaleFormatter::new(lang("en-US"));
        let de = LocaleFormatter::new(lang("de-DE"));
        let en_s = en.format_currency(1234.5, "EUR");
        let de_s = de.format_currency(1234.5, "EUR");
        assert!(en_s.starts_with('\u{20ac}'), "en-US euro prefix: {en_s}");
        assert!(de_s.contains('\u{20ac}'), "de-DE euro present: {de_s}");
        assert!(en_s.contains("1,234"), "en-US comma grouping: {en_s}");
        assert!(de_s.contains("1.234"), "de-DE dot grouping: {de_s}");
    }

    #[test]
    fn currency_uses_the_currency_fraction_digits() {
        // USD and EUR round to two digits, JPY to none, per CLDR's
        // currency fraction data.
        let f = LocaleFormatter::new(lang("en-US"));
        assert_eq!(f.format_currency(1234.5, "USD"), "$1,234.50");
        assert_eq!(f.format_currency(1234.5, "JPY"), "\u{a5}1,235");
    }

    #[test]
    fn currency_code_is_case_insensitive() {
        let f = LocaleFormatter::new(lang("en-US"));
        assert_eq!(
            f.format_currency(9.99, "usd"),
            f.format_currency(9.99, "USD")
        );
    }

    #[test]
    fn currency_bad_code_falls_back_to_number_and_code() {
        let f = LocaleFormatter::new(lang("en-US"));
        // Not three ASCII letters, so there is no ISO-4217 code to look up.
        assert_eq!(f.format_currency(1234.5, "US"), "1,234.5 US");
        assert_eq!(f.format_currency(1234.5, "US1"), "1,234.5 US1");
    }

    #[test]
    fn currency_cache_keeps_codes_apart() {
        // One `LocaleFormatter` serves many codes; the second lookup of a
        // code must come back with that code's symbol, not the first one's.
        let f = LocaleFormatter::new(lang("en-US"));
        let usd = f.format_currency(1234.5, "USD");
        let eur = f.format_currency(1234.5, "EUR");
        assert_eq!(usd, "$1,234.50");
        assert_eq!(eur, "\u{20ac}1,234.50");
        assert_eq!(f.format_currency(1234.5, "USD"), usd);
        assert_eq!(f.format_currency(1234.5, "EUR"), eur);
    }

    #[test]
    fn relative_cache_keeps_units_apart() {
        // One cell per unit, so a cell wired to the wrong unit is the bug
        // to catch: every unit keeps answering in its own words, and
        // answers the same way the second time.
        let f = LocaleFormatter::new(lang("en-US"));
        let cases = [
            (-10_i64, "second"),
            (-120, "minute"),
            (-3600 * 2, "hour"),
            (-86_400 * 3, "day"),
            (-2_592_000 * 2, "month"),
            (-31_536_000 * 3, "year"),
        ];
        for (secs, unit) in cases {
            let first = f.format_relative(secs);
            assert!(
                first.contains(unit),
                "{secs} seconds should read in {unit}s: {first}"
            );
            assert_eq!(f.format_relative(secs), first, "second read of {unit}");
        }
    }

    #[test]
    fn formatters_are_shared_across_threads() {
        // The cells fill on first use, so several threads can race for
        // one. `OnceLock` picks a single winner; this asserts every
        // accessor reads that winner and formats the same afterwards.
        let f = Arc::new(LocaleFormatter::new(lang("de-DE")));
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let f = Arc::clone(&f);
                thread::spawn(move || match i % 4 {
                    0 => f.format_number(12_345.67),
                    1 => f
                        .format_date(YmdHms::date(2024, 6, 15))
                        .expect("June 15 formats"),
                    2 => f.format_currency(1234.5, "EUR"),
                    _ => f.format_relative(-3600 * 2),
                })
            })
            .collect();
        for t in threads {
            t.join().expect("no formatting thread panics");
        }
        let number = f.format_number(12_345.67);
        assert!(number.contains("12.345"), "de-DE dot grouping: {number}");
        let date = f.format_date(YmdHms::date(2024, 6, 15)).unwrap();
        assert!(date.starts_with("15"), "de-DE day first: {date}");
        let money = f.format_currency(1234.5, "EUR");
        assert!(money.contains('\u{20ac}'), "de-DE euro present: {money}");
        let ago = f.format_relative(-3600 * 2);
        assert!(ago.contains("Stunde"), "de-DE 2 hours past: {ago}");
    }

    #[test]
    fn relative_time_english_cldr() {
        // Exact wording matches Unicode CLDR (`"2 hours ago"`,
        // `"in 2 hours"`, ...). Pluralization is handled by ICU per the
        // locale's plural rules.
        let f = LocaleFormatter::new(lang("en-US"));
        assert_eq!(f.format_relative(-3600 * 2), "2 hours ago");
        assert_eq!(f.format_relative(3600 * 2), "in 2 hours");
        assert!(
            f.format_relative(-60).contains("minute"),
            "single-minute past: got {}",
            f.format_relative(-60)
        );
    }

    #[test]
    fn relative_time_german_cldr() {
        let f = LocaleFormatter::new(lang("de-DE"));
        assert!(
            f.format_relative(-3600 * 2).contains("Stunde"),
            "2 hours past: got {}",
            f.format_relative(-3600 * 2)
        );
        assert!(
            f.format_relative(3600 * 2).contains("Stunde"),
            "2 hours future: got {}",
            f.format_relative(3600 * 2)
        );
    }

    #[test]
    fn relative_time_spanish_now_works_too() {
        // Any of CLDR's locales format, not just en + de.
        let f = LocaleFormatter::new(lang("es-ES"));
        let s = f.format_relative(-3600 * 2);
        assert!(
            s.contains('h') || s.contains("hora"),
            "es-ES hour past: got {s}"
        );
    }

    #[test]
    fn parses_dates_datetimes_and_zoned_timestamps() {
        assert_eq!(
            "2024-06-15".parse::<YmdHms>().unwrap(),
            YmdHms::date(2024, 6, 15)
        );
        assert_eq!(
            "2024-06-15T09:30".parse::<YmdHms>().unwrap(),
            YmdHms::datetime(2024, 6, 15, 9, 30, 0)
        );
        assert_eq!(
            "2024-06-15 09:30:45".parse::<YmdHms>().unwrap(),
            YmdHms::datetime(2024, 6, 15, 9, 30, 45)
        );
        // The fraction and the zone are dropped, so the calendar fields
        // are what the timestamp wrote and nothing is converted.
        assert_eq!(
            "2024-06-15T09:30:45.250Z".parse::<YmdHms>().unwrap(),
            YmdHms::datetime(2024, 6, 15, 9, 30, 45)
        );
        assert_eq!(
            "2024-06-15T09:30:45+02:00".parse::<YmdHms>().unwrap(),
            YmdHms::datetime(2024, 6, 15, 9, 30, 45)
        );
        assert_eq!(
            "2024-06-15T09:30:45-05:00".parse::<YmdHms>().unwrap(),
            YmdHms::datetime(2024, 6, 15, 9, 30, 45)
        );
    }

    #[test]
    fn unparseable_dates_error() {
        assert!("not a date".parse::<YmdHms>().is_err());
        assert!("2024-06".parse::<YmdHms>().is_err());
        assert!("2024-06-15T09".parse::<YmdHms>().is_err());
    }

    #[test]
    fn format_spec_covers_the_whole_set() {
        let de = LocaleFormatter::new(lang("de-DE"));
        assert_eq!(
            format_spec(&de, "number", "12345.678").unwrap(),
            de.format_number(12_345.678)
        );
        assert_eq!(
            format_spec(&de, "currency:EUR", "1234.5").unwrap(),
            de.format_currency(1234.5, "EUR")
        );
        assert_eq!(
            format_spec(&de, "date", "2024-06-15").unwrap(),
            de.format_date(YmdHms::date(2024, 6, 15)).unwrap()
        );
        assert_eq!(
            format_spec(&de, "time", "2024-06-15T09:30:45").unwrap(),
            de.format_time(YmdHms::datetime(2024, 6, 15, 9, 30, 45))
                .unwrap()
        );
        assert_eq!(
            format_spec(&de, "datetime", "2024-06-15T09:30:45").unwrap(),
            de.format_datetime(YmdHms::datetime(2024, 6, 15, 9, 30, 45))
                .unwrap()
        );
        assert_eq!(
            format_spec(&de, "relative", "-7200").unwrap(),
            de.format_relative(-7200)
        );
    }

    #[test]
    fn format_spec_declines_a_bad_spec_or_value() {
        let f = LocaleFormatter::new(lang("en-US"));
        assert_eq!(format_spec(&f, "wat", "hello"), None);
        assert_eq!(format_spec(&f, "currency:", "1234.5"), None);
        assert_eq!(format_spec(&f, "number", "not a number"), None);
        assert_eq!(format_spec(&f, "date", "not a date"), None);
        // Feb 30 parses as a triple and then fails the calendar.
        assert_eq!(format_spec(&f, "date", "2024-02-30"), None);
        assert_eq!(format_spec(&f, "relative", "soon"), None);
    }

    #[test]
    fn format_spec_reads_the_locale() {
        let en = LocaleFormatter::new(lang("en-US"));
        let de = LocaleFormatter::new(lang("de-DE"));
        assert_ne!(
            format_spec(&en, "number", "1234.5"),
            format_spec(&de, "number", "1234.5")
        );
    }

    #[test]
    fn bad_date_errors() {
        let f = LocaleFormatter::new(lang("en-US"));
        // Feb 30 is not a thing.
        assert!(f.format_date(YmdHms::date(2024, 2, 30)).is_err());
    }
}
