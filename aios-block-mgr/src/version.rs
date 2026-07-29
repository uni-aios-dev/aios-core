use std::cmp::Ordering;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionParseError {
    InvalidFormat(String),
}

impl fmt::Display for VersionParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VersionParseError::InvalidFormat(s) => write!(f, "Invalid version format: '{}'", s),
        }
    }
}

impl std::error::Error for VersionParseError {}

impl SemanticVersion {
    pub fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    pub fn parse(version: &str) -> Result<Self, VersionParseError> {
        let v = version.trim_start_matches('v');
        let parts: Vec<&str> = v.split('.').collect();
        if parts.len() != 3 {
            return Err(VersionParseError::InvalidFormat(version.to_string()));
        }
        let major = parts[0]
            .parse::<u32>()
            .map_err(|_| VersionParseError::InvalidFormat(version.to_string()))?;
        let minor = parts[1]
            .parse::<u32>()
            .map_err(|_| VersionParseError::InvalidFormat(version.to_string()))?;
        let patch = parts[2]
            .parse::<u32>()
            .map_err(|_| VersionParseError::InvalidFormat(version.to_string()))?;
        Ok(Self {
            major,
            minor,
            patch,
        })
    }

    pub fn is_compatible_with(&self, other: &SemanticVersion) -> bool {
        self.major == other.major && self.minor >= other.minor
    }

    pub fn is_newer_than(&self, other: &SemanticVersion) -> bool {
        self > other
    }

    pub fn bump_major(&mut self) {
        self.major += 1;
        self.minor = 0;
        self.patch = 0;
    }

    pub fn bump_minor(&mut self) {
        self.minor += 1;
        self.patch = 0;
    }

    pub fn bump_patch(&mut self) {
        self.patch += 1;
    }
}

impl Ord for SemanticVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        self.major
            .cmp(&other.major)
            .then(self.minor.cmp(&other.minor))
            .then(self.patch.cmp(&other.patch))
    }
}

impl PartialOrd for SemanticVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for SemanticVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_valid() {
        let v = SemanticVersion::parse("1.2.3").unwrap();
        assert_eq!(v, SemanticVersion::new(1, 2, 3));
    }

    #[test]
    fn test_parse_with_v_prefix() {
        let v = SemanticVersion::parse("v2.0.1").unwrap();
        assert_eq!(v, SemanticVersion::new(2, 0, 1));
    }

    #[test]
    fn test_parse_invalid() {
        assert!(SemanticVersion::parse("1.2").is_err());
        assert!(SemanticVersion::parse("abc").is_err());
        assert!(SemanticVersion::parse("1.2.3.4").is_err());
    }

    #[test]
    fn test_ordering() {
        assert!(
            SemanticVersion::parse("1.0.0").unwrap() < SemanticVersion::parse("1.0.1").unwrap()
        );
        assert!(
            SemanticVersion::parse("1.0.0").unwrap() < SemanticVersion::parse("1.1.0").unwrap()
        );
        assert!(
            SemanticVersion::parse("1.0.0").unwrap() < SemanticVersion::parse("2.0.0").unwrap()
        );
        assert!(
            SemanticVersion::parse("1.2.3").unwrap() == SemanticVersion::parse("1.2.3").unwrap()
        );
    }

    #[test]
    fn test_compatibility() {
        let base = SemanticVersion::new(1, 0, 0);
        assert!(SemanticVersion::new(1, 1, 0).is_compatible_with(&base));
        assert!(SemanticVersion::new(1, 0, 5).is_compatible_with(&base));
        assert!(!SemanticVersion::new(2, 0, 0).is_compatible_with(&base));
    }

    #[test]
    fn test_bump() {
        let mut v = SemanticVersion::new(1, 2, 3);
        v.bump_patch();
        assert_eq!(v, SemanticVersion::new(1, 2, 4));
        v.bump_minor();
        assert_eq!(v, SemanticVersion::new(1, 3, 0));
        v.bump_major();
        assert_eq!(v, SemanticVersion::new(2, 0, 0));
    }

    #[test]
    fn test_display() {
        let v = SemanticVersion::new(1, 2, 3);
        assert_eq!(format!("{}", v), "1.2.3");
    }

    #[test]
    fn test_is_newer_than() {
        let v1 = SemanticVersion::new(1, 0, 0);
        let v2 = SemanticVersion::new(1, 1, 0);
        assert!(v2.is_newer_than(&v1));
        assert!(!v1.is_newer_than(&v2));
    }
}
