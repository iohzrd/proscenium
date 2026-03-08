use maud::{DOCTYPE, Markup, PreEscaped, html};

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
                            a href="/" { "Feed" }
                            a href="/trending" { "Trending" }
                            a href="/users" { "Users" }
                        }
                    }
                }
                main { (content) }
                footer {
                    div.app-cta {
                        p { "Join the conversation" }
                        a.btn href="https://github.com/iohzrd/iroh-social/releases" target="_blank" rel="noopener" {
                            "Get the App"
                        }
                    }
                    p.footer-note { "Powered by Iroh Social -- a peer-to-peer social network" }
                }
            }
        }
    }
}

pub fn format_time(timestamp_ms: i64) -> String {
    let secs = timestamp_ms / 1000;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let diff = now - secs;

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

pub fn short_pubkey(pubkey: &str) -> String {
    if pubkey.len() > 12 {
        format!("{}...{}", &pubkey[..6], &pubkey[pubkey.len() - 6..])
    } else {
        pubkey.to_string()
    }
}

pub fn render_content(content: &str) -> Markup {
    use regex::Regex;
    use std::sync::LazyLock;

    static URL_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"https?://[^\s<>"')\]]+"#).unwrap());
    static HASHTAG_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"#(\w+)").unwrap());

    let escaped = content
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");

    let with_links = URL_RE.replace_all(&escaped, |caps: &regex::Captures| {
        let url = &caps[0];
        format!(r#"<a href="{url}" target="_blank" rel="noopener">{url}</a>"#)
    });

    let with_tags = HASHTAG_RE.replace_all(&with_links, |caps: &regex::Captures| {
        let tag = &caps[1];
        format!(r#"<a href="/trending?tag={tag}">#{tag}</a>"#)
    });

    let with_newlines = with_tags.replace('\n', "<br>");

    html! { (PreEscaped(with_newlines)) }
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
