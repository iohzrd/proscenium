//! Splitting of post text into URLs, hashtags and plain text, and construction of FTS5
//! queries from user input.

/// Maximum number of characters in a hashtag. A longer run after `#` is plain text.
pub const MAX_HASHTAG_CHARS: usize = 64;
/// Maximum number of distinct hashtags counted per post.
pub const MAX_HASHTAGS_PER_POST: usize = 10;
/// Maximum byte length of a search query.
pub const MAX_SEARCH_QUERY_LEN: usize = 256;
/// Maximum number of terms in a search query.
pub const MAX_SEARCH_TERMS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Token<'a> {
    Text(&'a str),
    /// A run starting with `http://` or `https://`.
    Url(&'a str),
    /// The characters after a `#`, without the `#`.
    Hashtag(&'a str),
}

fn is_tag_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn is_url_end(c: char) -> bool {
    c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'' | ')' | ']')
}

/// Byte length of the URL at the start of `s`, if `s` starts with one.
fn url_len(s: &str) -> Option<usize> {
    let scheme_len = if s.starts_with("https://") {
        8
    } else if s.starts_with("http://") {
        7
    } else {
        return None;
    };
    let rest = s.get(scheme_len..)?;
    let rest_len = rest.find(is_url_end).unwrap_or(rest.len());
    (rest_len > 0).then_some(scheme_len + rest_len)
}

/// Byte length of the hashtag name at the start of `s` (the text after `#`), if it is
/// 1 to `MAX_HASHTAG_CHARS` tag characters long and not all digits.
fn hashtag_len(s: &str) -> Option<usize> {
    let mut len = 0;
    let mut chars = 0;
    let mut all_digits = true;
    for c in s.chars() {
        if !is_tag_char(c) {
            break;
        }
        chars += 1;
        if chars > MAX_HASHTAG_CHARS {
            return None;
        }
        all_digits &= c.is_numeric();
        len += c.len_utf8();
    }
    (len > 0 && !all_digits).then_some(len)
}

/// Split `content` into text, URL and hashtag tokens. The concatenation of the tokens
/// (with `#` put back before each hashtag) equals `content`.
///
/// A `#` starts a hashtag only at the start of the text or after a character that is
/// not a tag character, and never inside a URL.
pub fn tokenize(content: &str) -> Vec<Token<'_>> {
    let mut tokens = Vec::new();
    let mut text_start = 0;
    let mut pos = 0;
    let mut prev: Option<char> = None;

    while let Some(c) = content.get(pos..).and_then(|rest| rest.chars().next()) {
        let rest = content.get(pos..).unwrap_or("");
        let matched = if c == 'h' {
            url_len(rest).map(|len| (len, Token::Url(rest.get(..len).unwrap_or(""))))
        } else if c == '#' && !prev.is_some_and(is_tag_char) {
            let after = rest.get(1..).unwrap_or("");
            hashtag_len(after).map(|len| (len + 1, Token::Hashtag(after.get(..len).unwrap_or(""))))
        } else {
            None
        };

        match matched {
            Some((len, token)) => {
                if text_start < pos {
                    tokens.push(Token::Text(content.get(text_start..pos).unwrap_or("")));
                }
                tokens.push(token);
                pos += len;
                text_start = pos;
                prev = content.get(..pos).and_then(|s| s.chars().next_back());
            }
            None => {
                pos += c.len_utf8();
                prev = Some(c);
            }
        }
    }
    if text_start < content.len() {
        tokens.push(Token::Text(content.get(text_start..).unwrap_or("")));
    }
    tokens
}

/// The distinct hashtags of one post, lowercased, in order of first occurrence.
/// At most `MAX_HASHTAGS_PER_POST` are returned.
pub fn extract_hashtags(content: &str) -> Vec<String> {
    let mut tags: Vec<String> = Vec::new();
    for token in tokenize(content) {
        if let Token::Hashtag(tag) = token {
            let tag = tag.to_lowercase();
            if !tags.contains(&tag) {
                tags.push(tag);
                if tags.len() == MAX_HASHTAGS_PER_POST {
                    break;
                }
            }
        }
    }
    tags
}

/// Lowercase `tag` and return it if it is a valid hashtag name (without `#`).
pub fn normalize_hashtag(tag: &str) -> Option<String> {
    let tag = tag.strip_prefix('#').unwrap_or(tag);
    (hashtag_len(tag) == Some(tag.len())).then(|| tag.to_lowercase())
}

/// Convert user input into an FTS5 query: each whitespace-separated term becomes one
/// quoted phrase, and all phrases must match. Returns `None` if the input is longer than
/// `MAX_SEARCH_QUERY_LEN`, has more than `MAX_SEARCH_TERMS` terms, or has no term that
/// contains a letter or digit.
///
/// Inside a quoted FTS5 string only `"` is special and is escaped by doubling, so no
/// input character is interpreted as an FTS5 operator.
pub fn fts_query(input: &str) -> Option<String> {
    if input.len() > MAX_SEARCH_QUERY_LEN {
        return None;
    }
    let mut query = String::new();
    let mut terms = 0;
    for term in input.split(|c: char| c.is_whitespace() || c.is_control()) {
        // A term without a letter or digit yields no token and would form an empty phrase.
        if !term.chars().any(char::is_alphanumeric) {
            continue;
        }
        terms += 1;
        if terms > MAX_SEARCH_TERMS {
            return None;
        }
        if !query.is_empty() {
            query.push(' ');
        }
        query.push('"');
        for c in term.chars() {
            if c == '"' {
                query.push('"');
            }
            query.push(c);
        }
        query.push('"');
    }
    (!query.is_empty()).then_some(query)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_splits_urls_hashtags_and_text() {
        let tokens = tokenize("see https://example.com/page#section and #rust!");
        assert_eq!(
            tokens,
            vec![
                Token::Text("see "),
                Token::Url("https://example.com/page#section"),
                Token::Text(" and "),
                Token::Hashtag("rust"),
                Token::Text("!"),
            ]
        );
    }

    #[test]
    fn tokenize_reassembles_to_input() {
        let inputs = [
            "",
            "#",
            "##a",
            "a#b #c",
            "http://",
            "https://x)y",
            "h#t http://a.b/#c<d",
            "\u{65e5}\u{672c} #\u{8a9e}\u{10348} https://\u{4f8b}.jp",
            "&#39; <b>&amp;</b>",
        ];
        for input in inputs {
            let mut out = String::new();
            for token in tokenize(input) {
                match token {
                    Token::Text(t) | Token::Url(t) => out.push_str(t),
                    Token::Hashtag(t) => {
                        out.push('#');
                        out.push_str(t);
                    }
                }
            }
            assert_eq!(out, input);
        }
    }

    #[test]
    fn hashtag_requires_boundary_and_bounded_length() {
        assert_eq!(tokenize("a#b"), vec![Token::Text("a#b")]);
        assert_eq!(
            tokenize("(#b)"),
            vec![Token::Text("("), Token::Hashtag("b"), Token::Text(")")]
        );
        let long = format!("#{}", "x".repeat(MAX_HASHTAG_CHARS + 1));
        assert_eq!(tokenize(&long), vec![Token::Text(long.as_str())]);
        let max = format!("#{}", "x".repeat(MAX_HASHTAG_CHARS));
        assert_eq!(tokenize(&max).len(), 1);
        assert!(matches!(tokenize(&max).first(), Some(Token::Hashtag(_))));
    }

    #[test]
    fn extract_hashtags_deduplicates_and_caps() {
        assert_eq!(extract_hashtags("#Rust #rust #RUST"), vec!["rust"]);
        let many: String = (0..50).map(|i| format!("#t{i} ")).collect();
        assert_eq!(extract_hashtags(&many).len(), MAX_HASHTAGS_PER_POST);
        // A URL fragment is not a hashtag.
        assert!(extract_hashtags("https://example.com/#frag").is_empty());
    }

    #[test]
    fn normalize_hashtag_accepts_only_tag_names() {
        assert_eq!(normalize_hashtag("Rust"), Some("rust".to_string()));
        assert_eq!(normalize_hashtag("#Rust_1"), Some("rust_1".to_string()));
        assert_eq!(normalize_hashtag(""), None);
        assert_eq!(normalize_hashtag("a b"), None);
        assert_eq!(normalize_hashtag("a\"b"), None);
        assert_eq!(normalize_hashtag("2024"), None);
    }

    #[test]
    fn digits_after_hash_are_not_a_hashtag() {
        assert_eq!(
            tokenize("&#39; #1 #2nd"),
            vec![Token::Text("&#39; #1 "), Token::Hashtag("2nd"),]
        );
    }

    #[test]
    fn fts_query_quotes_every_term() {
        assert_eq!(
            fts_query("hello world").as_deref(),
            Some("\"hello\" \"world\"")
        );
        assert_eq!(fts_query("c++").as_deref(), Some("\"c++\""));
        assert_eq!(fts_query("don't").as_deref(), Some("\"don't\""));
        assert_eq!(fts_query("#rust").as_deref(), Some("\"#rust\""));
        assert_eq!(fts_query("\"abc").as_deref(), Some("\"\"\"abc\""));
        assert_eq!(fts_query("a AND b").as_deref(), Some("\"a\" \"AND\" \"b\""));
        assert_eq!(fts_query("x NEAR(y)").as_deref(), Some("\"x\" \"NEAR(y)\""));
    }

    #[test]
    fn fts_query_rejects_input_without_terms_or_over_limits() {
        assert_eq!(fts_query(""), None);
        assert_eq!(fts_query("   "), None);
        assert_eq!(fts_query("++ -- \"\""), None);
        assert_eq!(fts_query(&"a".repeat(MAX_SEARCH_QUERY_LEN + 1)), None);
        let many = vec!["a"; MAX_SEARCH_TERMS + 1].join(" ");
        assert_eq!(fts_query(&many), None);
        let max = vec!["a"; MAX_SEARCH_TERMS].join(" ");
        assert!(fts_query(&max).is_some());
    }
}
