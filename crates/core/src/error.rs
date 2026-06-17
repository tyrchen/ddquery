//! Parse error type.
//!
//! A [`ParseError`] is intentionally small: a byte `offset` into the original
//! query plus a one-line, human-readable `reason`. It never carries the input
//! string itself, so it is cheap to clone and safe to log. The parser never
//! panics on hostile input — every failure path returns one of these.

use std::fmt;

use serde::{Deserialize, Serialize};

/// An error produced while parsing a monitor query.
///
/// Carries the byte `offset` at which parsing failed and a short `reason`.
/// Offsets are always on a UTF-8 character boundary of the original input.
///
/// # Examples
///
/// ```
/// use ddquery_core::parse;
///
/// let err = parse("avg(last_5m)").unwrap_err();
/// assert!(err.reason().contains("expected"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParseError {
    offset: usize,
    reason: String,
}

impl ParseError {
    /// Create a new error at `offset` with the given `reason`.
    #[must_use]
    pub fn new(offset: usize, reason: impl Into<String>) -> Self {
        Self {
            offset,
            reason: reason.into(),
        }
    }

    /// Byte offset into the original query where parsing failed.
    #[must_use]
    pub fn offset(&self) -> usize {
        self.offset
    }

    /// One-line, human-readable failure reason.
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "parse error at byte {}: {}", self.offset, self.reason)
    }
}

impl std::error::Error for ParseError {}
