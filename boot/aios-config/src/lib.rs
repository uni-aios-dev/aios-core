//! Reader for AIOS configuration files (`/aios/config/*.conf`).
//!
//! The format is a flat list of `key = value` lines. `#` starts a comment, the
//! key and the value are trimmed and a value may be wrapped in `"` or `'`:
//!
//! ```text
//! # AIOS system configuration
//! system.name = AIOS
//! system.arch = x86_64
//! boot.debug = false
//! blocks.demo.fuel = 1000
//! ```
//!
//! Rules, shared by `aios-boot` and `aios-kernel`:
//!
//! * blank lines, comments and lines without `=` are skipped;
//! * a key must be non-empty and made of ASCII letters, digits, `.`, `_` or
//!   `-`; anything else in the key position skips the line;
//! * a value may be empty (`key =` clears it) but never contains `#`;
//! * a repeated key overrides the earlier value (last one wins).
//!
//! The crate is `no_std` and allocation-free: [`Config`] borrows the text and
//! re-parses it lazily, so the kernel can hold a configuration module straight
//! from the bootloader's memory without copying it.

#![no_std]

/// One `key = value` pair borrowed from the configuration text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry<'a> {
    /// The trimmed key, e.g. `blocks.demo.fuel`.
    pub key: &'a str,
    /// The trimmed, unquoted value, e.g. `100`.
    pub value: &'a str,
}

/// A borrowed view over one configuration text.
#[derive(Debug, Clone, Copy)]
pub struct Config<'a> {
    text: &'a str,
}

impl<'a> Config<'a> {
    /// Wraps a configuration text. Nothing is parsed until a value is asked
    /// for, so this cannot fail.
    pub const fn new(text: &'a str) -> Self {
        Self { text }
    }

    /// The text this view borrows.
    pub const fn text(&self) -> &'a str {
        self.text
    }

    /// Iterates over every accepted `key = value` pair in file order,
    /// including overridden duplicates.
    pub fn entries(&self) -> Entries<'a> {
        Entries {
            lines: self.text.lines(),
        }
    }

    /// The effective value of `key`, or `None` when it is not set.
    ///
    /// Duplicates are resolved with last-one-wins, matching the file order.
    pub fn get(&self, key: &str) -> Option<&'a str> {
        let mut found = None;
        for entry in self.entries() {
            if entry.key == key {
                found = Some(entry.value);
            }
        }
        found
    }

    /// Whether `key` is present, even with an empty value.
    pub fn contains(&self, key: &str) -> bool {
        self.entries().any(|entry| entry.key == key)
    }

    /// The value of `key` parsed as an unsigned 32-bit integer.
    ///
    /// Accepts decimal and `0x`-prefixed hexadecimal digits.
    pub fn get_u32(&self, key: &str) -> Option<u32> {
        parse_u32(self.get(key)?)
    }

    /// The value of `key` parsed as a boolean.
    ///
    /// `true`/`yes`/`on`/`1` and `false`/`no`/`off`/`0` are accepted in any
    /// ASCII case; anything else is `None` so a typo is visible in the log
    /// instead of silently becoming `false`.
    pub fn get_bool(&self, key: &str) -> Option<bool> {
        parse_bool(self.get(key)?)
    }

    /// Number of accepted `key = value` pairs, duplicates included.
    pub fn count(&self) -> usize {
        self.entries().count()
    }

    /// Keys that appear more than once, in first-seen order.
    ///
    /// The kernel logs these at boot: a drop-in that overrides a key is normal,
    /// but an accidental duplicate inside one file is usually a mistake.
    pub fn duplicates(&self) -> Duplicates<'a> {
        Duplicates {
            text: self.text,
            index: 0,
        }
    }
}

/// Iterator over the accepted entries of a [`Config`].
#[derive(Debug, Clone)]
pub struct Entries<'a> {
    lines: core::str::Lines<'a>,
}

impl<'a> Iterator for Entries<'a> {
    type Item = Entry<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        for line in self.lines.by_ref() {
            if let Some(entry) = parse_line(line) {
                return Some(entry);
            }
        }
        None
    }
}

/// Iterator over the keys that occur more than once.
#[derive(Debug, Clone)]
pub struct Duplicates<'a> {
    text: &'a str,
    index: usize,
}

impl<'a> Iterator for Duplicates<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let position = self.index;
            let entry = Config::new(self.text).entries().nth(position)?;
            self.index += 1;
            let earlier = Config::new(self.text)
                .entries()
                .take(position)
                .any(|previous| previous.key == entry.key);
            if earlier {
                return Some(entry.key);
            }
        }
    }
}

/// Parses a bare value as an unsigned 32-bit integer.
///
/// Accepts surrounding whitespace, decimal digits and a `0x` hexadecimal
/// prefix. No underscores, signs or unit suffixes: a value that the parser
/// does not understand must not silently become a number.
pub fn parse_u32(value: &str) -> Option<u32> {
    let value = value.trim();
    let (digits, radix) = match value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        Some(hex) => (hex, 16),
        None => (value, 10),
    };
    u32::from_str_radix(digits, radix).ok()
}

/// Parses a bare value as a boolean.
///
/// `true`/`yes`/`on`/`1` and `false`/`no`/`off`/`0` are accepted in any ASCII
/// case. Anything else (including an empty value) is `None`, so a typo is
/// visible in the log instead of silently becoming `false`.
pub fn parse_bool(value: &str) -> Option<bool> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("true")
        || value.eq_ignore_ascii_case("yes")
        || value.eq_ignore_ascii_case("on")
        || value == "1"
    {
        return Some(true);
    }
    if value.eq_ignore_ascii_case("false")
        || value.eq_ignore_ascii_case("no")
        || value.eq_ignore_ascii_case("off")
        || value == "0"
    {
        return Some(false);
    }
    None
}

/// Parses one line into an [`Entry`], skipping comments, blanks and lines that
/// do not follow the `key = value` shape.
fn parse_line(line: &str) -> Option<Entry<'_>> {
    let line = line.split('#').next().unwrap_or("").trim();
    if line.is_empty() {
        return None;
    }
    let (key, value) = line.split_once('=')?;
    let key = key.trim();
    if !is_key(key) {
        return None;
    }
    Some(Entry {
        key,
        value: unquote(value.trim()),
    })
}

/// Whether `key` is a syntactically valid configuration key.
fn is_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Strips one pair of matching surrounding quotes, if present.
fn unquote(value: &str) -> &str {
    for quote in ['"', '\''] {
        if value.len() >= 2 && value.starts_with(quote) && value.ends_with(quote) {
            return &value[1..value.len() - 1];
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# AIOS system configuration.
system.name = AIOS
system.arch = x86_64    # inline comment

boot.console = serial
boot.debug = false
";

    #[test]
    fn parses_values() {
        let config = Config::new(SAMPLE);
        assert_eq!(config.get("system.name"), Some("AIOS"));
        assert_eq!(config.get("system.arch"), Some("x86_64"));
        assert_eq!(config.get("boot.console"), Some("serial"));
        assert_eq!(config.get("missing.key"), None);
    }

    #[test]
    fn counts_entries() {
        let config = Config::new(SAMPLE);
        assert_eq!(config.count(), 4);
        assert_eq!(config.entries().count(), 4);
        assert!(config.contains("boot.debug"));
        assert!(!config.contains("boot.nope"));
    }

    #[test]
    fn last_value_wins() {
        let config = Config::new("boot.debug = true\nboot.debug = false\n");
        assert_eq!(config.get("boot.debug"), Some("false"));
        assert_eq!(config.count(), 2);
    }

    #[test]
    fn skips_malformed_lines() {
        let config = Config::new(
            "\
just a sentence
= no key
bad key = 1
a.b.c = 1
",
        );
        assert_eq!(config.count(), 1);
        assert_eq!(config.get("a.b.c"), Some("1"));
        assert_eq!(config.get("bad key"), None);
    }

    #[test]
    fn quotes_and_empty_values() {
        let config = Config::new("a = 'single'\nb = \"double\"\nc =\nd = a#b\n");
        assert_eq!(config.get("a"), Some("single"));
        assert_eq!(config.get("b"), Some("double"));
        assert_eq!(config.get("c"), Some(""));
        // `#` always starts a comment, so the value stops before it.
        assert_eq!(config.get("d"), Some("a"));
    }

    #[test]
    fn parses_numbers() {
        let config = Config::new("dec = 100\nhex = 0x1f\nbig = 4294967296\nbad = ten\nneg = -1\n");
        assert_eq!(config.get_u32("dec"), Some(100));
        assert_eq!(config.get_u32("hex"), Some(31));
        assert_eq!(config.get_u32("big"), None);
        assert_eq!(config.get_u32("bad"), None);
        assert_eq!(config.get_u32("neg"), None);
        assert_eq!(config.get_u32("absent"), None);
    }

    #[test]
    fn parses_booleans() {
        let config = Config::new(
            "a = true\nb = YES\nc = On\nd = 1\ne = false\nf = no\ng = off\nh = 0\ni = maybe\n",
        );
        assert_eq!(config.get_bool("a"), Some(true));
        assert_eq!(config.get_bool("b"), Some(true));
        assert_eq!(config.get_bool("c"), Some(true));
        assert_eq!(config.get_bool("d"), Some(true));
        assert_eq!(config.get_bool("e"), Some(false));
        assert_eq!(config.get_bool("f"), Some(false));
        assert_eq!(config.get_bool("g"), Some(false));
        assert_eq!(config.get_bool("h"), Some(false));
        assert_eq!(config.get_bool("i"), None);
    }

    #[test]
    fn reports_duplicate_keys() {
        let config = Config::new("a = 1\nb = 2\na = 3\n");
        assert_eq!(config.duplicates().next(), Some("a"));
        assert_eq!(config.duplicates().count(), 1);
        assert_eq!(Config::new("a = 1\nb = 2\n").duplicates().count(), 0);
    }

    #[test]
    fn empty_text_has_no_entries() {
        let config = Config::new("");
        assert_eq!(config.count(), 0);
        assert!(!config.contains("a"));
        assert_eq!(config.get("a"), None);
    }
}
