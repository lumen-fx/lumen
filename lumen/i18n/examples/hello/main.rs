//! Tiny demo: load the en-US + de-DE FTL files, format a greeting,
//! and format `12 345,67` in three locales.
//!
//! Run with: `cargo run -p lumen-i18n --example hello`.

use lumen_i18n::{I18n, LocaleFormatter, t};
use unic_langid::LanguageIdentifier;

const EN_US_FTL: &str = include_str!("en-US.ftl");
const DE_DE_FTL: &str = include_str!("de-DE.ftl");

fn main() {
    let en: LanguageIdentifier = "en-US".parse().unwrap();
    let de: LanguageIdentifier = "de-DE".parse().unwrap();

    let mut i18n = I18n::new(de.clone(), vec![en.clone()]);
    i18n.load_ftl(en.clone(), EN_US_FTL).unwrap();
    i18n.load_ftl(de.clone(), DE_DE_FTL).unwrap();

    // Translation - pulls from `current` (de-DE) first.
    let greeting: String = t!(i18n, "greet", name = "Alice");
    println!("{greeting}"); // Hallo, Alice!

    let title: String = t!(i18n, "app-title");
    println!("{title}"); // Hallo Welt

    // Switch locales - fallback chain kicks in for missing keys.
    i18n.set_current(en.clone());
    let greeting_en: String = t!(i18n, "greet", name = "Alice");
    println!("{greeting_en}"); // Hello, Alice!

    // Formatters - number per locale.
    for tag in ["en-US", "de-DE", "fr-FR"] {
        let lang: LanguageIdentifier = tag.parse().unwrap();
        let fmt = LocaleFormatter::new(lang);
        println!("{tag}: {}", fmt.format_number(12_345.67));
    }
}
