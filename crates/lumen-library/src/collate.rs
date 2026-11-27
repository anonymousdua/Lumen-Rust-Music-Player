//! Unicode-aware sorting (CLDR collation via ICU4X).
//!
//! CLDR's Spanish tailoring of the root order: Latin first (accents sort next
//! to their base letters, "Árbol" beside "arbol", and "ñ" is its own letter
//! after "n"), then Devanagari, then Malayalam, each in its own alphabetical
//! order. The Spanish tailoring only changes "ñ", so English, Hindi and
//! Malayalam sort exactly as in the root order. Digits sort by numeric value ("Track 2" before
//! "Track 10"), and leading punctuation is ignored ("(I Can't Get No)" sorts
//! under "I").
//!
//! Sort keys are stored in the database so `ORDER BY sort_title` is both fast
//! and correct. [`COLLATION_VERSION`] is saved with them; when it changes, the
//! keys are rebuilt.

use icu_collator::options::{AlternateHandling, CollatorOptions, Strength};
use icu_collator::preferences::CollationNumericOrdering;
use icu_collator::{Collator, CollatorBorrowed, CollatorPreferences};

/// Bump when the collator, its options or the ICU version change.
pub const COLLATION_VERSION: &str = "icu4x-2.4.0-es-numeric-shifted-v1";

pub struct Collation {
    collator: CollatorBorrowed<'static>,
}

impl Collation {
    pub fn new() -> Self {
        let mut prefs = match "es".parse::<icu_locale_core::Locale>() {
            Ok(locale) => CollatorPreferences::from(&locale),
            Err(_) => CollatorPreferences::default(),
        };
        prefs.numeric_ordering = Some(CollationNumericOrdering::True);
        let mut options = CollatorOptions::default();
        options.strength = Some(Strength::Tertiary);
        options.alternate_handling = Some(AlternateHandling::Shifted);
        let collator = match Collator::try_new(prefs, options) {
            Ok(c) => c,
            // Compiled data for the root locale is always present.
            Err(_) => Collator::try_new(CollatorPreferences::default(), CollatorOptions::default())
                .unwrap_or_else(|_| unreachable!("root collation data is compiled in")),
        };
        Self { collator }
    }

    /// Binary sort key; comparing keys bytewise equals comparing strings.
    pub fn key(&self, s: &str) -> Vec<u8> {
        let mut out = Vec::with_capacity(s.len() * 3);
        let _ = self.collator.write_sort_key_to(s.trim(), &mut out);
        out
    }

    pub fn compare(&self, a: &str, b: &str) -> std::cmp::Ordering {
        self.collator.compare(a.trim(), b.trim())
    }
}

impl Default for Collation {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(words: &[&str]) -> Vec<String> {
        let c = Collation::new();
        let mut v: Vec<(Vec<u8>, String)> =
            words.iter().map(|w| (c.key(w), w.to_string())).collect();
        v.sort();
        v.into_iter().map(|(_, w)| w).collect()
    }

    #[test]
    fn mixed_script_order() {
        let got = sorted(&[
            "Zebra",
            "നിലാവ്",
            "apple",
            "Éclair",
            "तुम ही हो",
            "eclair",
            "Árbol",
            "banana",
            "अजनबी",
            "അമ്മ",
        ]);
        assert_eq!(
            got,
            vec![
                "apple",
                "Árbol",
                "banana",
                "eclair",
                "Éclair",
                "Zebra",
                "अजनबी",
                "तुम ही हो",
                "അമ്മ",
                "നിലാവ്"
            ]
        );
    }

    #[test]
    fn spanish_and_numbers() {
        assert_eq!(
            sorted(&["Ñandú", "nube", "ozono", "Track 10", "Track 2"]),
            vec!["nube", "Ñandú", "ozono", "Track 2", "Track 10"]
        );
    }

    #[test]
    fn leading_punctuation_is_ignored() {
        assert_eq!(
            sorted(&["(I Can't Get No) Satisfaction", "Hello", "Jump"]),
            vec!["Hello", "(I Can't Get No) Satisfaction", "Jump"]
        );
    }

    #[test]
    fn keys_match_compare() {
        let c = Collation::new();
        let words = ["b", "A", "a", "á", "ब", "ക", "10", "9"];
        for a in words {
            for b in words {
                assert_eq!(c.key(a).cmp(&c.key(b)), c.compare(a, b), "{a} vs {b}");
            }
        }
    }
}
