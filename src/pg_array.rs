//! Parsing for postgres's array literal syntax, e.g. `{a,"b c",NULL}`.
//!
//! `array['a', 'b']` is grammar, so sqlparser gives it structure and `DataFusion` plans it
//! as a list. `'{a,b}'` is an ordinary string literal which postgres only turns into an
//! array when coercing it to an array type, and `DataFusion` has no equivalent, so the JSON
//! path operators parse it here. Modelled on postgres's `array_in`, restricted to one
//! dimension, since a JSON path is never nested.

/// Parse a one-dimensional postgres array literal into its elements.
///
/// An unquoted, case-insensitive `NULL` is `None`. Returns `None` for anything malformed,
/// including a nested array.
pub(crate) fn parse_array_literal(input: &str) -> Option<Vec<Option<String>>> {
    let inner = input.trim().strip_prefix('{')?.strip_suffix('}')?;
    if inner.trim().is_empty() {
        return Some(Vec::new());
    }

    let mut elements = Vec::new();
    let mut element = Element::default();
    let mut in_quotes = false;
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        match c {
            // a backslash escapes the next character, inside or outside quotes
            '\\' => element.push_literal(chars.next()?),
            '"' if in_quotes => {
                in_quotes = false;
                element.close_quote();
            }
            '"' => {
                in_quotes = true;
                element.open_quote()?;
            }
            ',' if !in_quotes => elements.push(element.finish().ok()?),
            '{' | '}' if !in_quotes => return None,
            _ => element.buf.push(c),
        }
    }
    if in_quotes {
        return None;
    }
    elements.push(element.finish().ok()?);
    Some(elements)
}

/// The element being accumulated.
#[derive(Default)]
struct Element {
    buf: String,
    /// The content of a completed quoted section, if any
    quoted: Option<String>,
    /// Whether any character was quoted or escaped, which makes the element a string
    /// unconditionally: `"NULL"` and `\NULL` are the four character string
    literal: bool,
}

impl Element {
    fn push_literal(&mut self, c: char) {
        self.literal = true;
        self.buf.push(c);
    }

    /// Only whitespace may precede an opening quote
    fn open_quote(&mut self) -> Option<()> {
        if self.quoted.is_some() || !self.buf.trim().is_empty() {
            return None;
        }
        self.buf.clear();
        self.literal = true;
        Some(())
    }

    fn close_quote(&mut self) {
        self.quoted = Some(std::mem::take(&mut self.buf));
    }

    /// Complete the element: `Err` if malformed, `Ok(None)` for `NULL`
    fn finish(&mut self) -> Result<Option<String>, ()> {
        let Element { buf, quoted, literal } = std::mem::take(self);
        if let Some(quoted) = quoted {
            // only whitespace may follow a closing quote
            return buf.trim().is_empty().then_some(Some(quoted)).ok_or(());
        }
        // whitespace around an unquoted element is not part of it
        let value = buf.trim();
        if value.is_empty() {
            // postgres rejects an empty unquoted element, e.g. `{a,}`
            Err(())
        } else if !literal && value.eq_ignore_ascii_case("null") {
            Ok(None)
        } else {
            Ok(Some(value.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_array_literal as parse;

    /// Parse, expecting success and no NULL elements.
    fn strs(input: &str) -> Vec<String> {
        parse(input)
            .unwrap_or_else(|| panic!("{input:?} should parse"))
            .into_iter()
            .map(|e| e.unwrap_or_else(|| panic!("{input:?} should have no NULL elements")))
            .collect()
    }

    #[test]
    fn simple() {
        assert_eq!(strs("{a}"), ["a"]);
        assert_eq!(strs("{a,b,c}"), ["a", "b", "c"]);
        assert_eq!(strs("{code.file.path}"), ["code.file.path"]);
        assert_eq!(strs("{a,-1}"), ["a", "-1"]);
    }

    #[test]
    fn empty() {
        assert_eq!(parse("{}"), Some(vec![]));
        assert_eq!(parse("{ }"), Some(vec![]));
    }

    #[test]
    fn whitespace() {
        assert_eq!(strs("  {a,b}  "), ["a", "b"]);
        assert_eq!(strs("{ a , b }"), ["a", "b"]);
        // interior whitespace is content
        assert_eq!(strs("{a b}"), ["a b"]);
    }

    #[test]
    fn quoted() {
        assert_eq!(strs(r#"{"a,b"}"#), ["a,b"]);
        assert_eq!(strs(r#"{"a,b",c}"#), ["a,b", "c"]);
        assert_eq!(strs(r#"{"  padded  "}"#), ["  padded  "]);
        assert_eq!(strs(r#"{ "a" , "b" }"#), ["a", "b"]);
        assert_eq!(strs(r#"{"{a}"}"#), ["{a}"]);
        assert_eq!(strs(r#"{""}"#), [""]);
    }

    #[test]
    fn escaped() {
        assert_eq!(strs(r"{a\,b}"), ["a,b"]);
        assert_eq!(strs(r#"{"a\"b"}"#), ["a\"b"]);
        assert_eq!(strs(r"{a\\b}"), [r"a\b"]);
        assert_eq!(strs(r"{\{a\}}"), ["{a}"]);
    }

    #[test]
    fn null() {
        assert_eq!(parse("{NULL}"), Some(vec![None]));
        assert_eq!(
            parse("{a,null,b}"),
            Some(vec![Some("a".into()), None, Some("b".into())])
        );
        // quoted or escaped, it is the string
        assert_eq!(strs(r#"{"NULL"}"#), ["NULL"]);
        assert_eq!(strs(r"{\NULL}"), ["NULL"]);
    }

    #[test]
    fn malformed() {
        assert_eq!(parse("a,b"), None);
        assert_eq!(parse("{a"), None);
        assert_eq!(parse("{a,}"), None);
        assert_eq!(parse("{,a}"), None);
        assert_eq!(parse(r#"{"a}"#), None);
        assert_eq!(parse(r"{a\}"), None);
        assert_eq!(parse(r#"{a"b"}"#), None);
        assert_eq!(parse(r#"{"a"b}"#), None);
        // nested arrays are not JSON paths
        assert_eq!(parse("{{a,b}}"), None);
        assert_eq!(parse("{a,{b}}"), None);
    }
}
