//! A page of HTML as text a model can read: what the page says, with its headings, lists
//! and links kept and its scripts, styles and markup gone. Pure, so it is tested natively.
//!
//! This is no browser. It reads tags as they come, closes nothing that was left open and
//! runs no script, which is enough for documentation, articles and reference pages; a page
//! that is built by its scripts has little text to find.

use std::borrow::Cow;
use std::fmt::Write as _;

use crate::url;

/// Elements whose contents are not the page's text.
const UNREAD: [&str; 6] = ["script", "style", "noscript", "template", "svg", "iframe"];
/// Elements that begin and end a paragraph of their own.
const PARAGRAPHS: [&str; 9] = [
    "p",
    "pre",
    "blockquote",
    "ul",
    "ol",
    "table",
    "section",
    "article",
    "hr",
];
/// Elements that begin and end a line of their own.
const LINES: [&str; 17] = [
    "div",
    "li",
    "tr",
    "br",
    "header",
    "footer",
    "nav",
    "main",
    "aside",
    "title",
    "dt",
    "dd",
    "form",
    "figure",
    "figcaption",
    "details",
    "summary",
];

/// The text being written.
#[derive(Default)]
struct Text {
    out: String,
    /// How many `<pre>` are open: inside one, spacing is kept as written.
    pre: usize,
    /// Whether a space is owed before the next word.
    space: bool,
    /// The links that are open: where each one's text begins, and its address.
    links: Vec<(usize, Option<String>)>,
}

impl Text {
    /// Add `text`, with runs of spacing made one space unless inside a `<pre>`.
    fn say(&mut self, text: &str) {
        let text = decode(text);
        if self.pre > 0 {
            self.out.push_str(&text);
            return;
        }
        for c in text.chars() {
            if c.is_whitespace() {
                self.space = true;
                continue;
            }
            if self.space && !self.out.is_empty() && !self.out.ends_with(['\n', ' ']) {
                self.out.push(' ');
            }
            self.space = false;
            self.out.push(c);
        }
    }

    /// End the line, if there is one.
    fn line(&mut self) {
        let written = self.out.trim_end_matches(' ').len();
        self.out.truncate(written);
        if !self.out.is_empty() && !self.out.ends_with('\n') {
            self.out.push('\n');
        }
        self.space = false;
    }

    /// End the paragraph: the line, and a blank one after it.
    fn paragraph(&mut self) {
        self.line();
        if !self.out.is_empty() && !self.out.ends_with("\n\n") {
            self.out.push('\n');
        }
    }

    /// Write the link that just closed as `[text](address)`.
    fn link(&mut self) {
        let Some((from, Some(address))) = self.links.pop() else {
            return;
        };
        let Some(said) = self.out.get(from..).map(str::to_owned) else {
            return;
        };
        let words = said.trim();
        if words.is_empty() || words == address {
            return;
        }
        if words.contains('\n') {
            // A link around a block: the address after it, on its last line.
            let written = self.out.trim_end().len();
            self.out.truncate(written);
            // Writing to a `String` cannot fail.
            let _ = writeln!(self.out, " ({address})");
            return;
        }
        let lead = said.len() - said.trim_start().len();
        self.out.truncate(from + lead);
        let _ = write!(self.out, "[{words}]({address})");
    }

    /// Take what the tag `name` means for the text: `closing` for `</name>`.
    fn tag(&mut self, name: &str, closing: bool, tag: &str, base: &str) {
        match name {
            "a" if closing => self.link(),
            "a" => {
                if self.space && !self.out.is_empty() && !self.out.ends_with(['\n', ' ']) {
                    self.out.push(' ');
                    self.space = false;
                }
                let address = attribute(tag, "href").and_then(|href| url::resolve(base, &href));
                self.links.push((self.out.len(), address));
            }
            "img" => {
                if let Some(alt) = attribute(tag, "alt").filter(|alt| !alt.trim().is_empty()) {
                    self.say(&format!(" [image: {}] ", alt.trim()));
                }
            }
            "pre" => {
                self.paragraph();
                self.pre = if closing {
                    self.pre.saturating_sub(1)
                } else {
                    self.pre + 1
                };
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.paragraph();
                if !closing {
                    let level = name.trim_start_matches('h').parse().unwrap_or(1);
                    self.out.push_str(&"#".repeat(level));
                    self.out.push(' ');
                }
            }
            "li" if !closing => {
                self.line();
                self.out.push_str("- ");
            }
            "td" | "th" if !closing => {
                if !self.out.is_empty() && !self.out.ends_with('\n') {
                    self.out.push_str(" | ");
                    self.space = false;
                }
            }
            "hr" => {
                self.paragraph();
                self.out.push_str("---");
                self.paragraph();
            }
            name if PARAGRAPHS.contains(&name) => self.paragraph(),
            name if LINES.contains(&name) => self.line(),
            _ => {}
        }
    }

    /// The text, with no more than one blank line in a row.
    fn finish(mut self) -> String {
        self.line();
        let mut tidy = String::with_capacity(self.out.len());
        let mut blank = 0;
        for line in self.out.lines() {
            let line = line.trim_end();
            blank = if line.is_empty() { blank + 1 } else { 0 };
            if blank < 2 {
                tidy.push_str(line);
                tidy.push('\n');
            }
        }
        tidy.trim().to_owned()
    }
}

/// The text of the page `html`, found at the address `base`, which its links are worked out
/// against.
pub fn to_text(html: &str, base: &str) -> String {
    let mut text = Text::default();
    let mut rest = html;
    while let Some(at) = rest.find('<') {
        let (before, tail) = rest.split_at(at);
        text.say(before);
        rest = read_tag(&mut text, tail, base);
    }
    text.say(rest);
    text.finish()
}

/// Read the tag `tail` begins with into `text`, and return what follows it.
fn read_tag<'a>(text: &mut Text, tail: &'a str, base: &str) -> &'a str {
    if let Some(comment) = tail.strip_prefix("<!--") {
        return comment.split_once("-->").map_or("", |(_, after)| after);
    }
    let inside = tail.get(1..).unwrap_or_default();
    let begins_a_tag = inside
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || "/!?".contains(c));
    let (Some(end), true) = (tag_end(tail), begins_a_tag) else {
        // A `<` that begins no tag, as in `a < b`.
        text.say("<");
        return inside;
    };
    let (tag, after) = tail.split_at(end + 1);
    let closing = inside.starts_with('/');
    let name: String = inside
        .trim_start_matches('/')
        .chars()
        .take_while(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_ascii_lowercase();
    if !closing && UNREAD.contains(&name.as_str()) {
        return after_closing(after, &name);
    }
    text.tag(&name, closing, tag, base);
    after
}

/// Where the tag `tail` begins with ends: its `>`, not counting one inside a quoted value.
fn tag_end(tail: &str) -> Option<usize> {
    let mut quote = None;
    for (at, c) in tail.char_indices() {
        match (quote, c) {
            (None, '>') => return Some(at),
            (None, '"' | '\'') => quote = Some(c),
            (Some(open), c) if c == open => quote = None,
            _ => {}
        }
    }
    None
}

/// What follows the `</name>` that closes an unread element, or nothing if it never closes.
fn after_closing<'a>(rest: &'a str, name: &str) -> &'a str {
    let closes = |candidate: &str| {
        candidate
            .get(2..2 + name.len())
            .is_some_and(|found| found.eq_ignore_ascii_case(name))
    };
    rest.match_indices("</")
        .filter_map(|(at, _)| rest.get(at..))
        .find(|candidate| closes(candidate))
        .and_then(|candidate| candidate.split_once('>'))
        .map_or("", |(_, after)| after)
}

/// The value of the attribute `wanted` in `tag`, with its character references read.
fn attribute(tag: &str, wanted: &str) -> Option<String> {
    // Past the tag's name.
    let mut rest = tag
        .trim_start_matches('<')
        .trim_start_matches(|c: char| !c.is_whitespace() && c != '>');
    loop {
        rest = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '/');
        if rest.is_empty() || rest.starts_with('>') {
            return None;
        }
        let name_end = rest
            .find(|c: char| c.is_whitespace() || "=>/".contains(c))
            .unwrap_or(rest.len());
        let (name, after) = rest.split_at(name_end);
        let after = after.trim_start();
        let Some(value) = after.strip_prefix('=') else {
            rest = after;
            continue;
        };
        let value = value.trim_start();
        let (found, after) = if let Some(quote @ ('"' | '\'')) = value.chars().next() {
            let inner = value.get(1..).unwrap_or_default();
            inner.split_once(quote).unwrap_or((inner, ""))
        } else {
            let end = value
                .find(|c: char| c.is_whitespace() || c == '>')
                .unwrap_or(value.len());
            value.split_at(end)
        };
        if name.eq_ignore_ascii_case(wanted) {
            return Some(decode(found).into_owned());
        }
        rest = after;
    }
}

/// `text` with its character references (`&amp;`, `&#8212;`, `&#x2014;`) read.
fn decode(text: &str) -> Cow<'_, str> {
    if !text.contains('&') {
        return Cow::Borrowed(text);
    }
    let mut decoded = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        let (before, tail) = rest.split_at(at);
        decoded.push_str(before);
        let reference = tail
            .get(1..)
            .and_then(|after| after.split_once(';'))
            .filter(|(name, _)| name.len() <= 10)
            .and_then(|(name, after)| Some((character(name)?, after)));
        if let Some((c, after)) = reference {
            decoded.push(c);
            rest = after;
        } else {
            decoded.push('&');
            rest = tail.get(1..).unwrap_or_default();
        }
    }
    decoded.push_str(rest);
    Cow::Owned(decoded)
}

/// The character the reference `name` stands for, as in `&name;`.
fn character(name: &str) -> Option<char> {
    if let Some(number) = name.strip_prefix('#') {
        let code = match number.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => number.parse().ok()?,
        };
        return char::from_u32(code);
    }
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => ' ',
        "mdash" => '—',
        "ndash" => '–',
        "hellip" => '…',
        "lsquo" => '‘',
        "rsquo" => '’',
        "ldquo" => '“',
        "rdquo" => '”',
        "laquo" => '«',
        "raquo" => '»',
        "middot" => '·',
        "bull" => '•',
        "rarr" => '→',
        "larr" => '←',
        "times" => '×',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "deg" => '°',
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "https://example.com/docs/intro.html";

    fn text(html: &str) -> String {
        to_text(html, BASE)
    }

    #[test]
    fn a_page_becomes_its_headings_paragraphs_and_lists() {
        let page = "<!doctype html><html><head><title>Guide</title>\
            <style>body { color: red }</style><script>var a = '<p>no</p>';</script></head>\
            <body><h1>Getting  started</h1><p>Install\n   it <b>first</b>.</p>\
            <ul><li>One</li><li>Two &amp; three</li></ul><!-- hidden --><p>Done.</p></body></html>";
        assert_eq!(
            text(page),
            "Guide\n\n# Getting started\n\nInstall it first.\n\n- One\n- Two & three\n\nDone."
        );
    }

    #[test]
    fn links_keep_their_address_worked_out_against_the_page() {
        assert_eq!(
            text(
                "<p>See <a href=\"api.html#run\">the API</a>, <a href='/x'>x</a> and \
                  <a href=\"#top\">up</a> or <a href=\"mailto:a@b.c\">mail</a>.</p>"
            ),
            "See [the API](https://example.com/docs/api.html#run), \
             [x](https://example.com/x) and up or mail."
        );
        // A link that says its own address is written once.
        assert_eq!(
            text("<a href=https://example.org/>https://example.org/</a>"),
            "https://example.org/"
        );
        assert_eq!(
            text("<a href=\"/a\"><div>Card</div><div>More</div></a>"),
            "Card\nMore (https://example.com/a)"
        );
    }

    #[test]
    fn preformatted_text_keeps_its_spacing() {
        assert_eq!(
            text("<p>Run:</p><pre><code>fn main() {\n    run();\n}</code></pre><p>ok</p>"),
            "Run:\n\nfn main() {\n    run();\n}\n\nok"
        );
    }

    #[test]
    fn tables_images_and_rules_are_written_plainly() {
        assert_eq!(
            text("<table><tr><th>Name</th><th>Use</th></tr><tr><td>a</td><td>b</td></tr></table>"),
            "Name | Use\na | b"
        );
        assert_eq!(
            text("<p>Logo <img src=x.png alt=\"The logo\"> here<hr>after</p>"),
            "Logo [image: The logo] here\n\n---\n\nafter"
        );
    }

    #[test]
    fn character_references_are_read_and_unknown_ones_left() {
        assert_eq!(
            text("a &lt; b &#38; c &#x2014; d&nbsp;e &unknown; &amp;amp; & f"),
            "a < b & c — d e &unknown; &amp; & f"
        );
    }

    #[test]
    fn what_is_broken_is_read_as_far_as_it_goes() {
        assert_eq!(text("1 < 2 and 3 > 2"), "1 < 2 and 3 > 2");
        assert_eq!(text("<p>open <b>never closed"), "open never closed");
        assert_eq!(text("text <script>never closed"), "text");
        assert_eq!(
            text("<a title=\"a > b\" href=\"/x\">y</a>"),
            "[y](https://example.com/x)"
        );
        assert_eq!(text("<p>cut <a href=\"x"), "cut <a href=\"x");
        assert_eq!(text(""), "");
    }
}
