use crate::content::{Token, tokenize};
use maud::{DOCTYPE, Markup, PreEscaped, html};
use proscenium_types::truncate_chars;

pub fn page(title: &str, server_name: &str, content: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (title) " - " (server_name) }
                style { (PreEscaped(CSS)) }
            }
            body {
                header {
                    nav {
                        a.logo href="/" { (server_name) }
                        div.nav-links {
                            a href="/" { "Arc" }
                            a href="/trending" { "Trending" }
                            a href="/users" { "Users" }
                        }
                    }
                }
                main { (content) }
                footer {
                    div.app-cta {
                        p { "Join the conversation" }
                        a.btn href="https://github.com/iohzrd/proscenium/releases" target="_blank" rel="noopener" {
                            "Get the App"
                        }
                    }
                    p.footer-note { "Powered by Proscenium -- a peer-to-peer social network" }
                }
            }
        }
    }
}

pub fn format_time(timestamp_ms: i64) -> String {
    let now_ms = i64::try_from(proscenium_types::now_millis()).unwrap_or(i64::MAX);
    let diff = now_ms.saturating_sub(timestamp_ms) / 1000;

    if diff < 60 {
        "just now".to_string()
    } else if diff < 3600 {
        let m = diff / 60;
        format!("{m}m ago")
    } else if diff < 86400 {
        let h = diff / 3600;
        format!("{h}h ago")
    } else if diff < 604800 {
        let d = diff / 86400;
        format!("{d}d ago")
    } else {
        let weeks = diff / 604800;
        format!("{weeks}w ago")
    }
}

/// First 6 and last 6 characters of `pubkey` joined by `...`; the whole string if it
/// has at most 12 characters.
pub fn short_pubkey(pubkey: &str) -> String {
    if pubkey.chars().count() <= 12 {
        return pubkey.to_string();
    }
    let head = truncate_chars(pubkey, 6);
    let tail_start = pubkey
        .char_indices()
        .rev()
        .nth(5)
        .map_or(0, |(index, _)| index);
    let tail = pubkey.get(tail_start..).unwrap_or("");
    format!("{head}...{tail}")
}

/// Render post text: each URL and hashtag token becomes a link, all other text is
/// emitted as text. Every token is escaped by maud when it is written.
pub fn render_content(content: &str) -> Markup {
    html! {
        @for token in tokenize(content) {
            @match token {
                Token::Text(text) => (text),
                Token::Url(url) => {
                    a href=(url) target="_blank" rel="noopener noreferrer" { (url) }
                }
                Token::Hashtag(tag) => {
                    a href=(tag_href(tag)) { "#" (tag) }
                }
            }
        }
    }
}

/// Path of the page that lists posts with the hashtag `tag`.
pub fn tag_href(tag: &str) -> String {
    format!("/trending?tag={}", encode_query_value(tag))
}

/// Percent-encode `value` for use as a URL query value: every byte except ASCII
/// letters, digits, `-`, `.`, `_` and `~` is encoded.
fn encode_query_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

const CSS: &str = r#"
*, *::before, *::after { box-sizing: border-box; margin: 0; padding: 0; }

:root {
    --bg: #0a0a0f;
    --bg-card: #12121a;
    --bg-hover: #1a1a25;
    --border: #222233;
    --text: #e0e0e8;
    --text-dim: #888899;
    --text-faint: #555566;
    --accent: #6c8cff;
    --accent-hover: #8ca4ff;
    --radius: 12px;
    --max-w: 640px;
}

body {
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
    background: var(--bg);
    color: var(--text);
    line-height: 1.5;
    min-height: 100vh;
    display: flex;
    flex-direction: column;
}

a { color: var(--accent); text-decoration: none; }
a:hover { color: var(--accent-hover); }

header {
    border-bottom: 1px solid var(--border);
    position: sticky;
    top: 0;
    background: var(--bg);
    z-index: 100;
}

nav {
    max-width: var(--max-w);
    margin: 0 auto;
    padding: 0.75rem 1rem;
    display: flex;
    align-items: center;
    justify-content: space-between;
}

.logo {
    font-size: 1.15rem;
    font-weight: 700;
    color: var(--text);
}

.nav-links { display: flex; gap: 1.25rem; }
.nav-links a { color: var(--text-dim); font-size: 0.9rem; }
.nav-links a:hover { color: var(--text); }

main {
    max-width: var(--max-w);
    margin: 0 auto;
    padding: 0 1rem;
    width: 100%;
    flex: 1;
}

footer {
    border-top: 1px solid var(--border);
    text-align: center;
    padding: 2rem 1rem;
    margin-top: 2rem;
}

.app-cta { margin-bottom: 1rem; }
.app-cta p { color: var(--text-dim); margin-bottom: 0.5rem; }

.btn {
    display: inline-block;
    background: var(--accent);
    color: #fff;
    padding: 0.6rem 1.5rem;
    border-radius: 8px;
    font-weight: 600;
    font-size: 0.9rem;
    transition: background 0.15s;
}
.btn:hover { background: var(--accent-hover); color: #fff; }

.footer-note { color: var(--text-faint); font-size: 0.8rem; }

/* Post card */
.post {
    border-bottom: 1px solid var(--border);
    padding: 1rem 0;
}
.post:last-child { border-bottom: none; }
.post:hover { background: var(--bg-hover); margin: 0 -1rem; padding: 1rem; border-radius: var(--radius); }

.post-header {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    margin-bottom: 0.4rem;
}

.post-author {
    font-weight: 600;
    color: var(--text);
}

.post-pubkey {
    color: var(--text-faint);
    font-size: 0.8rem;
    font-family: monospace;
}

.post-time {
    color: var(--text-faint);
    font-size: 0.8rem;
    margin-left: auto;
}

.post-content {
    word-break: break-word;
    white-space: pre-wrap;
}

.post-content a { word-break: break-all; }

.post-meta {
    display: flex;
    gap: 1rem;
    margin-top: 0.5rem;
    color: var(--text-faint);
    font-size: 0.8rem;
}

.post-reply-context {
    color: var(--text-dim);
    font-size: 0.85rem;
    margin-bottom: 0.3rem;
}

.post-stats {
    display: flex;
    gap: 1.25rem;
    margin-top: 0.5rem;
    color: var(--text-dim);
    font-size: 0.8rem;
}
.post-stats .stat { color: var(--text-faint); }

/* Profile */
.profile-header {
    padding: 1.5rem 0;
    border-bottom: 1px solid var(--border);
    margin-bottom: 0.5rem;
}

.profile-name {
    font-size: 1.4rem;
    font-weight: 700;
}

.profile-pubkey {
    font-family: monospace;
    font-size: 0.8rem;
    color: var(--text-faint);
    word-break: break-all;
    margin-top: 0.25rem;
}

.profile-bio {
    margin-top: 0.5rem;
    color: var(--text-dim);
}

.profile-stats {
    display: flex;
    gap: 1.5rem;
    margin-top: 0.75rem;
    color: var(--text-dim);
    font-size: 0.9rem;
}
.profile-stats strong { color: var(--text); }

/* Trending */
.tag-list { list-style: none; }

.tag-item {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0.75rem 0;
    border-bottom: 1px solid var(--border);
}

.tag-name { font-size: 1.1rem; font-weight: 600; }
.tag-stats { color: var(--text-dim); font-size: 0.85rem; }

/* Users list */
.user-card {
    display: block;
    padding: 0.75rem 0;
    border-bottom: 1px solid var(--border);
    color: inherit;
}
.user-card:hover { background: var(--bg-hover); margin: 0 -1rem; padding: 0.75rem 1rem; border-radius: var(--radius); }
.user-name { font-weight: 600; }
.user-bio { color: var(--text-dim); font-size: 0.9rem; margin-top: 0.15rem; }
.user-meta { color: var(--text-faint); font-size: 0.8rem; margin-top: 0.15rem; }

.section-title {
    font-size: 1.2rem;
    font-weight: 700;
    padding: 1rem 0 0.5rem;
    border-bottom: 1px solid var(--border);
    margin-bottom: 0.5rem;
}

.empty {
    text-align: center;
    color: var(--text-dim);
    padding: 3rem 1rem;
}

.load-more {
    display: block;
    text-align: center;
    padding: 1rem;
    color: var(--accent);
    font-size: 0.9rem;
}

@media (max-width: 480px) {
    nav { padding: 0.5rem 0.75rem; }
    .nav-links { gap: 0.75rem; }
    main { padding: 0 0.75rem; }
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn render(content: &str) -> String {
        render_content(content).into_string()
    }

    #[test]
    fn url_with_fragment_is_one_link() {
        assert_eq!(
            render("https://example.com/page#section"),
            r#"<a href="https://example.com/page#section" target="_blank" rel="noopener noreferrer">https://example.com/page#section</a>"#
        );
    }

    #[test]
    fn text_is_escaped_and_entities_are_not_rewritten() {
        assert_eq!(
            render("<script>alert(1)</script> &#39; \"q\""),
            "&lt;script&gt;alert(1)&lt;/script&gt; &amp;#39; &quot;q&quot;"
        );
        assert_eq!(render("a & b"), "a &amp; b");
    }

    #[test]
    fn hashtag_links_are_percent_encoded() {
        assert_eq!(
            render("#rust and #\u{65e5}\u{672c}"),
            "<a href=\"/trending?tag=rust\">#rust</a> and \
             <a href=\"/trending?tag=%E6%97%A5%E6%9C%AC\">#\u{65e5}\u{672c}</a>"
        );
    }

    #[test]
    fn url_ends_before_quote_and_markup_characters() {
        assert_eq!(
            render("see http://a.test/x\"onmouseover=\"1"),
            "see <a href=\"http://a.test/x\" target=\"_blank\" rel=\"noopener noreferrer\">http://a.test/x</a>&quot;onmouseover=&quot;1"
        );
    }

    #[test]
    fn newlines_are_kept_as_text() {
        assert_eq!(render("a\nb"), "a\nb");
    }

    #[test]
    fn short_pubkey_cuts_on_char_boundaries() {
        assert_eq!(short_pubkey("abcdefghijkl"), "abcdefghijkl");
        assert_eq!(short_pubkey("abcdefghijklmnop"), "abcdef...klmnop");
        // 5 ASCII characters, one 4-byte character, 8 ASCII characters.
        assert_eq!(
            short_pubkey("aaaaa\u{10348}bbbbbbbb"),
            "aaaaa\u{10348}...bbbbbb"
        );
        assert_eq!(short_pubkey(""), "");
    }
}
