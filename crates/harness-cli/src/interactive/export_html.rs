//! `/export session.html`: the conversation as one self-contained page.
//!
//! prime-agent exports a single HTML file with everything inlined
//! (`core/export-html`). This page is the same kind of file - inline CSS, no
//! script and nothing fetched from the network - drawn from the conversation the
//! next turn would send the model: each message the user wrote, each answer, and
//! the tool calls a turn made. Every piece of text is escaped, and the session's
//! secrets are replaced before anything is written.

use std::fmt::Write as _;

use harness_providers::{MessageRole, ProviderMessage};

const STYLE: &str = "body{font:15px/1.55 system-ui,-apple-system,Segoe UI,sans-serif;max-width:52rem;margin:2rem auto;padding:0 1rem;color:#1f2328;background:#fff}\
header{border-bottom:1px solid #d0d7de;margin-bottom:1.5rem}h1{font-size:1.4rem;margin:0 0 .3rem}\
.meta{color:#59636e;font-size:.85rem;margin-bottom:1rem}\
.msg{margin:1rem 0;padding:.75rem 1rem;border-radius:8px}\
.user{background:#f0f4ff;border-left:3px solid #7c6faf}.assistant{background:#f6f8fa}\
.tool{background:#fafafa;border:1px dashed #d0d7de;font-size:.9rem}\
.role{font-size:.75rem;text-transform:uppercase;letter-spacing:.05em;color:#59636e;margin-bottom:.3rem}\
pre{background:#0d0d10;color:#e6e6e6;padding:.75rem;border-radius:6px;overflow-x:auto}\
code{font-family:ui-monospace,Consolas,monospace;font-size:.88em}p code{background:#eaeef2;padding:.1em .3em;border-radius:4px}\
details summary{cursor:pointer;color:#59636e}\
@media (prefers-color-scheme:dark){body{background:#0d1117;color:#e6edf3}.user{background:#1a1f35}.assistant{background:#161b22}\
.tool{background:#11151c;border-color:#30363d}p code{background:#30363d}header{border-color:#30363d}}";

/// Escape text for HTML.
fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            other => escaped.push(other),
        }
    }
    escaped
}

/// `code` spans of one escaped line.
fn inline(line: &str) -> String {
    let escaped = escape(line);
    let mut out = String::new();
    let mut open = false;
    for (index, part) in escaped.split('`').enumerate() {
        if index > 0 {
            out.push_str(if open { "</code>" } else { "<code>" });
            open = !open;
        }
        out.push_str(part);
    }
    if open {
        // An unclosed backtick is text, not the start of code.
        out = escaped;
    }
    out
}

/// Markdown the model writes, as HTML: fenced code blocks and paragraphs with
/// inline code. Anything else stays text.
fn markdown(text: &str) -> String {
    let mut html = String::new();
    let mut paragraph: Vec<String> = Vec::new();
    let mut fence: Option<(String, Vec<String>)> = None;
    let flush = |paragraph: &mut Vec<String>, html: &mut String| {
        if !paragraph.is_empty() {
            let _ = write!(html, "<p>{}</p>", paragraph.join("<br>"));
            paragraph.clear();
        }
    };
    for line in text.lines() {
        if let Some(label) = line.trim_start().strip_prefix("```") {
            if let Some((language, lines)) = fence.take() {
                let class = if language.is_empty() {
                    String::new()
                } else {
                    format!(" class=\"language-{}\"", escape(&language))
                };
                let _ = write!(
                    html,
                    "<pre><code{class}>{}</code></pre>",
                    escape(&lines.join("\n"))
                );
            } else {
                flush(&mut paragraph, &mut html);
                let language = label.split_whitespace().next().unwrap_or_default();
                fence = Some((language.to_owned(), Vec::new()));
            }
            continue;
        }
        if let Some((_, lines)) = fence.as_mut() {
            lines.push(line.to_owned());
        } else if line.trim().is_empty() {
            flush(&mut paragraph, &mut html);
        } else {
            paragraph.push(inline(line));
        }
    }
    if let Some((_, lines)) = fence {
        let _ = write!(
            html,
            "<pre><code>{}</code></pre>",
            escape(&lines.join("\n"))
        );
    }
    flush(&mut paragraph, &mut html);
    html
}

/// Replace every secret in `text`.
fn redact(text: &str, secrets: &[String]) -> String {
    secrets
        .iter()
        .filter(|secret| !secret.is_empty())
        .fold(text.to_owned(), |text, secret| {
            text.replace(secret.as_str(), "[REDACTED]")
        })
}

/// The page for a conversation.
#[must_use]
pub fn render(
    title: &str,
    model: &str,
    summary: Option<&str>,
    messages: &[ProviderMessage],
    secrets: &[String],
) -> String {
    let mut body = String::new();
    if let Some(summary) = summary {
        let _ = write!(
            body,
            "<div class=\"msg tool\"><div class=\"role\">earlier turns, compacted</div>{}</div>",
            markdown(&redact(summary, secrets))
        );
    }
    for message in messages {
        let (class, role) = match message.role {
            MessageRole::User => ("user", "you"),
            MessageRole::Assistant => ("assistant", "agent"),
            MessageRole::Tool => ("tool", "tool result"),
            MessageRole::System => continue,
        };
        let text = redact(&message.content, secrets);
        let _ = write!(
            body,
            "<div class=\"msg {class}\"><div class=\"role\">{role}</div>"
        );
        if message.role == MessageRole::Tool {
            let _ = write!(
                body,
                "<details><summary>output</summary><pre><code>{}</code></pre></details>",
                escape(&text)
            );
        } else {
            body.push_str(&markdown(&text));
        }
        for call in &message.tool_calls {
            let _ = write!(
                body,
                "<details><summary>{}</summary><pre><code>{}</code></pre></details>",
                escape(&call.name),
                escape(&redact(&call.arguments, secrets))
            );
        }
        body.push_str("</div>");
    }
    format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{title}</title><style>{STYLE}</style></head><body><header><h1>{title}</h1><div class=\"meta\">Harness Agents session export · model {model}</div></header>{body}</body></html>\n",
        title = escape(title),
        model = escape(model),
    )
}

#[cfg(test)]
mod tests {
    use super::render;
    use harness_providers::{MessageRole, ProviderMessage, ProviderToolCall};

    #[test]
    fn q09_html_export_is_self_contained_and_escaped() {
        let messages = vec![
            ProviderMessage::new(MessageRole::User, "try <script>alert(1)</script> and `a<b`"),
            ProviderMessage::assistant_with_calls(
                "Done:\n\n```rust\nfn main() {}\n```",
                vec![ProviderToolCall::new(
                    "c1",
                    "read_file",
                    r#"{"path":"a.rs"}"#,
                )],
            ),
        ];
        let page = render("my <title>", "deepseek/x", None, &messages, &[]);
        assert!(page.starts_with("<!doctype html>"));
        assert!(!page.contains("<script"), "no script at all: {page}");
        assert!(!page.contains("src=\"http") && !page.contains("href=\"http"));
        assert!(page.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(page.contains("<code>a&lt;b</code>"));
        assert!(page.contains("<pre><code class=\"language-rust\">fn main() {}</code></pre>"));
        assert!(page.contains("<summary>read_file</summary>"));
        assert!(page.contains("my &lt;title&gt;"));
    }

    #[test]
    fn q09_html_export_redacts_secrets() {
        let messages = vec![ProviderMessage::new(
            MessageRole::User,
            "my key is sk-secret-123",
        )];
        let page = render(
            "t",
            "m",
            Some("summary mentions sk-secret-123"),
            &messages,
            &["sk-secret-123".to_owned()],
        );
        assert!(!page.contains("sk-secret-123"), "{page}");
        assert_eq!(page.matches("[REDACTED]").count(), 2);
    }
}
