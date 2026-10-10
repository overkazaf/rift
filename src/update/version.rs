//! Minimal semantic-version parsing and ordering (`v0.4.1`, `0.5.0-rc.1`).

use std::cmp::Ordering;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// Pre-release identifiers (`rc.1` -> ["rc", "1"]); empty for a release.
    pub pre: Vec<String>,
}

impl Version {
    /// Parse `1.2.3`, `v1.2.3`, `1.2` (patch 0), `1.2.3-beta.2`, `1.2.3+build`.
    pub fn parse(s: &str) -> Option<Version> {
        let s = s.trim();
        let s = s.strip_prefix('v').or_else(|| s.strip_prefix('V')).unwrap_or(s);
        let s = s.split('+').next()?; // build metadata never affects ordering
        let (core, pre) = match s.split_once('-') {
            Some((c, p)) => (c, Some(p)),
            None => (s, None),
        };
        let mut nums = core.split('.');
        let num = |p: Option<&str>| -> Option<u64> {
            let p = p?;
            if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            p.parse().ok()
        };
        let major = num(nums.next())?;
        let minor = num(nums.next())?;
        let patch = match nums.next() {
            Some(p) => num(Some(p))?,
            None => 0,
        };
        if nums.next().is_some() {
            return None;
        }
        let pre = match pre {
            Some(p) if p.is_empty() || p.split('.').any(str::is_empty) => return None,
            Some(p) => p.split('.').map(str::to_string).collect(),
            None => Vec::new(),
        };
        Some(Version { major, minor, patch, pre })
    }

    /// The running build's version.
    pub fn current() -> Version {
        Version::parse(crate::config::VERSION).unwrap_or(Version { major: 0, minor: 0, patch: 0, pre: Vec::new() })
    }

    pub fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.pre.is_empty() {
            write!(f, "-{}", self.pre.join("."))?;
        }
        Ok(())
    }
}

fn cmp_ident(a: &str, b: &str) -> Ordering {
    let num = |s: &str| (!s.is_empty() && s.bytes().all(|c| c.is_ascii_digit())).then(|| s.parse::<u64>().ok()).flatten();
    match (num(a), num(b)) {
        (Some(x), Some(y)) => x.cmp(&y),
        (Some(_), None) => Ordering::Less, // numeric identifiers sort before alphanumeric
        (None, Some(_)) => Ordering::Greater,
        (None, None) => a.cmp(b),
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater, // 1.0.0 > 1.0.0-rc.1
                (false, true) => Ordering::Less,
                (false, false) => {
                    for (a, b) in self.pre.iter().zip(&other.pre) {
                        let o = cmp_ident(a, b);
                        if o != Ordering::Equal {
                            return o;
                        }
                    }
                    self.pre.len().cmp(&other.pre.len())
                }
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap_or_else(|| panic!("parse {s}"))
    }

    #[test]
    fn parses_common_forms() {
        assert_eq!(v("0.4.0"), Version { major: 0, minor: 4, patch: 0, pre: vec![] });
        assert_eq!(v("v1.2.3"), v("1.2.3"));
        assert_eq!(v("1.2"), v("1.2.0"));
        assert_eq!(v("1.2.3+build.5"), v("1.2.3"));
        assert_eq!(v("1.0.0-rc.1").pre, vec!["rc", "1"]);
        assert_eq!(v("v0.5.0-beta.2").to_string(), "0.5.0-beta.2");
        for bad in ["", "v", "1", "1.x.0", "1.2.3.4", "1.2.3-", "1.2.3-a..b", "latest", "-1.0.0"] {
            assert!(Version::parse(bad).is_none(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn orders_like_semver() {
        let chain = [
            "0.3.9", "0.4.0-alpha", "0.4.0-alpha.1", "0.4.0-alpha.beta", "0.4.0-beta", "0.4.0-beta.2",
            "0.4.0-beta.11", "0.4.0-rc.1", "0.4.0", "0.4.1", "0.10.0", "1.0.0",
        ];
        for w in chain.windows(2) {
            assert!(v(w[0]) < v(w[1]), "{} < {}", w[0], w[1]);
        }
        assert_eq!(v("v0.4.0").cmp(&v("0.4.0")), Ordering::Equal);
    }

    #[test]
    fn current_version_parses() {
        assert_eq!(Version::current().to_string(), crate::config::VERSION);
    }
}
