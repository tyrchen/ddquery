//! A byte cursor over the query string.
//!
//! The parser is scannerless: rather than producing a flat token vector up
//! front (which would lose the context-sensitivity of metric names, postfix
//! modifiers, and the dual comma/space filter separators), it walks this
//! cursor directly. Byte offsets for [`ParseError`](crate::ParseError) fall
//! out naturally from the cursor's tracked position.
//!
//! The cursor works on `&str` and only ever splits on ASCII bytes it has
//! verified, so every reported offset lands on a UTF-8 character boundary.

use crate::error::ParseError;

/// A non-consuming, position-tracking view over the input.
#[derive(Debug, Clone)]
pub(crate) struct Cursor<'a> {
    input: &'a str,
    pos: usize,
}

impl<'a> Cursor<'a> {
    /// Create a cursor at the start of `input`.
    pub(crate) fn new(input: &'a str) -> Self {
        Self { input, pos: 0 }
    }

    /// The not-yet-consumed remainder.
    pub(crate) fn rest(&self) -> &'a str {
        &self.input[self.pos..]
    }

    /// Whether all input has been consumed (after trimming trailing space).
    pub(crate) fn is_done(&self) -> bool {
        self.rest().trim_start().is_empty()
    }

    /// Peek at the next non-whitespace character without consuming it.
    pub(crate) fn peek(&self) -> Option<char> {
        self.rest().trim_start().chars().next()
    }

    /// Build a [`ParseError`] at the current position.
    pub(crate) fn error(&self, reason: impl Into<String>) -> ParseError {
        ParseError::new(self.pos, reason)
    }

    /// Skip leading ASCII whitespace.
    pub(crate) fn skip_ws(&mut self) {
        let trimmed = self.rest().len() - self.rest().trim_start().len();
        self.pos += trimmed;
    }

    /// Consume `lit` if it is the next token (after whitespace). Returns
    /// `true` on success.
    pub(crate) fn eat(&mut self, lit: &str) -> bool {
        self.skip_ws();
        if self.rest().starts_with(lit) {
            self.pos += lit.len();
            true
        } else {
            false
        }
    }

    /// Consume `lit` or return an error naming what was expected.
    pub(crate) fn expect(&mut self, lit: &str) -> Result<(), ParseError> {
        if self.eat(lit) {
            Ok(())
        } else {
            Err(self.error(format!("expected `{lit}`")))
        }
    }

    /// Consume the longest leading run of characters matching `pred`
    /// (after whitespace). Returns the consumed slice, which may be empty.
    pub(crate) fn take_while(&mut self, pred: impl Fn(char) -> bool) -> &'a str {
        self.skip_ws();
        let rest = self.rest();
        let end = rest
            .char_indices()
            .find(|(_, c)| !pred(*c))
            .map_or(rest.len(), |(i, _)| i);
        let taken = &rest[..end];
        self.pos += end;
        taken
    }

    /// Parse a single- or double-quoted string literal, returning its inner
    /// contents with surrounding quotes removed and `\"`/`\\` unescaped.
    pub(crate) fn take_quoted(&mut self) -> Result<String, ParseError> {
        self.skip_ws();
        let rest = self.rest();
        let Some(quote @ ('"' | '\'')) = rest.chars().next() else {
            return Err(self.error("expected a quoted string"));
        };
        let mut out = String::new();
        let mut chars = rest.char_indices();
        chars.next(); // opening quote
        let mut escaped = false;
        for (i, c) in chars {
            if escaped {
                // Preserve the escape for any character other than the quote
                // and backslash, so embedded search syntax survives verbatim.
                match c {
                    '"' | '\'' | '\\' => out.push(c),
                    other => {
                        out.push('\\');
                        out.push(other);
                    }
                }
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == quote {
                self.pos += i + c.len_utf8();
                return Ok(out);
            } else {
                out.push(c);
            }
        }
        Err(self.error("unterminated quoted string"))
    }

    /// Parse a numeric literal (optional sign, integer/decimal, optional
    /// scientific notation).
    pub(crate) fn take_number(&mut self) -> Result<f64, ParseError> {
        self.skip_ws();
        let start = self.pos;
        let rest = self.rest();
        let mut end = 0;
        let bytes = rest.as_bytes();
        if matches!(bytes.first(), Some(b'+' | b'-')) {
            end += 1;
        }
        let mut seen_digit = false;
        let mut seen_dot = false;
        while end < bytes.len() {
            match bytes[end] {
                b'0'..=b'9' => {
                    seen_digit = true;
                    end += 1;
                }
                b'.' if !seen_dot => {
                    seen_dot = true;
                    end += 1;
                }
                b'e' | b'E' => {
                    end += 1;
                    if matches!(bytes.get(end), Some(b'+' | b'-')) {
                        end += 1;
                    }
                }
                _ => break,
            }
        }
        if !seen_digit {
            return Err(ParseError::new(start, "expected a number"));
        }
        let text = &rest[..end];
        let value: f64 = text
            .parse()
            .map_err(|_| ParseError::new(start, format!("invalid number `{text}`")))?;
        self.pos += end;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_eat_literal_after_whitespace() {
        let mut c = Cursor::new("   :rest");
        assert!(c.eat(":"));
        assert_eq!(c.rest(), "rest");
    }

    #[test]
    fn test_should_take_while_predicate() {
        let mut c = Cursor::new("abc123 .x");
        assert_eq!(c.take_while(|ch| ch.is_ascii_alphanumeric()), "abc123");
        assert_eq!(c.rest(), " .x");
    }

    #[test]
    fn test_should_unescape_quoted_string() {
        let mut c = Cursor::new(r#""a \"b\" c""#);
        assert_eq!(c.take_quoted().unwrap(), r#"a "b" c"#);
    }

    #[test]
    fn test_should_preserve_unknown_escapes_in_quoted_string() {
        let mut c = Cursor::new(r#""status:\w+""#);
        assert_eq!(c.take_quoted().unwrap(), r"status:\w+");
    }

    #[test]
    fn test_should_error_on_unterminated_string() {
        let mut c = Cursor::new("\"oops");
        assert!(c.take_quoted().is_err());
    }

    #[test]
    fn test_should_parse_numbers() {
        assert_eq!(Cursor::new("300").take_number().unwrap(), 300.0);
        assert_eq!(Cursor::new("-2.5").take_number().unwrap(), -2.5);
        assert_eq!(Cursor::new("1e3").take_number().unwrap(), 1000.0);
    }

    #[test]
    fn test_should_report_offset_on_error() {
        let mut c = Cursor::new("avg");
        c.take_while(|ch| ch.is_ascii_alphabetic());
        let err = c.expect("(").unwrap_err();
        assert_eq!(err.offset(), 3);
    }
}
