//! `gdl_version = "MAJOR.MINOR"`: which edition of GDL a file is written in.
//!
//! **A minor version only adds**: optional fields, constructors and spellings.
//! A file written for an older minor reads unchanged. A file written for a
//! newer minor is still evaluated, because most of it is likely understood;
//! when it fails, the failure says the file is newer than this reader. A
//! different major is refused before evaluation: it may remove, rename or
//! change the meaning of what this reader knows.
//!
//! Read from the tokens, before parsing. Starlark resolves names when it
//! compiles a module, so a file written for a newer GDL fails on an unknown
//! constructor before any call runs -- too early for `gear()` to see its own
//! `gdl_version` argument.

use gearbox_ir::{Diagnostic, DiagnosticCode, Location};
use starlark_syntax::codemap::{CodeMap, Pos, Span};
use starlark_syntax::lexer::{Lexer, Token};

use crate::declarative::{dialect, span_to_range};

/// The GDL this reader implements.
pub const CURRENT: Version = Version { major: 1, minor: 2 };

/// A GDL edition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

impl Version {
    /// `"1.0"`, and nothing else: `"1"` and `"v1.0"` are refused so that one
    /// spelling means one thing.
    #[must_use]
    pub fn parse(spelling: &str) -> Option<Self> {
        let (major, minor) = spelling.split_once('.')?;
        let number = |s: &str| {
            (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                .then(|| s.parse().ok())
                .flatten()
        };
        Some(Self {
            major: number(major)?,
            minor: number(minor)?,
        })
    }
}

/// The version a file declares, checked against [`CURRENT`].
///
/// `Ok(None)` when it declares none, which means `1.0`.
///
/// # Errors
///
/// GBX0123, to report instead of evaluating, when the value is not
/// `MAJOR.MINOR` or names a major this reader does not read.
pub fn declared(uri: &str, source: &str) -> Result<Option<Version>, Box<Diagnostic>> {
    let codemap = CodeMap::new(uri.to_owned(), source.to_owned());
    let tokens: Vec<(usize, Token, usize)> = Lexer::new(source, &dialect(), codemap.clone())
        .map_while(Result::ok)
        .collect();
    let location = |start: usize, end: usize| {
        Location::new(
            uri.to_owned(),
            span_to_range(
                &codemap,
                Span::new(
                    Pos::new(u32::try_from(start).unwrap_or(u32::MAX)),
                    Pos::new(u32::try_from(end).unwrap_or(u32::MAX)),
                ),
            ),
        )
    };
    let mut found: Option<(&str, usize, usize)> = None;
    for window in tokens.windows(3) {
        if !matches!((&window[0].1, &window[1].1),
            (Token::Identifier(name), Token::Equal) if name == "gdl_version")
        {
            continue;
        }
        let Token::String(value) = &window[2].1 else {
            return Err(Box::new(
                Diagnostic::error(
                    DiagnosticCode::GdlVersionUnsupported,
                    "GDL version must be a literal string",
                    format!("write a file header: `gdl_version = \"{CURRENT}\"`"),
                )
                .at(location(window[2].0, window[2].2)),
            ));
        };
        if found.is_some_and(|(previous, _, _)| previous != value) {
            return Err(Box::new(
                Diagnostic::error(
                    DiagnosticCode::GdlVersionUnsupported,
                    "Conflicting GDL version declarations",
                    "keep one file-level gdl_version; remove the legacy argument or make it agree",
                )
                .at(location(window[2].0, window[2].2)),
            ));
        }
        found = Some((value, window[2].0, window[2].2));
    }
    let Some((spelling, start, end)) = found else {
        return Ok(None);
    };
    let at = location(start, end);
    let Some(version) = Version::parse(spelling) else {
        return Err(Box::new(
            Diagnostic::error(
                DiagnosticCode::GdlVersionUnsupported,
                format!("`gdl_version = \"{spelling}\"` is not a GDL version"),
                format!("write major and minor, e.g. `gdl_version = \"{CURRENT}\"`"),
            )
            .at(at),
        ));
    };
    if version.major != CURRENT.major {
        return Err(Box::new(Diagnostic::error(
            DiagnosticCode::GdlVersionUnsupported,
            format!(
                "this file is written for GDL {version}; this Gearbox reads GDL {}.x",
                CURRENT.major
            ),
            if version.major > CURRENT.major {
                "update Gearbox to one that reads this major version"
            } else {
                "rewrite the file for the current GDL; a major version changes what a file means"
            },
        )
        .at(at)));
    }
    Ok(Some(version))
}

#[cfg(test)]
mod tests {
    use super::Version;

    #[test]
    fn a_version_is_major_dot_minor() {
        assert_eq!(Version::parse("1.0"), Some(Version { major: 1, minor: 0 }));
        assert_eq!(
            Version::parse("2.13"),
            Some(Version {
                major: 2,
                minor: 13
            })
        );
        for bad in ["1", "v1.0", "1.0.0", "1.", ".1", "1.x", "-1.0", "+1.0"] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
    }
}
