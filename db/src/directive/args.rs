//! The `key=value` / positional argument list a directive may carry.
//!
//! `--migrate:tool("positional", name="value")` hands the text between the
//! parentheses to [`parse_args`], which produces an [`Args`] the directive's
//! [`Directive::parse`](super::Directive::parse) can read.
//!
//! The grammar is deliberately small — no nested lists, no nesting of
//! directives:
//!
//! ```text
//! args      := /* empty */ | arg ( "," arg )*
//! arg       := value | name "=" value
//! name      := ASCII identifier
//! value     := "'" chars "'" | '"' chars '"' | bare
//! ```
//!
//! A quoted value collapses doubled quotes (`'it''s'` → `it's`). A bare value is
//! any remaining run of characters, trimmed.

// DECISION: positional and named arguments may appear in either order, since
// `("a", b="c")` reads naturally and there is no reason to force all the
// positional ones first. What is not allowed is an empty slot (`a,,b`, a trailing
// comma) or the same name twice — both are reported, not ignored.

use std::collections::BTreeMap;

use crate::error::ArgError;
use crate::lex::Span;

/// One argument value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgValue {
    /// A quoted value, quotes removed and doubled quotes collapsed.
    String(String),
    /// An unquoted value, kept verbatim (trimmed).
    Bare(String),
}

impl ArgValue {
    /// The value as text, whichever way it was written.
    pub fn as_str(&self) -> &str {
        match self {
            Self::String(s) | Self::Bare(s) => s,
        }
    }

    /// Was the value written in quotes? A directive that needs a plain keyword
    /// can require `false` here and reject `"skip"` in favour of `skip`.
    pub fn is_quoted(&self) -> bool {
        matches!(self, Self::String(_))
    }
}

/// A parsed directive argument list.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Args {
    positional: Vec<ArgValue>,
    named: BTreeMap<String, ArgValue>,
}

impl Args {
    /// An argument list with nothing in it — what `--migrate:key` and
    /// `--migrate:key()` both produce.
    pub fn empty() -> Self {
        Self::default()
    }

    /// `true` when there are no positional and no named arguments.
    pub fn is_empty(&self) -> bool {
        self.positional.is_empty() && self.named.is_empty()
    }

    /// How many arguments were given, positional and named together.
    pub fn len(&self) -> usize {
        self.positional.len() + self.named.len()
    }

    /// The positional arguments, in the order written.
    pub fn positional(&self) -> &[ArgValue] {
        &self.positional
    }

    /// The positional argument at `index`.
    pub fn get(&self, index: usize) -> Option<&ArgValue> {
        self.positional.get(index)
    }

    /// The named argument `name`.
    pub fn named(&self, name: &str) -> Option<&ArgValue> {
        self.named.get(name)
    }

    /// The names given, in sorted order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.named.keys().map(String::as_str)
    }
}

/// Parse the text between a directive's parentheses.
///
/// `base` is the directive's span; each error is placed on the same line at the
/// column where the offending text starts, so a caller prints a precise
/// location.
///
/// The lexer already rejects an unclosed `(` on the directive line, so in the
/// normal pipeline this sees balanced text. It stays defensive because a
/// directive can also be built programmatically: unbalanced parens and
/// unterminated quotes are distinct errors here, not panics.
pub fn parse_args(text: &str, base: &Span) -> Result<Args, ArgError> {
    if text.trim().is_empty() {
        return Ok(Args::empty());
    }

    let mut args = Args::empty();
    for (offset, raw) in split_top_level(text, base)? {
        let (name, value) = parse_argument(raw, base, offset)?;
        match name {
            None => args.positional.push(value),
            Some(name) => {
                if args.named.contains_key(&name) {
                    return Err(ArgError::DuplicateName {
                        name,
                        span: at(base, offset),
                    });
                }
                args.named.insert(name, value);
            }
        }
    }
    Ok(args)
}

/// Split on commas that are not inside a quote, rejecting stray parens and an
/// unterminated quote while doing it.
///
/// Returns `(character offset, raw text)` per slot, so errors can point at the
/// slot rather than at the whole list.
fn split_top_level<'a>(text: &'a str, base: &Span) -> Result<Vec<(usize, &'a str)>, ArgError> {
    let mut slots = Vec::new();
    // Byte and character offset of the current slot's start. Offsets are carried
    // in characters because a `Span` column counts characters.
    let mut start_byte = 0usize;
    let mut start_chars = 0usize;
    let mut chars = 0usize;
    // The open quote and where it opened.
    let mut quote: Option<(char, usize)> = None;
    let mut iter = text.char_indices().peekable();

    while let Some((byte, c)) = iter.next() {
        match quote {
            Some((open, _)) if c == open => {
                // A doubled quote is one escaped quote and keeps the string open.
                if iter.peek().is_some_and(|(_, next)| *next == open) {
                    iter.next();
                    chars += 1;
                } else {
                    quote = None;
                }
            }
            Some(_) => {}
            None => match c {
                '\'' | '"' => quote = Some((c, chars)),
                ',' => {
                    slots.push((start_chars, &text[start_byte..byte]));
                    start_byte = byte + c.len_utf8();
                    start_chars = chars + 1;
                }
                '(' | ')' => {
                    return Err(ArgError::UnbalancedParen {
                        found: c,
                        span: at(base, chars),
                    });
                }
                _ => {}
            },
        }
        chars += 1;
    }

    if let Some((_, opened_at)) = quote {
        return Err(ArgError::UnterminatedString {
            span: at(base, opened_at),
        });
    }
    slots.push((start_chars, &text[start_byte..]));
    Ok(slots)
}

/// One `value` or `name=value` slot.
fn parse_argument(
    raw: &str,
    base: &Span,
    offset: usize,
) -> Result<(Option<String>, ArgValue), ArgError> {
    // Leading whitespace is not part of the token, so the error column has to
    // account for it.
    let leading = raw.chars().count() - raw.trim_start().chars().count();
    let token = raw.trim();
    if token.is_empty() {
        return Err(ArgError::EmptyArgument {
            span: at(base, offset + leading),
        });
    }

    match split_name_value(token) {
        Some((name, value, value_offset)) => {
            let name = name.trim();
            if name.is_empty() {
                return Err(ArgError::EmptyArgument {
                    span: at(base, offset + leading),
                });
            }
            if let Some(quote) = name.find(['\'', '"']) {
                return Err(ArgError::UnexpectedQuote {
                    found: name[quote..].chars().next().unwrap_or('"'),
                    span: at(base, offset + leading + name[..quote].chars().count()),
                });
            }
            let value_start = offset + leading + value_offset;
            if value.trim().is_empty() {
                return Err(ArgError::MissingValue {
                    name: name.to_string(),
                    span: at(base, value_start),
                });
            }
            let value = parse_value(value.trim(), base, value_start)?;
            Ok((Some(name.to_string()), value))
        }
        None => Ok((None, parse_value(token, base, offset + leading)?)),
    }
}

/// The `name`, the `value`, and the character offset the value starts at, for
/// `name=value` split on the first `=` outside a quoted value. `None` when the
/// slot is a bare value.
fn split_name_value(token: &str) -> Option<(&str, &str, usize)> {
    let mut quote: Option<char> = None;
    let mut iter = token.char_indices().peekable();

    while let Some((byte, c)) = iter.next() {
        match quote {
            Some(open) if c == open => {
                if iter.peek().is_some_and(|(_, next)| *next == open) {
                    iter.next();
                } else {
                    quote = None;
                }
            }
            Some(_) => {}
            None => match c {
                '=' => {
                    return Some((
                        &token[..byte],
                        &token[byte + c.len_utf8()..],
                        token[..byte + c.len_utf8()].chars().count(),
                    ));
                }
                '\'' | '"' => quote = Some(c),
                _ => {}
            },
        }
    }
    None
}

/// A single value: quoted or bare.
fn parse_value(text: &str, base: &Span, offset: usize) -> Result<ArgValue, ArgError> {
    let Some(first) = text.chars().next() else {
        return Err(ArgError::EmptyArgument {
            span: at(base, offset),
        });
    };
    if first != '\'' && first != '"' {
        // Bare: any run of characters, but a quote appearing inside it means the
        // value is not what it looks like.
        if let Some(quote) = text.find(['\'', '"']) {
            return Err(ArgError::UnexpectedQuote {
                found: text[quote..].chars().next().unwrap_or('"'),
                span: at(base, offset + text[..quote].chars().count()),
            });
        }
        return Ok(ArgValue::Bare(text.to_string()));
    }

    let quote_len = first.len_utf8();
    let mut out = String::new();
    let mut rest = &text[quote_len..];

    loop {
        let Some(close) = rest.find(first) else {
            return Err(ArgError::UnterminatedString {
                span: at(base, offset),
            });
        };
        let after = &rest[close + quote_len..];
        if after.starts_with(first) {
            // A doubled quote is one literal quote; keep scanning past the pair.
            out.push_str(&rest[..close]);
            out.push(first);
            rest = &after[quote_len..];
            continue;
        }
        out.push_str(&rest[..close]);
        let trailing = after.trim();
        if trailing.is_empty() {
            return Ok(ArgValue::String(out));
        }
        // Something followed the closing quote, so the value did not end where
        // it looked like it did.
        let prefix = &text[..text.len() - after.len() + (after.len() - trailing.len())];
        return Err(ArgError::UnexpectedQuote {
            found: trailing.chars().next().unwrap_or(' '),
            span: at(base, offset + prefix.chars().count()),
        });
    }
}

/// `base` moved `extra` characters along the same line.
///
/// Directive argument lists never span lines — the lexer reads a directive body
/// up to the newline — so adjusting the column is enough.
fn at(base: &Span, extra_chars: usize) -> Span {
    Span::new(base.file.clone(), base.line, base.col + extra_chars)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Span {
        Span::new("m.sql", 4, 12)
    }

    fn parsed(text: &str) -> Args {
        parse_args(text, &base()).unwrap_or_else(|e| panic!("{text:?} should parse, got {e:?}"))
    }

    fn failed(text: &str) -> ArgError {
        parse_args(text, &base())
            .err()
            .unwrap_or_else(|| panic!("{text:?} should fail"))
    }

    // ==== the cases named in PLAN.md, "Directive/registry-level tests" =====

    #[test]
    fn no_arguments() {
        assert!(parsed("").is_empty());
        assert_eq!(parsed("").len(), 0);
        // Whitespace only is still no arguments.
        assert!(parsed("   ").is_empty());
    }

    #[test]
    fn one_quoted_positional() {
        let args = parsed("\"a\"");
        assert_eq!(args.len(), 1);
        assert_eq!(args.get(0), Some(&ArgValue::String("a".to_string())));
        assert!(args.named("x").is_none());
    }

    #[test]
    fn one_named_argument() {
        let args = parsed("a=\"b\"");
        assert_eq!(args.named("a"), Some(&ArgValue::String("b".to_string())));
        assert!(args.positional().is_empty());
    }

    #[test]
    fn positional_and_named_mixed() {
        let args = parsed("\"a\", b=\"c\"");
        assert_eq!(args.get(0), Some(&ArgValue::String("a".to_string())));
        assert_eq!(args.named("b"), Some(&ArgValue::String("c".to_string())));
        assert_eq!(args.len(), 2);
    }

    #[test]
    fn unbalanced_paren_is_its_own_error() {
        let error = failed("a)b");
        match error {
            ArgError::UnbalancedParen { found, span } => {
                assert_eq!(found, ')');
                // Column 12 + 1 character along.
                assert_eq!((span.line, span.col), (4, 13));
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn unterminated_string_is_its_own_error() {
        let error = failed("\"a");
        assert!(
            matches!(error, ArgError::UnterminatedString { .. }),
            "got {error:?}"
        );
    }

    // ==== the rest of the grammar ==========================================

    #[test]
    fn bare_values_are_kept_verbatim() {
        let args = parsed("skip, 42");
        assert_eq!(
            args.positional(),
            &[
                ArgValue::Bare("skip".to_string()),
                ArgValue::Bare("42".to_string()),
            ]
        );
    }

    #[test]
    fn doubled_quotes_collapse_inside_a_value() {
        assert_eq!(
            parsed("'it''s'").get(0),
            Some(&ArgValue::String("it's".to_string()))
        );
        assert_eq!(
            parsed("'a''b','c'").positional(),
            &[
                ArgValue::String("a'b".to_string()),
                ArgValue::String("c".to_string()),
            ]
        );
    }

    #[test]
    fn a_comma_inside_quotes_does_not_split() {
        let args = parsed("\"a,b\", c");
        assert_eq!(args.get(0), Some(&ArgValue::String("a,b".to_string())));
        assert_eq!(args.get(1), Some(&ArgValue::Bare("c".to_string())));
    }

    #[test]
    fn an_equals_inside_quotes_is_part_of_the_value() {
        let args = parsed("\"a=b\"");
        assert_eq!(args.get(0), Some(&ArgValue::String("a=b".to_string())));
        assert!(args.named("a").is_none());
    }

    #[test]
    fn a_quote_inside_a_bare_value_is_an_error() {
        // Balanced quotes so the slot reaches the value parser at all — an
        // unbalanced one is caught earlier, as an unterminated string.
        let error = failed("a\"b\"");
        assert!(
            matches!(error, ArgError::UnexpectedQuote { .. }),
            "got {error:?}"
        );
    }

    #[test]
    fn text_after_a_closing_quote_is_an_error() {
        let error = failed("\"a\"b");
        assert!(
            matches!(error, ArgError::UnexpectedQuote { .. }),
            "got {error:?}"
        );
    }

    #[test]
    fn empty_slots_are_errors_not_skipped() {
        for text in [",", "a,", ",a", "a,,b"] {
            let error = failed(text);
            assert!(
                matches!(error, ArgError::EmptyArgument { .. }),
                "{text:?} gave {error:?}"
            );
        }
    }

    #[test]
    fn a_name_without_a_value_is_an_error() {
        let error = failed("a=");
        match error {
            ArgError::MissingValue { name, .. } => assert_eq!(name, "a"),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn a_value_without_a_name_is_an_error() {
        let error = failed("=b");
        assert!(
            matches!(error, ArgError::EmptyArgument { .. }),
            "got {error:?}"
        );
    }

    #[test]
    fn a_repeated_name_is_an_error() {
        let error = failed("a=1, a=2");
        match error {
            ArgError::DuplicateName { name, .. } => assert_eq!(name, "a"),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn errors_point_at_the_offending_slot() {
        // `a=1, ,` — the empty slot is the text between the two commas; its
        // trimmed start is 5 characters into the argument list.
        let error = failed("a=1, ,");
        let ArgError::EmptyArgument { span } = error else {
            panic!("got {error:?}");
        };
        assert_eq!((span.line, span.col), (4, 12 + 5));
    }

    #[test]
    fn quoted_and_bare_values_are_distinguishable() {
        let args = parsed("\"quoted\", bare");
        assert!(args.get(0).is_some_and(ArgValue::is_quoted));
        assert!(!args.get(1).is_some_and(ArgValue::is_quoted));
        assert_eq!(args.get(1).map(ArgValue::as_str), Some("bare"));
    }

    #[test]
    fn names_are_listed_in_sorted_order() {
        let args = parsed("z=1, a=2");
        assert_eq!(args.names().collect::<Vec<_>>(), vec!["a", "z"]);
    }
}
