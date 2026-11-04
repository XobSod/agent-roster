//! A release's notes as they read in plain text, in the TUI's window and
//! from `roster update`: GitHub's Markdown, lightly taken apart.

/// A release's notes as `roster update` prints them: its heading, then its
/// lines, each at its indent, not wrapped (the terminal does it).
pub fn said(version: &str, date: &str, markdown: &str) -> String {
    let lines = plain(markdown)
        .into_iter()
        .map(|l| format!("{}{}", " ".repeat(l.lead), l.text));
    std::iter::once(heading(version, date))
        .chain(lines)
        .collect::<Vec<_>>()
        .join("\n")
}

/// A release's heading: `roster 0.2.0 · 2026-11-20`.
pub fn heading(version: &str, date: &str) -> String {
    match (text_only(version), text_only(date)) {
        (version, date) if date.is_empty() => format!("roster {version}"),
        (version, date) => format!("roster {version} · {date}"),
    }
}

/// `text` with nothing a terminal would take for more than text (an escape,
/// a carriage return): a tab as spaces, any other control left out.
fn text_only(text: &str) -> String {
    (text.replace('\t', "    ").chars())
        .filter(|c| !c.is_control())
        .collect()
}

/// A line of notes as it reads in plain text: indented by `lead`, and what
/// it wraps to by `indent` (under a list item's text); code as it's written,
/// cut rather than wrapped.
#[derive(Debug, PartialEq)]
pub struct Plain {
    pub text: String,
    pub lead: usize,
    pub indent: usize,
    pub code: bool,
}

/// Markdown as it reads in plain text, a line for each of its lines (a line
/// break in GitHub's notes is one): headings, list items and quotes by
/// their text, emphasis and code marks left out, a link by its text; no
/// blank lines at either end, nor two in a row.
pub fn plain(markdown: &str) -> Vec<Plain> {
    let mut lines: Vec<Plain> = Vec::new();
    let mut code = false;
    for line in markdown.lines() {
        let line = text_only(line);
        let line = line.trim_end();
        let text = line.trim_start();
        let lead = line.len() - text.len();
        let plain = |text: String, lead: usize, indent: usize| Plain {
            text,
            lead,
            indent,
            code: false,
        };
        if text.starts_with("```") || text.starts_with("~~~") {
            code = !code;
            continue;
        }
        let said = if code {
            Plain {
                text: format!("  {line}"),
                lead: 0,
                indent: 0,
                code: true,
            }
        } else if let Some(title) = heading_text(text) {
            plain(inline(title), 0, 0)
        } else if text.len() >= 3 && text.chars().all(|c| matches!(c, '-' | '*' | '_')) {
            plain(String::new(), 0, 0)
        } else if let Some(item) = ["- ", "* ", "+ "].iter().find_map(|m| text.strip_prefix(m)) {
            plain(format!("• {}", inline(item)), lead, lead + 2)
        } else if let Some(quote) = text.strip_prefix('>') {
            plain(format!("│ {}", inline(quote.trim_start())), lead, lead + 2)
        } else {
            plain(inline(text), lead, lead)
        };
        let blank = said.text.trim().is_empty();
        let after_blank = lines.last().is_none_or(|l| l.text.trim().is_empty());
        if !(blank && after_blank) || said.code {
            lines.push(said);
        }
    }
    while lines
        .last()
        .is_some_and(|l| l.text.trim().is_empty() && !l.code)
    {
        lines.pop();
    }
    lines
}

/// A heading's text (`## Fixes`, `### Fixes ###`), if `line` is one.
fn heading_text(line: &str) -> Option<&str> {
    let hashes = line.len() - line.trim_start_matches('#').len();
    let rest = &line[hashes..];
    if !(1..=6).contains(&hashes) || !(rest.is_empty() || rest.starts_with(' ')) {
        return None;
    }
    Some(rest.trim().trim_end_matches('#').trim_end())
}

/// A line's Markdown as its text: `**`, `__` and backticks left out, a link
/// or an image by its text.
fn inline(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        if let Some(after) = rest.strip_prefix("**").or_else(|| rest.strip_prefix("__")) {
            rest = after;
        } else if c == '`' {
            rest = &rest[1..];
        } else if let Some((label, after)) = link(rest) {
            out.push_str(&inline(label));
            rest = after;
        } else {
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    out
}

/// `[label](url)` or `![label](url)` at the start of `text`: its label, and
/// what follows it.
fn link(text: &str) -> Option<(&str, &str)> {
    let inner = text.strip_prefix('!').unwrap_or(text).strip_prefix('[')?;
    let close = inner.find("](")?;
    let after = &inner[close + 2..];
    let end = after.find(')')?;
    Some((&inner[..close], &after[end + 1..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(markdown: &str) -> Vec<String> {
        plain(markdown).into_iter().map(|l| l.text).collect()
    }

    #[test]
    fn notes_read_as_plain_text() {
        let notes = "## What's new\n\n\n- **Faster** start, see [#12](https://example.invalid/12)\n  - `roster update` installs it\n> Mind the `config.toml`\n\n---\nRead ![the docs](https://example.invalid/d.png).\n\n```sh\nroster  update\n```\n\n";
        assert_eq!(
            texts(notes),
            [
                "What's new",
                "",
                "• Faster start, see #12",
                "• roster update installs it",
                "│ Mind the config.toml",
                "",
                "Read the docs.",
                "",
                "  roster  update",
            ]
        );
        let lines = plain(notes);
        // A nested item under its parent's text, and wrapped under its own.
        assert_eq!((lines[3].lead, lines[3].indent), (2, 4));
        assert!(lines[8].code);
        // Not headings, links or emphasis after all.
        assert_eq!(
            texts("#hashtag\n[not](a link\nsnake_case 2*3"),
            ["#hashtag", "[not](a link", "snake_case 2*3"]
        );
    }

    #[test]
    fn notes_are_said_under_their_heading() {
        assert_eq!(
            said("0.2.0", "2026-11-20", "## Fixes\r\n- one\n  - two"),
            "roster 0.2.0 · 2026-11-20\nFixes\n• one\n  • two"
        );
        assert_eq!(said("0.2.0", "", ""), "roster 0.2.0");
        // Nothing for the terminal but text.
        assert_eq!(
            said(
                "0.2.0",
                "2026\x1b[2J",
                "\x1b]0;title\x07- red: \x1b[31mx\x1b[0m\r\tdone"
            ),
            "roster 0.2.0 · 2026[2J\n]0;title- red: [31mx[0m    done"
        );
    }
}
