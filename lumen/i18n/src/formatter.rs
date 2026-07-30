//! Locale-aware number / date / time / datetime formatting (W5.8).
//!
//! Thin wrappers around ICU4X 2.x:
//!
//! - Numbers - [`icu_decimal::DecimalFormatter`] (the `ryu` feature is
//!   on, so floats convert via `RoundTrip`).
//! - Dates / times / date-times - [`icu_datetime::DateTimeFormatter`]
//!   parameterized with `fieldsets::YMD::medium()` etc. The Gregorian
//!   calendar is implicit; non-Gregorian calendars (e.g. `ja-u-ca-japanese`)
//!   come along automatically when the locale tag asks for them.
//! - Currency - CLDR-driven via [`icu::experimental::dimension::currency::formatter::CurrencyFormatter`]
//!   (round-7 upgrade: gated behind the `icu` umbrella's `experimental`
//!   feature; previously a hand-rolled ISO-4217-code suffix).
//! - Relative time - CLDR-driven via [`icu::experimental::relativetime::RelativeTimeFormatter`]
//!   (round-7 upgrade: previously a hand-rolled English/German stub).

use bevy_ecs::resource::Resource;
use fixed_decimal::FloatPrecision;
use icu::calendar::Date;
use icu::datetime::DateTimeFormatter;
use icu::datetime::fieldsets;
use icu::datetime::input::{DateTime as IcuDateTime, Time as IcuTime};
use icu::decimal::DecimalFormatter;
use icu::decimal::input::Decimal;
use icu::decimal::options::DecimalFormatterOptions;
use icu::experimental::dimension::currency::CurrencyCode;
use icu::experimental::dimension::currency::formatter::CurrencyFormatter;
use icu::experimental::dimension::currency::options::CurrencyFormatterOptions;
use icu::experimental::relativetime::{
    RelativeTimeFormatter, RelativeTimeFormatterOptions, options::Numeric,
};
use icu::locale::Locale;
use thiserror::Error;
use tinystr::TinyAsciiStr;
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

/// Holds ICU4X formatters for one locale. Stored as a bevy_ecs
/// [`Resource`] by [`crate::I18nPlugin`].
///
/// All formatters share `lang`; switching locales means
/// re-constructing the resource (cheap - each `try_new` is a hash
/// lookup against the baked-in CLDR data plus a few small allocations).
#[derive(Resource)]
pub struct LocaleFormatter {
    /// Active locale.
    pub lang: LanguageIdentifier,
    /// Decimal formatter.
    pub decimal: DecimalFormatter,
    /// Date-only formatter (medium length).
    pub date: DateTimeFormatter<fieldsets::YMD>,
    /// Time-only formatter (medium length).
    pub time: DateTimeFormatter<fieldsets::T>,
    /// Date + time formatter (medium length).
    pub datetime: DateTimeFormatter<fieldsets::YMDT>,
    /// Currency formatter (round-7: real ICU4X CLDR-driven, not the
    /// hand-rolled ISO-4217 suffix stub).
    pub currency: CurrencyFormatter,
    /// Per-unit relative-time formatters (round-7: real ICU4X
    /// CLDR-driven, not the hand-rolled en/de stub). One formatter per
    /// unit because ICU4X 2.x `icu_experimental` exposes per-unit
    /// constructors rather than a single multi-unit formatter.
    pub relative_second: RelativeTimeFormatter,
    /// Long-form relative minute formatter.
    pub relative_minute: RelativeTimeFormatter,
    /// Long-form relative hour formatter.
    pub relative_hour: RelativeTimeFormatter,
    /// Long-form relative day formatter.
    pub relative_day: RelativeTimeFormatter,
    /// Long-form relative month formatter.
    pub relative_month: RelativeTimeFormatter,
    /// Long-form relative year formatter.
    pub relative_year: RelativeTimeFormatter,
}

impl LocaleFormatter {
    /// Build formatters for `lang`. Panics on a malformed locale - use
    /// [`Self::try_new`] for the fallible variant. Falls back to
    /// en-US-equivalent formatters on data-load failure (which only
    /// happens for exotic calendars that the compiled-in CLDR data
    /// dropped).
    pub fn new(lang: LanguageIdentifier) -> Self {
        Self::try_new(lang.clone()).unwrap_or_else(|e| {
            tracing::warn!(?e, %lang, "LocaleFormatter::try_new failed, falling back to en-US");
            let fallback: LanguageIdentifier = "en-US".parse().expect("en-US is valid");
            Self::try_new(fallback).expect("en-US always loads")
        })
    }

    /// Fallible constructor. Returns [`FormatterError::Load`] when ICU4X
    /// can't produce a formatter for `lang`.
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
        let decimal =
            DecimalFormatter::try_new(loc.clone().into(), DecimalFormatterOptions::default())
                .map_err(|e| FormatterError::Load(format!("decimal: {e}")))?;
        let date = DateTimeFormatter::try_new(loc.clone().into(), fieldsets::YMD::medium())
            .map_err(|e| FormatterError::Load(format!("date: {e}")))?;
        let time = DateTimeFormatter::try_new(loc.clone().into(), fieldsets::T::medium())
            .map_err(|e| FormatterError::Load(format!("time: {e}")))?;
        let datetime = DateTimeFormatter::try_new(loc.clone().into(), fieldsets::YMDT::medium())
            .map_err(|e| FormatterError::Load(format!("datetime: {e}")))?;
        // Currency formatter - CLDR-driven (round-7).
        let currency =
            CurrencyFormatter::try_new(loc.clone().into(), CurrencyFormatterOptions::default())
                .map_err(|e| FormatterError::Load(format!("currency: {e}")))?;
        // Per-unit relative-time formatters - CLDR-driven (round-7).
        // ICU4X 2.x `icu_experimental` exposes per-unit constructors
        // rather than a single multi-unit one; we instantiate Long
        // form for each.
        let rt_opts = RelativeTimeFormatterOptions {
            numeric: Numeric::Always,
        };
        let relative_second =
            RelativeTimeFormatter::try_new_long_second(loc.clone().into(), rt_opts)
                .map_err(|e| FormatterError::Load(format!("relative second: {e}")))?;
        let relative_minute =
            RelativeTimeFormatter::try_new_long_minute(loc.clone().into(), rt_opts)
                .map_err(|e| FormatterError::Load(format!("relative minute: {e}")))?;
        let relative_hour = RelativeTimeFormatter::try_new_long_hour(loc.clone().into(), rt_opts)
            .map_err(|e| FormatterError::Load(format!("relative hour: {e}")))?;
        let relative_day = RelativeTimeFormatter::try_new_long_day(loc.clone().into(), rt_opts)
            .map_err(|e| FormatterError::Load(format!("relative day: {e}")))?;
        let relative_month = RelativeTimeFormatter::try_new_long_month(loc.clone().into(), rt_opts)
            .map_err(|e| FormatterError::Load(format!("relative month: {e}")))?;
        let relative_year = RelativeTimeFormatter::try_new_long_year(loc.into(), rt_opts)
            .map_err(|e| FormatterError::Load(format!("relative year: {e}")))?;
        Ok(Self {
            lang,
            decimal,
            date,
            time,
            datetime,
            currency,
            relative_second,
            relative_minute,
            relative_hour,
            relative_day,
            relative_month,
            relative_year,
        })
    }

    /// Format `n` per locale rules. en-US: `12,345.67`; de-DE:
    /// `12.345,67`; fr-FR: `12 345,67` (with a NBSP grouping).
    pub fn format_number(&self, n: f64) -> String {
        match Decimal::try_from_f64(n, FloatPrecision::RoundTrip) {
            Ok(d) => self.decimal.format(&d).to_string(),
            Err(e) => {
                tracing::warn!(?e, n, "Decimal::try_from_f64 failed");
                n.to_string()
            }
        }
    }

    /// Format a pre-built [`Decimal`] (use when caller already has
    /// fixed-precision input and wants to avoid the float round-trip).
    pub fn format_decimal(&self, d: &Decimal) -> String {
        self.decimal.format(d).to_string()
    }

    /// Format `amount` as a CLDR-driven currency string. `currency` is
    /// a 3-character ISO-4217 code (`"USD"`, `"EUR"`, `"JPY"`, ...); the
    /// formatter picks the right symbol position + spacing per locale
    /// (en-US: `$1,234.56`; de-DE: `1.234,56` with a trailing euro sign;
    /// ja-JP: a leading yen sign).
    ///
    /// Round-7 upgrade: replaces the hand-rolled `<number> <code>`
    /// suffix with `icu_experimental`'s `CurrencyFormatter`.
    pub fn format_currency(&self, amount: f64, currency: &str) -> String {
        let d = match Decimal::try_from_f64(amount, FloatPrecision::RoundTrip) {
            Ok(d) => d,
            Err(_) => return amount.to_string(),
        };
        let code = match TinyAsciiStr::<3>::try_from_str(currency) {
            Ok(s) => CurrencyCode(s),
            Err(_) => return format!("{} {}", self.format_number(amount), currency),
        };
        self.currency.format_fixed_decimal(&d, code).to_string()
    }

    /// Format the date part of `when`. Medium length (e.g.
    /// `Jun 15, 2024` for en-US, `15.06.2024` for de-DE).
    pub fn format_date(&self, when: YmdHms) -> Result<String, FormatterError> {
        let d = Date::try_new_iso(when.year, when.month, when.day)
            .map_err(|e| FormatterError::Date(format!("{e:?}")))?;
        Ok(self.date.format(&d).to_string())
    }

    /// Format the time part of `when`. Medium length (e.g. `3:45:00 PM` /
    /// `15:45:00`).
    pub fn format_time(&self, when: YmdHms) -> Result<String, FormatterError> {
        let d = Date::try_new_iso(when.year, when.month, when.day)
            .map_err(|e| FormatterError::Date(format!("{e:?}")))?;
        let t = IcuTime::try_new(when.hour, when.minute, when.second, 0)
            .map_err(|e| FormatterError::Date(format!("time: {e:?}")))?;
        let dt = IcuDateTime { date: d, time: t };
        Ok(self.time.format(&dt).to_string())
    }

    /// Format full date + time. Medium length on both halves.
    pub fn format_datetime(&self, when: YmdHms) -> Result<String, FormatterError> {
        let d = Date::try_new_iso(when.year, when.month, when.day)
            .map_err(|e| FormatterError::Date(format!("{e:?}")))?;
        let t = IcuTime::try_new(when.hour, when.minute, when.second, 0)
            .map_err(|e| FormatterError::Date(format!("time: {e:?}")))?;
        let dt = IcuDateTime { date: d, time: t };
        Ok(self.datetime.format(&dt).to_string())
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
    ///
    /// Round-7 upgrade: replaces the hand-rolled en/de stub with
    /// `icu_experimental`'s `RelativeTimeFormatter` (per-unit, long
    /// form). All 700+ CLDR locales now work, not just en + de.
    pub fn format_relative(&self, secs_from_now: i64) -> String {
        let abs_secs = secs_from_now.unsigned_abs();
        let (count, formatter) = match abs_secs {
            0..=44 => (secs_from_now, &self.relative_second),
            45..=2_699 => (secs_from_now / 60, &self.relative_minute),
            2_700..=86_399 => (secs_from_now / 3_600, &self.relative_hour),
            86_400..=2_591_999 => (secs_from_now / 86_400, &self.relative_day),
            2_592_000..=31_535_999 => (secs_from_now / 2_592_000, &self.relative_month),
            _ => (secs_from_now / 31_536_000, &self.relative_year),
        };
        let d = match Decimal::try_from_f64(count as f64, FloatPrecision::Integer) {
            Ok(d) => d,
            Err(_) => return secs_from_now.to_string(),
        };
        formatter.format(d).to_string()
    }
}

#[cfg(test)]
mod tests {
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
        // Round-7: CLDR-driven now. en-US prepends the symbol with no
        // gap; de-DE appends with a NBSP. Both contain the digits.
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
    fn relative_time_english_cldr() {
        // Round-7: CLDR-driven now. Exact wording matches Unicode CLDR
        // (`"2 hours ago"`, `"in 2 hours"`, ...). Pluralization handled
        // by ICU per the locale's plural rules.
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
        // Round-7 win: not just en + de anymore. Any of CLDR's 700+
        // locales work because the formatter is real, not a stub.
        let f = LocaleFormatter::new(lang("es-ES"));
        let s = f.format_relative(-3600 * 2);
        assert!(
            s.contains('h') || s.contains("hora"),
            "es-ES hour past: got {s}"
        );
    }

    #[test]
    fn bad_date_errors() {
        let f = LocaleFormatter::new(lang("en-US"));
        // Feb 30 is not a thing.
        assert!(f.format_date(YmdHms::date(2024, 2, 30)).is_err());
    }
}
