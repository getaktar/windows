//! Copied text, as an upload: a link to download, or text saved as a file
//! (the Mac app's docs/capture-and-instant-links.md, "Clipboard text and
//! links"). The rules are the Mac app's `ClipboardText`, so the same text
//! becomes the same kind of file on every platform.

use url::Url;

/// Longer text isn't taken for a link.
pub const MAX_LINK_LENGTH: usize = 8 * 1024;
/// Bigger text isn't parsed to tell whether it's JSON.
pub const MAX_JSON_CHECK_BYTES: usize = 8 * 1024 * 1024;
/// Only this many lines are read for Markdown marks.
const MAX_MARKDOWN_LINES: usize = 5_000;

/// The http(s) link `text` is, when all of it (trimmed) is one link: one
/// word with a host, nothing else around it.
pub fn link_in(text: &str) -> Option<Url> {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.chars().count() > MAX_LINK_LENGTH || trimmed.chars().any(char::is_whitespace) {
        return None;
    }
    let url = Url::parse(trimmed).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    url.host_str().filter(|host| !host.is_empty())?;
    Some(url)
}

/// What a copied text is saved as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextKind {
    Plain,
    Markdown,
    Json,
}

impl TextKind {
    pub fn extension(self) -> &'static str {
        match self {
            TextKind::Plain => "txt",
            TextKind::Markdown => "md",
            TextKind::Json => "json",
        }
    }
}

/// JSON when the whole text is an object or an array, Markdown when it has
/// enough of its marks (a heading, a code block, a link, a table, a
/// list...), plain text otherwise.
pub fn kind_of(text: &str) -> TextKind {
    if is_json(text) {
        TextKind::Json
    } else if looks_like_markdown(text) {
        TextKind::Markdown
    } else {
        TextKind::Plain
    }
}

pub fn is_json(text: &str) -> bool {
    let trimmed = text.trim();
    if !(trimmed.starts_with('{') || trimmed.starts_with('[')) || trimmed.len() > MAX_JSON_CHECK_BYTES {
        return false;
    }
    matches!(serde_json::from_str::<serde_json::Value>(trimmed), Ok(serde_json::Value::Object(_) | serde_json::Value::Array(_)))
}

/// A fenced code block alone is enough; otherwise marks add up, and it
/// takes two (a heading, a link, a table, or two list items).
pub fn looks_like_markdown(text: &str) -> bool {
    let mut score = 0;
    for line in text.split('\n').take(MAX_MARKDOWN_LINES) {
        let trimmed = line.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            return true;
        }
        if trimmed.starts_with('#') {
            let hashes = trimmed.chars().take_while(|c| *c == '#').count();
            if hashes <= 6 && trimmed[hashes..].starts_with(' ') {
                score += 2;
            }
        }
        if trimmed.starts_with("- [ ] ") || trimmed.to_lowercase().starts_with("- [x] ") {
            score += 2;
        } else if trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("+ ") || is_ordered_list_item(trimmed) {
            score += 1;
        }
        if trimmed.starts_with("> ") {
            score += 1;
        }
        if trimmed.starts_with('|') && trimmed.contains("---") {
            score += 2;
        }
        if score >= 2 {
            return true;
        }
    }
    if has_markdown_link(text) {
        score += 2;
    }
    if has_enclosed(text, "**", |c| c != '*' && c != '\n') {
        score += 1;
    }
    if has_enclosed(text, "`", |c| c != '`' && c != '\n') {
        score += 1;
    }
    score >= 2
}

/// "1. " to "999. " (or "1) ").
fn is_ordered_list_item(line: &str) -> bool {
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    if !(1..=3).contains(&digits) {
        return false;
    }
    let rest = &line[digits..];
    rest.starts_with(". ") || rest.starts_with(") ")
}

/// `[text](url)` with an http(s), absolute, anchor or ./ address, as the
/// Mac app's `\[[^\]\n]+\]\((https?://|/|#|\./)[^)\s]*\)`.
fn has_markdown_link(text: &str) -> bool {
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        let label = after.chars().take_while(|c| *c != ']' && *c != '\n').map(char::len_utf8).sum::<usize>();
        if label > 0 && after[label..].starts_with("](") {
            let target = &after[label + 2..];
            let prefix = ["https://", "http://", "/", "#", "./"].into_iter().find(|prefix| target.starts_with(prefix));
            if let Some(prefix) = prefix {
                let address = &target[prefix.len()..];
                let length = address.chars().take_while(|c| *c != ')' && !c.is_whitespace()).map(char::len_utf8).sum::<usize>();
                if address[length..].starts_with(')') {
                    return true;
                }
            }
        }
        rest = after;
    }
    false
}

/// `mark`, then one or more characters `inside` allows, then `mark` again
/// (`**bold**`, `` `code` ``).
fn has_enclosed(text: &str, mark: &str, inside: impl Fn(char) -> bool) -> bool {
    let mut rest = text;
    while let Some(start) = rest.find(mark) {
        let after = &rest[start + mark.len()..];
        let length = after.chars().take_while(|c| inside(*c)).map(char::len_utf8).sum::<usize>();
        if length > 0 && after[length..].starts_with(mark) {
            return true;
        }
        rest = after;
    }
    false
}

/// Whether `text` is worth uploading: not empty or only spaces.
pub fn has_content(text: &str) -> bool {
    !text.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_a_whole_link_only() {
        assert_eq!(link_in("  https://example.com/a.png \n").map(String::from), Some("https://example.com/a.png".to_string()));
        assert!(link_in("http://localhost:8080/x").is_some());
        assert!(link_in("see https://example.com/a.png").is_none());
        assert!(link_in("https://example.com/a b.png").is_none());
        assert!(link_in("ftp://example.com/a.png").is_none());
        assert!(link_in("file:///C:/a.png").is_none());
        assert!(link_in("https://").is_none());
        assert!(link_in("example.com/a.png").is_none());
        assert!(link_in("").is_none());
        let long = format!("https://example.com/{}", "a".repeat(MAX_LINK_LENGTH));
        assert!(link_in(&long).is_none());
    }

    #[test]
    fn tells_json_markdown_and_plain_text_apart() {
        assert_eq!(kind_of("{\"a\": 1}"), TextKind::Json);
        assert_eq!(kind_of(" [1, 2, 3] \n"), TextKind::Json);
        assert_eq!(kind_of("{not json"), TextKind::Plain);
        assert_eq!(kind_of("\"just a string\""), TextKind::Plain);
        assert_eq!(kind_of("```swift\nlet a = 1\n```"), TextKind::Markdown);
        assert_eq!(kind_of("# Title\n\nSome text"), TextKind::Markdown);
        assert_eq!(kind_of("#hashtag and more"), TextKind::Plain);
        assert_eq!(kind_of("- one\n- two"), TextKind::Markdown);
        assert_eq!(kind_of("- one item"), TextKind::Plain);
        assert_eq!(kind_of("- [ ] todo"), TextKind::Markdown);
        assert_eq!(kind_of("1. first\n2. second"), TextKind::Markdown);
        assert_eq!(kind_of("| a | b |\n|---|---|"), TextKind::Markdown);
        assert_eq!(kind_of("See [the docs](https://example.com/docs)."), TextKind::Markdown);
        assert_eq!(kind_of("A [bracket] (not a link)"), TextKind::Plain);
        assert_eq!(kind_of("Use **bold** and `code` here"), TextKind::Markdown);
        assert_eq!(kind_of("Only **bold** here"), TextKind::Plain);
        assert_eq!(kind_of("password123"), TextKind::Plain);
        // Windows line endings change nothing.
        assert_eq!(kind_of("# Title\r\n\r\ntext"), TextKind::Markdown);
        assert_eq!(kind_of("- one\r\n- two\r\n"), TextKind::Markdown);
    }

    #[test]
    fn empty_text_has_no_content() {
        assert!(!has_content(" \n\t "));
        assert!(has_content(" x "));
    }

    #[test]
    fn extensions() {
        assert_eq!(TextKind::Plain.extension(), "txt");
        assert_eq!(TextKind::Markdown.extension(), "md");
        assert_eq!(TextKind::Json.extension(), "json");
    }
}
