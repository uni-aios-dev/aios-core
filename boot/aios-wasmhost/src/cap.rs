//! Capability tokens for blocks.
//!
//! A block never receives ambient authority. The kernel builds a [`Token`] for
//! each `MODULE_BLOCK` from its configuration (for example
//! `blocks.empty.rights = log, clock` and `blocks.empty.fuel = 20000`) and the
//! [`crate::Host`] refuses a host import the token does not grant. The same
//! token bounds execution through `fuel`, so a runaway block is stopped by the
//! runtime instead of being killed by a watchdog.
//!
//! Rights are a small bit set with stable names, so a configuration file and a
//! serial log say the same thing.

use core::fmt;

/// A set of rights a block may hold.
///
/// The constants are the only source of truth: [`Rights::from_names`] parses the
/// names used in `*.conf`, and [`Rights::fmt`] prints them back in the same
/// spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rights(u32);

impl Rights {
    /// No rights at all: the block can compute, but cannot call the host.
    pub const NONE: Self = Self(0);
    /// `aios.log` — write a UTF-8 string from the block's linear memory to the
    /// kernel log.
    pub const LOG: Self = Self(1 << 0);
    /// `aios.exit` — terminate the block with a status code.
    pub const EXIT: Self = Self(1 << 1);
    /// `aios.now_ms` — read the monotonic millisecond clock.
    pub const CLOCK: Self = Self(1 << 2);
    /// `aios.yield` — give the CPU back between long computations.
    pub const YIELD: Self = Self(1 << 3);

    /// Every right the host knows about.
    pub const ALL: Self = Self(Self::LOG.0 | Self::EXIT.0 | Self::CLOCK.0 | Self::YIELD.0);

    /// `(right, name)` pairs, in a stable order and spelling.
    pub const NAMES: [(Self, &'static str); 4] = [
        (Self::LOG, "log"),
        (Self::EXIT, "exit"),
        (Self::CLOCK, "clock"),
        (Self::YIELD, "yield"),
    ];

    /// The raw bit set, for configuration and logs.
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Builds a set from raw bits, dropping anything outside [`Rights::ALL`].
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits & Self::ALL.0)
    }

    /// The union of two sets.
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether every right of `other` is present.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether no right is present.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Parses a comma or `|` separated list such as `log, clock`.
    ///
    /// Names are matched in any ASCII case, surrounding whitespace is ignored,
    /// an empty item is skipped and an unknown name makes the whole list
    /// `None`, so a typo in `blocks.*.rights` is a configuration error instead
    /// of a silently smaller capability set.
    pub fn from_names(text: &str) -> Option<Self> {
        let mut rights = Self::NONE;
        for item in text.split([',', '|']) {
            let item = item.trim();
            if item.is_empty() {
                continue;
            }
            let (right, _) = Self::NAMES
                .iter()
                .find(|(_, name)| name.eq_ignore_ascii_case(item))?;
            rights = rights.union(*right);
        }
        Some(rights)
    }
}

impl core::ops::BitOr for Rights {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        self.union(other)
    }
}

impl fmt::Display for Rights {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_empty() {
            return f.write_str("none");
        }
        let mut first = true;
        for (right, name) in Self::NAMES {
            if self.contains(right) {
                if !first {
                    f.write_str(",")?;
                }
                f.write_str(name)?;
                first = false;
            }
        }
        Ok(())
    }
}

/// The authority and budget the kernel grants to one block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    block: u32,
    rights: Rights,
    fuel: u32,
}

impl Token {
    /// A token for block `block` with exactly `rights` and `fuel` instructions.
    pub const fn new(block: u32, rights: Rights, fuel: u32) -> Self {
        Self {
            block,
            rights,
            fuel,
        }
    }

    /// A token that grants nothing and cannot execute a single instruction.
    pub const fn deny_all(block: u32) -> Self {
        Self::new(block, Rights::NONE, 0)
    }

    /// The block this token belongs to.
    pub const fn block(&self) -> u32 {
        self.block
    }

    /// The rights granted to the block.
    pub const fn rights(&self) -> Rights {
        self.rights
    }

    /// The instruction budget of one run.
    pub const fn fuel(&self) -> u32 {
        self.fuel
    }

    /// Whether every right of `rights` is granted.
    pub const fn allows(&self, rights: Rights) -> bool {
        self.rights.contains(rights)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;
    use alloc::string::ToString;

    #[test]
    fn names_round_trip() {
        let rights = Rights::from_names("log, CLOCK|exit").unwrap();
        assert_eq!(rights, Rights::LOG.union(Rights::CLOCK).union(Rights::EXIT));
        assert!(rights.contains(Rights::LOG));
        assert!(rights.contains(Rights::CLOCK | Rights::EXIT));
        assert!(!rights.contains(Rights::YIELD));
        assert_eq!(format!("{rights}"), "log,exit,clock");
        assert_eq!(Rights::ALL.to_string(), "log,exit,clock,yield");
        assert_eq!(Rights::NONE.to_string(), "none");
    }

    #[test]
    fn unknown_or_partial_names_are_errors() {
        assert_eq!(Rights::from_names("log,teleport"), None);
        assert_eq!(Rights::from_names(""), Some(Rights::NONE));
        assert_eq!(Rights::from_names(" , | "), Some(Rights::NONE));
        assert_eq!(Rights::from_names("LOG"), Some(Rights::LOG));
    }

    #[test]
    fn bits_are_masked_to_known_rights() {
        assert_eq!(Rights::from_bits(u32::MAX), Rights::ALL);
        assert_eq!(Rights::from_bits(0), Rights::NONE);
        assert!(Rights::NONE.is_empty());
        assert!(!Rights::ALL.is_empty());
    }

    #[test]
    fn token_gate() {
        let token = Token::new(7, Rights::LOG | Rights::CLOCK, 1000);
        assert_eq!(token.block(), 7);
        assert_eq!(token.fuel(), 1000);
        assert!(token.allows(Rights::LOG));
        assert!(token.allows(Rights::LOG | Rights::CLOCK));
        assert!(!token.allows(Rights::LOG | Rights::EXIT));
        assert!(!Token::deny_all(0).allows(Rights::LOG));
    }
}
