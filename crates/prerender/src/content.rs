//! Markdown content: frontmatter, body, and the URL it belongs at.
//!
//! Templates are compiled into the binary, but content is not — a docs site has
//! hundreds of pages and they change without a rebuild of the template code.
//! So content is read from disk when the site builds, and the route list comes
//! from the content tree rather than from a list someone maintains by hand.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{anyhow, Result};
use comrak::adapters::SyntaxHighlighterAdapter;
use gray_matter::{Matter, Pod};
use regex::Regex;
use serde::Deserialize;
use syntect::html::{css_for_theme_with_class_style, ClassStyle, ClassedHTMLGenerator};
use syntect::parsing::{SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;
use two_face::theme::EmbeddedLazyThemeSet;
use walkdir::WalkDir;

/// Frontmatter fields this generator understands. Everything else in the block
/// is ignored rather than rejected, because a docs site accumulates keys that
/// only some tool cares about.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct FrontMatter {
    pub title: String,
    pub description: String,
    /// Mintlify and friends use this to override the URL the page lives at.
    pub slug: Option<String>,
}

/// One content file, parsed.
#[derive(Debug, Clone)]
pub struct Doc {
    /// URL path, derived from the file's location under the content root.
    pub url: String,
    /// Path relative to the content root, without the extension.
    pub slug: String,
    pub front: FrontMatter,
    /// The body, already rendered to HTML.
    pub html: String,
    pub source: PathBuf,
}

/// Rewrites the MDX-style component tags a Mintlify export is full of.
///
/// Only the callouts are given real meaning; the rest become plain containers
/// so their content survives even though their behaviour does not. A tag's
/// `title` becomes a heading, because for `Accordion`, `Step` and `Card` it is
/// the only thing identifying the block.
///
/// Two things this has to get right, both found by counting the real corpus
/// rather than by reading the markdown spec:
///
/// * Most tags carry attributes (`<Tab title="Ruby">`), and matching only the
///   bare form left 928 of them untouched in Lago's docs.
/// * Fenced code blocks contain tags that look identical and must not be
///   rewritten, or every JSX sample in the documentation is silently corrupted.
fn rewrite_components(md: &str) -> String {
    static CLOSE: OnceLock<Regex> = OnceLock::new();
    let close = CLOSE.get_or_init(|| Regex::new(r"</([A-Z][A-Za-z0-9]*)>").unwrap());

    let without_js = strip_javascript(md);
    let lines: Vec<String> = without_js.lines().map(str::to_string).collect();
    let dedented = dedent_components(&lines).join("\n");

    // Replacement runs over whole runs of prose rather than line by line.
    // Tags are routinely written across several lines --
    //
    // ```text
    //   <Card
    //     title="Billing"
    //     icon="receipt">
    // ```
    //
    // -- and a per-line regex cannot match one. Those survived into the output
    // as unknown elements, which the HTML parser then nested everything else
    // inside, pulling the whole document out of shape.
    let mut out = String::with_capacity(dedented.len() + dedented.len() / 8);
    let mut prose = String::new();

    let flush = |prose: &mut String, out: &mut String| {
        if prose.is_empty() {
            return;
        }
        let rewritten = close.replace_all(prose, |caps: &regex::Captures<'_>| {
            closing_html(&caps[1]).to_string()
        });
        out.push_str(&sanitize_raw_tags(&rewrite_open_tags(&rewritten)));
        prose.clear();
    };

    for (line, in_code) in fenced_lines(&dedented) {
        if in_code {
            flush(&mut prose, &mut out);
            out.push_str(line);
            out.push('\n');
        } else {
            prose.push_str(line);
            prose.push('\n');
        }
    }
    flush(&mut prose, &mut out);
    out
}


/// Removes the JavaScript an MDX file may carry.
///
/// Mintlify pages can be React modules: a `mode: "custom"` landing page starts
/// with `export function openSearch() {…}`, embeds `{…}` expressions as element
/// children, and styles with `style={{…}}`. None of that is content, and a
/// markdown renderer emits all of it as visible text — the Lago welcome page
/// showed its own event handlers to the reader.
///
/// This strips what cannot be rendered rather than attempting to run it. A
/// page built mostly from JSX will come out sparse; it will not come out as a
/// listing of someone's click handler.
fn strip_javascript(md: &str) -> String {
    let mut out = String::with_capacity(md.len());
    let mut depth = 0usize;
    let mut skipping_statement = false;
    let mut skipping_until: Option<&'static str> = None;

    for (line, in_code) in fenced_lines(md) {
        if in_code {
            out.push_str(line);
            out.push('\n');
            continue;
        }

        let trimmed = line.trim_start();

        // A <script> or <style> block is page machinery, and its body is not
        // markup the renderer should show. The JSON-LD block on Lago's welcome
        // page was otherwise printed to the reader in full.
        if let Some(close) = skipping_until {
            if trimmed.contains(close) {
                skipping_until = None;
            }
            continue;
        }
        for (open, close) in [("<script", "</script>"), ("<style", "</style>")] {
            if trimmed.starts_with(open) {
                if !trimmed.contains(close) {
                    skipping_until = Some(close);
                }
                break;
            }
        }
        if skipping_until.is_some() || trimmed.starts_with("<script") || trimmed.starts_with("<style") {
            continue;
        }

        // `export`/`import` at the top level introduce a module, not prose.
        if depth == 0 && !skipping_statement {
            if trimmed.starts_with("export ") || trimmed.starts_with("import ") {
                skipping_statement = true;
            }
            // A line opening with a brace between tags is an expression, not
            // text; prose does not begin a paragraph with `{`.
            else if trimmed.starts_with('{') {
                skipping_statement = true;
            }
        }

        if skipping_statement {
            depth = depth
                .saturating_add(line.matches('{').count())
                .saturating_sub(line.matches('}').count());
            if depth == 0 {
                skipping_statement = false;
            }
            continue;
        }

        out.push_str(&strip_expression_children(line));
        out.push('\n');
    }
    out
}

/// Drops `{…}` runs that sit between tags, which in MDX are JavaScript
/// expressions rather than text. A brace in ordinary prose is left alone,
/// since it is only an expression when it follows markup.
fn strip_expression_children(line: &str) -> String {
    if !line.contains('{') {
        return line.to_string();
    }
    let bytes = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < line.len() {
        if bytes[i] == b'{' && out.trim_end().ends_with('>') {
            let mut depth = 0usize;
            let mut j = i;
            while j < line.len() {
                match bytes[j] {
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            if depth == 0 && j < line.len() {
                i = j + 1;
                continue;
            }
        }
        let ch = line[i..].chars().next().expect("non-empty remainder");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Strips the indentation MDX authors put inside component tags.
///
/// Content is routinely written as
///
/// ```text
///   <Tab title="Dashboard">
///       1. Access the **"Coupons"** section
///   </Tab>
/// ```
///
/// which MDX renders as a list. CommonMark does not: four spaces makes an
/// indented code block, so every list, heading and emphasis inside a component
/// would come out as literal text. Mintlify strips this; so must we, or the
/// markdown inside 235 `<Tab>`s and 118 `<Step>`s is lost.
///
/// The interior of each block is shifted left by its own smallest indent, then
/// the same treatment is applied to anything nested inside it.
fn dedent_components(lines: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(lines.len());
    let mut i = 0;
    let mut fence: Option<String> = None;

    while i < lines.len() {
        let trimmed = lines[i].trim_start();
        if let Some(marker) = fence_marker(trimmed) {
            match &fence {
                Some(open) if *open == marker => fence = None,
                Some(_) => {}
                None => fence = Some(marker),
            }
            out.push(lines[i].clone());
            i += 1;
            continue;
        }
        if fence.is_some() {
            out.push(lines[i].clone());
            i += 1;
            continue;
        }

        match open_block_at(lines, i).and_then(|(tag, last)| {
            find_close(lines, last, &tag).map(|close| (last, close))
        }) {
            Some((last, close)) => {
                // The tag itself may span several lines; all of them are
                // emitted flush left so the rewriter sees one tag.
                let shift_tag = lines[i].len() - lines[i].trim_start().len();
                for line in &lines[i..=last] {
                    out.push(strip_indent(line, shift_tag));
                }
                let interior = &lines[last + 1..close];
                let shift = min_indent(interior);
                let dedented: Vec<String> = interior
                    .iter()
                    .map(|line| strip_indent(line, shift))
                    .collect();
                out.extend(dedent_components(&dedented));
                out.push(lines[close].trim_start().to_string());
                i = close + 1;
            }
            None => {
                out.push(lines[i].clone());
                i += 1;
            }
        }
    }
    out
}

fn fence_marker(trimmed: &str) -> Option<String> {
    for marker in ["```", "~~~"] {
        if trimmed.starts_with(marker) {
            return Some(marker.to_string());
        }
    }
    None
}

/// The tag name and last line of an opening tag starting on line `i`, when
/// that line begins one.
///
/// Lowercase tags count too. Raw HTML in MDX is indented for readability just
/// as components are, and four spaces is an indented code block — the Lago
/// welcome page printed its own card markup to the reader as a code listing.
///
/// The tag is scanned rather than pattern-matched on the line, because an
/// opening tag routinely spans many lines — a `<Card>` whose `icon={…}` holds
/// an inline SVG runs to thirty. Treating only single-line tags as blocks left
/// those bodies undedented, and a four-space-indented body is an indented code
/// block to CommonMark, so the content came out as a literal listing.
fn open_block_at(lines: &[String], i: usize) -> Option<(String, usize)> {
    let trimmed = lines[i].trim_start();
    if !trimmed.starts_with('<') || !trimmed[1..].starts_with(|c: char| c.is_ascii_alphabetic()) {
        return None;
    }
    let indent = lines[i].len() - trimmed.len();
    let rest = lines[i..].join("\n");
    let (name, _attrs, self_closing, end) = scan_tag_any(&rest, indent)?;
    // Whitespace is content in these, so their indentation is not ours to
    // normalise.
    if matches!(name.as_str(), "pre" | "code" | "textarea") {
        return None;
    }
    if self_closing {
        return None;
    }
    // Anything after the tag on its last line means it is inline prose, not a
    // block wrapper, and dedenting around it would reflow the sentence.
    let last = i + rest[..end].matches('\n').count();
    if !rest[end..].lines().next().unwrap_or("").trim().is_empty() {
        return None;
    }
    Some((name, last))
}

/// The line holding the close tag matching an opener that ends on `open_last`.
///
/// Scanning starts after the opener and at depth one, because a tag spanning
/// several lines does not begin on the line it ends on — counting it again
/// would never balance.
fn find_close(lines: &[String], open_last: usize, tag: &str) -> Option<usize> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut depth = 1usize;
    for (offset, line) in lines.iter().enumerate().skip(open_last + 1) {
        let trimmed = line.trim_start();
        if trimmed.starts_with(&open) && !trimmed.trim_end().ends_with("/>") {
            depth += 1;
        }
        if trimmed.starts_with(&close) {
            depth -= 1;
            if depth == 0 {
                return Some(offset);
            }
        }
    }
    None
}

/// The indentation to strip from a block's interior.
///
/// Measured from the prose, not from nested tags. A `<Frame>` sitting at a
/// shallower indent than the paragraphs around it dragged the baseline down,
/// and the text that had been four spaces deeper landed at exactly four —
/// which CommonMark reads as a code block. Lists and bold inside those blocks
/// came out as literal text on a dozen pages.
///
/// Nested tags are dedented by their own recursion, so excluding them here
/// costs nothing.
fn min_indent(lines: &[String]) -> usize {
    let indent = |l: &String| l.len() - l.trim_start().len();
    let is_tag = |l: &String| {
        let t = l.trim_start();
        t.starts_with("</") || (t.starts_with('<') && t[1..].starts_with(|c: char| c.is_ascii_alphabetic()))
    };

    let prose = lines
        .iter()
        .filter(|l| !l.trim().is_empty() && !is_tag(l))
        .map(indent)
        .min();

    prose.unwrap_or_else(|| {
        lines.iter().filter(|l| !l.trim().is_empty()).map(indent).min().unwrap_or(0)
    })
}

fn strip_indent(line: &str, shift: usize) -> String {
    let indent = line.len() - line.trim_start().len();
    // Only leading ASCII whitespace is removed, so slicing stays on a boundary.
    line[shift.min(indent)..].to_string()
}

/// Yields each line with whether it sits inside a fenced code block. The fence
/// itself counts as inside, so it is copied through untouched.
fn fenced_lines(md: &str) -> impl Iterator<Item = (&str, bool)> {
    let mut fence: Option<String> = None;
    md.lines().map(move |line| {
        let trimmed = line.trim_start();
        let opener = trimmed
            .starts_with("```")
            .then(|| "```")
            .or_else(|| trimmed.starts_with("~~~").then(|| "~~~"));
        match (&fence, opener) {
            (None, Some(marker)) => {
                fence = Some(marker.to_string());
                (line, true)
            }
            (Some(marker), Some(found)) if marker == found => {
                fence = None;
                (line, true)
            }
            (Some(_), _) => (line, true),
            (None, None) => (line, false),
        }
    })
}


/// Finds the end of a component tag that starts at `start`.
///
/// Returns the tag name, its attribute text, whether it self-closes, and the
/// byte index just past the closing `>`.
///
/// A regex cannot do this. Attributes are JSX, so a value may be a braced
/// expression containing arbitrary markup:
///
/// ```text
///   <Card title="GoCardless" icon={ <svg width="32">…</svg> }>
/// ```
///
/// Matching to the first `>` ends the tag inside the `<svg>`, which splits the
/// component in half and leaves the rest of the document unbalanced — the
/// article element closes early and every later block escapes its container.
/// So braces and quotes are tracked, and only a `>` at brace depth zero and
/// outside a string ends the tag.
fn scan_tag(src: &str, start: usize) -> Option<(String, String, bool, usize)> {
    let bytes = src.as_bytes();
    if bytes.get(start) != Some(&b'<') {
        return None;
    }

    let mut i = start + 1;
    let name_start = i;
    while i < bytes.len() && bytes[i].is_ascii_alphanumeric() {
        i += 1;
    }
    let name = &src[name_start..i];
    if name.is_empty() || !name.starts_with(|c: char| c.is_ascii_uppercase()) {
        return None;
    }

    let attrs_start = i;
    let mut quote: Option<u8> = None;
    let mut braces = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                b'"' | b'\'' => quote = Some(c),
                b'{' => braces += 1,
                b'}' => braces = braces.saturating_sub(1),
                b'>' if braces == 0 => {
                    let attrs = &src[attrs_start..i];
                    let self_closing = attrs.trim_end().ends_with('/');
                    let attrs = attrs.trim_end().trim_end_matches('/');
                    return Some((name.to_string(), attrs.to_string(), self_closing, i + 1));
                }
                _ => {}
            },
        }
        i += 1;
    }
    None
}


/// Rewrites raw HTML tags to drop attributes a browser cannot use.
///
/// MDX pages carry React attributes — `className`, `style={{…}}`, and event
/// handlers whose values are whole functions. A handler written across several
/// lines contains a blank line, which ends the HTML block as far as CommonMark
/// is concerned, and the remainder of the function is printed to the reader as
/// prose. Dropping them removes both the noise and the thing that breaks the
/// block, and `className` becomes `class` so the markup is at least styleable.
fn sanitize_raw_tags(prose: &str) -> String {
    const KEEP: [&str; 8] = ["href", "src", "alt", "id", "title", "colspan", "rowspan", "lang"];

    let mut out = String::with_capacity(prose.len());
    let mut i = 0;
    while i < prose.len() {
        let rest = &prose[i..];
        if rest.starts_with('<') && rest[1..].starts_with(|c: char| c.is_ascii_lowercase()) {
            if let Some((name, attrs, self_closing, end)) = scan_tag_any(prose, i) {
                if attrs.contains('{') || attrs.contains("className") {
                    out.push('<');
                    out.push_str(&name);
                    for (key, value) in attributes(&attrs) {
                        let key = if key.eq_ignore_ascii_case("classname") { "class" } else { &key };
                        if KEEP.contains(&key) || key == "class" {
                            out.push_str(&format!(" {key}=\"{}\"", escape_text(&value)));
                        }
                    }
                    if self_closing {
                        out.push_str(" /");
                    }
                    out.push('>');
                    i = end;
                    continue;
                }
            }
        }
        let ch = rest.chars().next().expect("non-empty remainder");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Quoted attributes only. A braced JSX value has no HTML equivalent, so it is
/// skipped rather than guessed at.
fn attributes(attrs: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let bytes = attrs.as_bytes();
    let mut i = 0;
    while i < attrs.len() {
        while i < attrs.len() && !bytes[i].is_ascii_alphabetic() {
            i += 1;
        }
        let start = i;
        while i < attrs.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'-') {
            i += 1;
        }
        if start == i {
            break;
        }
        let key = &attrs[start..i];
        while i < attrs.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i < attrs.len() && bytes[i] == b'=' {
            i += 1;
            while i < attrs.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < attrs.len() && bytes[i] == b'"' {
                let value_start = i + 1;
                if let Some(close) = attrs[value_start..].find('"') {
                    found.push((key.to_string(), attrs[value_start..value_start + close].to_string()));
                    i = value_start + close + 1;
                    continue;
                }
            }
            // Braced or unquoted: skip the value entirely.
            let mut depth = 0usize;
            while i < attrs.len() {
                match bytes[i] {
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            i += 1;
                            break;
                        }
                    }
                    b' ' if depth == 0 => break,
                    _ => {}
                }
                i += 1;
            }
        }
    }
    found
}

/// As [`scan_tag`], without requiring the name to start uppercase.
fn scan_tag_any(src: &str, start: usize) -> Option<(String, String, bool, usize)> {
    let bytes = src.as_bytes();
    if bytes.get(start) != Some(&b'<') {
        return None;
    }
    let mut i = start + 1;
    let name_start = i;
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'-') {
        i += 1;
    }
    let name = &src[name_start..i];
    if name.is_empty() {
        return None;
    }
    let attrs_start = i;
    let mut quote: Option<u8> = None;
    let mut braces = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                b'"' | b'\'' => quote = Some(c),
                b'{' => braces += 1,
                b'}' => braces = braces.saturating_sub(1),
                b'>' if braces == 0 => {
                    let attrs = &src[attrs_start..i];
                    let self_closing = attrs.trim_end().ends_with('/');
                    return Some((
                        name.to_string(),
                        attrs.trim_end().trim_end_matches('/').to_string(),
                        self_closing,
                        i + 1,
                    ));
                }
                _ => {}
            },
        }
        i += 1;
    }
    None
}

/// Rewrites every opening component tag in a run of prose.
fn rewrite_open_tags(prose: &str) -> String {
    let mut out = String::with_capacity(prose.len());
    let mut i = 0;
    while i < prose.len() {
        let rest = &prose[i..];
        if rest.starts_with('<')
            && rest[1..].starts_with(|c: char| c.is_ascii_uppercase())
        {
            if let Some((name, attrs, self_closing, end)) = scan_tag(prose, i) {
                out.push_str(&opening_html(&name, &attrs, self_closing));
                i = end;
                continue;
            }
        }
        let ch = rest.chars().next().expect("non-empty remainder");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// How a component maps onto HTML: the class suffix, and whether it is a
/// callout rather than a plain container.
fn component_kind(tag: &str) -> (String, bool) {
    const CALLOUTS: [(&str, &str); 6] = [
        ("Info", "info"),
        ("Note", "note"),
        ("Tip", "tip"),
        ("Warning", "warning"),
        ("Check", "check"),
        ("Update", "update"),
    ];
    match CALLOUTS.iter().find(|(name, _)| *name == tag) {
        Some((_, kind)) => ((*kind).to_string(), true),
        None => (tag.to_lowercase(), false),
    }
}

fn attribute<'a>(attrs: &'a str, name: &str) -> Option<&'a str> {
    let at = attrs.find(&format!("{name}=\""))? + name.len() + 2;
    let rest = &attrs[at..];
    Some(&rest[..rest.find('"')?])
}

fn opening_html(tag: &str, attrs: &str, self_closing: bool) -> String {
    let (kind, is_callout) = component_kind(tag);
    let class = if is_callout {
        format!("callout callout-{kind}")
    } else {
        format!("mdx mdx-{kind}")
    };
    if self_closing {
        // Nothing to wrap, but the attributes often are the content (an icon,
        // a field name), so they are kept as data for CSS to surface.
        return match attribute(attrs, "title").or_else(|| attribute(attrs, "name")) {
            Some(title) => format!("<div class=\"{class}\"><p class=\"mdx-title\">{}</p></div>", escape_text(title)),
            None => format!("<div class=\"{class}\"></div>"),
        };
    }
    match attribute(attrs, "title").or_else(|| attribute(attrs, "name")) {
        Some(title) => format!(
            "<div class=\"{class}\">\n\n<p class=\"mdx-title\">{}</p>\n\n",
            escape_text(title)
        ),
        None => format!("<div class=\"{class}\">\n\n"),
    }
}

fn closing_html(_tag: &str) -> &'static str {
    "\n\n</div>"
}

/// Minimal text escaping for an attribute value promoted into the document.
fn escape_text(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Classes are prefixed so syntect's scope atoms — `keyword`, `string`,
/// `comment`, `source` — cannot collide with the site's own class names.
/// [`highlight_stylesheet`] uses the same prefix, so the two stay in step.
const CLASS_PREFIX: &str = "hl-";
const CLASS_STYLE: ClassStyle = ClassStyle::SpacedPrefixed { prefix: CLASS_PREFIX };
/// The selector syntect puts the theme's own foreground and background on.
const THEME_CLASS: &str = "hl-code";

/// Syntax definitions, from bat's set rather than syntect's bundled defaults:
/// this content is mostly ruby, bash, json and TypeScript, and the defaults are
/// missing a good half of that.
///
/// Deserializing the set costs more than parsing a document does, so it happens
/// once per process and every document shares it.
fn syntaxes() -> &'static SyntaxSet {
    static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
    SYNTAXES.get_or_init(two_face::syntax::extra_newlines)
}

fn themes() -> &'static EmbeddedLazyThemeSet {
    static THEMES: OnceLock<EmbeddedLazyThemeSet> = OnceLock::new();
    THEMES.get_or_init(two_face::theme::extra)
}

/// Resolves a fence's info string to a syntax, falling back to plain text so an
/// unknown or absent tag still produces a well-formed, escaped block.
fn syntax_for(lang: Option<&str>) -> &'static SyntaxReference {
    let set = syntaxes();
    lang.map(str::trim)
        // Fences carry extra tokens in this content (```json title="x"); only
        // the first one names a language.
        .and_then(|l| l.split([' ', ',']).next())
        .filter(|l| !l.is_empty())
        .and_then(|l| set.find_syntax_by_token(l))
        .unwrap_or_else(|| set.find_syntax_plain_text())
}

/// Highlights fenced blocks at build time into CSS classes rather than inline
/// styles: the markup stays small across hundreds of pages, and the colour
/// scheme becomes a stylesheet swap instead of a rebuild.
struct Highlighter;

impl SyntaxHighlighterAdapter for Highlighter {
    fn write_highlighted(
        &self,
        output: &mut dyn fmt::Write,
        lang: Option<&str>,
        code: &str,
    ) -> fmt::Result {
        let syntax = syntax_for(lang);
        let mut generator =
            ClassedHTMLGenerator::new_with_class_style(syntax, syntaxes(), CLASS_STYLE);

        for line in LinesWithEndings::from(code) {
            // A syntax that trips over its own regex would otherwise drop the
            // rest of the block on the floor. Losing colour is acceptable;
            // losing the sample is not.
            if generator
                .parse_html_for_line_which_includes_newline(line)
                .is_err()
            {
                return comrak::html::escape(output, code);
            }
        }
        output.write_str(&generator.finalize())
    }

    fn write_pre_tag(
        &self,
        output: &mut dyn fmt::Write,
        attributes: HashMap<&'static str, Cow<'_, str>>,
    ) -> fmt::Result {
        comrak::html::write_opening_tag(output, "pre", attributes)
    }

    fn write_code_tag(
        &self,
        output: &mut dyn fmt::Write,
        mut attributes: HashMap<&'static str, Cow<'_, str>>,
    ) -> fmt::Result {
        // The theme's own colours hang off `hl-code`, and syntect's spans are
        // emitted inside the `<code>` tag, so that is where the class belongs.
        // Comrak's `language-*` class is kept — copy buttons and CSS key off it.
        let class = match attributes.remove("class") {
            Some(existing) => format!("{THEME_CLASS} {existing}"),
            None => THEME_CLASS.to_string(),
        };
        attributes.insert("class", class.into());
        comrak::html::write_opening_tag(output, "code", attributes)
    }
}

/// The CSS that matches the classes [`parse`] emits, for one theme.
///
/// Callers write this to disk next to the site's own stylesheet; switching the
/// colour scheme is then a matter of regenerating this file, with no change to
/// any page.
///
/// Theme names are bat's, matched case-insensitively: `ansi`, `base16`,
/// `base16-eighties.dark`, `base16-mocha.dark`, `base16-ocean.dark`,
/// `base16-ocean.light`, `base16-256`, `Catppuccin Frappe`, `Catppuccin Latte`,
/// `Catppuccin Macchiato`, `Catppuccin Mocha`, `Coldark-Cold`, `Coldark-Dark`,
/// `DarkNeon`, `Dracula`, `GitHub`, `gruvbox-dark`, `gruvbox-light`,
/// `InspiredGitHub`, `1337`, `Monokai Extended`, `Monokai Extended Bright`,
/// `Monokai Extended Light`, `Monokai Extended Origin`, `Nord`, `OneHalfDark`,
/// `OneHalfLight`, `Solarized (dark)`, `Solarized (light)`, `Sublime Snazzy`,
/// `TwoDark`, `zenburn`.
pub fn highlight_stylesheet(theme: &str) -> Result<String> {
    let name = EmbeddedLazyThemeSet::theme_names()
        .iter()
        .copied()
        .find(|t| t.as_name().eq_ignore_ascii_case(theme))
        .ok_or_else(|| {
            let known: Vec<&str> = EmbeddedLazyThemeSet::theme_names()
                .iter()
                .map(|t| t.as_name())
                .collect();
            anyhow!("unknown highlight theme {theme:?}; known themes: {}", known.join(", "))
        })?;

    css_for_theme_with_class_style(themes().get(name), CLASS_STYLE)
        .map_err(|e| anyhow!("stylesheet for theme {}: {e}", name.as_name()))
}

fn markdown_options() -> comrak::Options<'static> {
    let mut options = comrak::Options::default();
    options.extension.table = true;
    options.extension.strikethrough = true;
    options.extension.autolink = true;
    options.extension.tasklist = true;
    options.extension.footnotes = true;
    // Anchors on headings, so a sidebar can link into a page.
    options.extension.header_id_prefix = Some(String::new());
    // The content is ours, and it contains HTML on purpose.
    options.render.r#unsafe = true;
    options
}

fn render_markdown(md: &str) -> String {
    static HIGHLIGHTER: Highlighter = Highlighter;

    let mut plugins = comrak::options::Plugins::default();
    plugins.render.codefence_syntax_highlighter = Some(&HIGHLIGHTER);
    comrak::markdown_to_html_with_plugins(md, &markdown_options(), &plugins)
}

/// gray_matter owns the delimiters; the YAML between them goes to
/// `serde_norway` instead of gray_matter's bundled `yaml-rust2` engine.
///
/// The content has multi-line double-quoted values whose continuation lines sit
/// flush left. That is out of spec, and yaml-rust2 rejects it, but libyaml —
/// and so every other tool these docs have ever passed through — accepts it.
/// Dropping a page over it would be a regression dressed up as strictness.
struct Yaml;

impl gray_matter::engine::Engine for Yaml {
    fn parse(content: &str) -> gray_matter::Result<Pod> {
        let value: serde_norway::Value = serde_norway::from_str(content)
            .map_err(|e| gray_matter::Error::deserialize_error(&e.to_string()))?;
        Ok(pod(value))
    }
}

/// `Pod` is the only currency gray_matter's `Engine` trait deals in, so the
/// parsed YAML has to be restated in it before serde can type it.
fn pod(value: serde_norway::Value) -> Pod {
    use serde_norway::Value;
    match value {
        Value::Null => Pod::Null,
        Value::Bool(b) => Pod::Boolean(b),
        Value::Number(n) => match n.as_i64() {
            Some(i) => Pod::Integer(i),
            None => Pod::Float(n.as_f64().unwrap_or_default()),
        },
        Value::String(s) => Pod::String(s),
        Value::Sequence(items) => Pod::Array(items.into_iter().map(pod).collect()),
        Value::Mapping(map) => Pod::Hash(
            map.into_iter()
                .filter_map(|(k, v)| pod_key(k).map(|k| (k, pod(v))))
                .collect(),
        ),
        // A `!Tag` annotation has no meaning to any key read here; the value it
        // wraps is the part worth keeping.
        Value::Tagged(tagged) => pod(tagged.value),
    }
}

/// Composite keys cannot survive the trip into `Pod`'s string-keyed hash, and
/// nothing in this frontmatter uses one.
fn pod_key(value: serde_norway::Value) -> Option<String> {
    use serde_norway::Value;
    match value {
        Value::String(s) => Some(s),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Splits the YAML frontmatter block off the front of a file and types it.
///
/// Unknown keys fall away in the `Pod` → `FrontMatter` step, and a file with no
/// block at all simply yields the defaults.
fn front_matter(text: &str) -> Result<(FrontMatter, String), gray_matter::Error> {
    let parsed = Matter::<Yaml>::new().parse::<FrontMatter>(text)?;
    Ok((parsed.data.unwrap_or_default(), parsed.content))
}

/// Renders one markdown file.
pub fn parse(source: &Path, content_root: &Path) -> io::Result<Doc> {
    let text = fs::read_to_string(source)?;
    let (front, body) = front_matter(&text)
        .map_err(|e| io::Error::other(format!("{}: frontmatter: {e}", source.display())))?;

    let slug = source
        .strip_prefix(content_root)
        .unwrap_or(source)
        .with_extension("")
        .to_string_lossy()
        .replace('\\', "/");
    // `foo/index.md` is the page for `foo`, not for `foo/index`.
    let slug = slug.strip_suffix("/index").unwrap_or(&slug).to_string();
    let url = front.slug.clone().unwrap_or_else(|| format!("/{slug}"));

    let html = render_markdown(&rewrite_components(&body));
    Ok(Doc { url, slug, front, html, source: source.to_path_buf() })
}

/// Reads every `.md` and `.mdx` under `root`, in sorted order so a build is
/// reproducible.
pub fn load_dir(root: &Path) -> io::Result<Vec<Doc>> {
    let mut sources = Vec::new();
    for entry in WalkDir::new(root) {
        let entry = entry?;
        if entry.file_type().is_file()
            && matches!(
                entry.path().extension().and_then(|e| e.to_str()),
                Some("md" | "mdx")
            )
        {
            sources.push(entry.into_path());
        }
    }
    // WalkDir's traversal order follows the filesystem, so sort by full path
    // rather than relying on per-directory ordering.
    sources.sort();
    sources.iter().map(|p| parse(p, root)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn md(src: &str) -> String {
        comrak::markdown_to_html(&rewrite_components(src), &markdown_options())
    }

    #[test]
    fn components_with_attributes_are_rewritten() {
        // Matching only the bare `<Tab>` form left 928 tags untouched in the
        // real corpus, which is what this guards against.
        let html = md("<Tab title=\"Ruby\">\n  Install it.\n</Tab>\n");
        assert!(html.contains(r#"<div class="mdx mdx-tab">"#), "{html}");
        assert!(html.contains("Ruby"), "{html}");
        assert!(html.contains("Install it."), "{html}");
    }

    #[test]
    fn a_title_attribute_becomes_a_heading() {
        let html = md("<Accordion title=\"Why billing is hard\">\n  Because.\n</Accordion>\n");
        assert!(html.contains(r#"<p class="mdx-title">Why billing is hard</p>"#), "{html}");
    }

    #[test]
    fn a_title_is_escaped_rather_than_injected() {
        let html = md("<Card title=\"a <script> & co\">\n  x\n</Card>\n");
        assert!(!html.contains("<script>"), "{html}");
        assert!(html.contains("&lt;script&gt;"), "{html}");
    }

    #[test]
    fn self_closing_components_do_not_swallow_the_rest_of_the_page() {
        let html = md("<Icon name=\"check\" />\n\nAfter the icon.\n");
        assert!(html.contains("After the icon."), "{html}");
    }

    #[test]
    fn markdown_indented_inside_a_component_is_still_markdown() {
        // MDX authors indent inside tags; CommonMark reads four spaces as a
        // code block, so without dedenting this list is literal text.
        let html = md("<Tab title=\"Dashboard\">\n    To create one:\n\n    1. Access the **list**;\n    2. Click add;\n</Tab>\n");
        assert!(html.contains("<ol"), "list should render: {html}");
        assert!(html.contains("<strong>list</strong>"), "emphasis should render: {html}");
        assert!(!html.contains("<pre"), "should not become a code block: {html}");
    }

    #[test]
    fn a_nested_tag_does_not_drag_the_baseline_down() {
        // The <Frame> sits shallower than the prose. Measuring the baseline
        // from it left the list at four spaces, i.e. a code block.
        let html = md(
            "<Tab title=\"Dashboard\">\n      To create one:\n\n        1. Open the **Features** page;\n\n    <Frame caption=\"x\">\n      shot\n    </Frame>\n</Tab>\n",
        );
        assert!(html.contains("<ol"), "list should render: {html}");
        assert!(html.contains("<strong>Features</strong>"), "bold should render: {html}");
        assert!(!html.contains("1. Open the **Features**"), "literal text: {html}");
    }

    #[test]
    fn nested_components_are_dedented_at_each_level() {
        let html = md("<Tabs>\n  <Tab title=\"One\">\n      # Heading\n  </Tab>\n</Tabs>\n");
        assert!(html.contains("<h1"), "nested heading should render: {html}");
    }

    #[test]
    fn code_blocks_inside_components_keep_their_own_indentation() {
        let html = md("<Tab title=\"Shell\">\n    ```bash\n    if true; then\n      echo hi\n    fi\n    ```\n</Tab>\n");
        // The fence is dedented with its block, but the relative indent inside
        // the sample must survive or every nested snippet is reflowed.
        assert!(html.contains("echo hi"), "{html}");
        assert!(html.contains("  echo hi") || html.contains("  <span"), "relative indent lost: {html}");
    }

    #[test]
    fn a_tag_written_across_several_lines_is_rewritten() {
        // These survived a per-line regex and reached the browser as unknown
        // elements, which then nested the rest of the document inside them.
        let html = md("<Card\n  title=\"Billing\"\n  icon=\"receipt\">\n  Body.\n</Card>\n");
        assert!(html.contains(r#"<div class="mdx mdx-card">"#), "{html}");
        assert!(html.contains("Billing"), "{html}");
        assert!(!html.to_lowercase().contains("<card"), "raw tag survived: {html}");
    }

    #[test]
    fn no_capitalised_tag_survives_a_realistic_block() {
        let html = md(
            "<CardGroup cols={2}>\n  <Card\n    title=\"One\"\n    icon=\"x\"\n  >\n    ```bash\n    echo hi\n    ```\n  </Card>\n</CardGroup>\n",
        );
        assert!(!regex::Regex::new(r"<[A-Z]").unwrap().is_match(&html), "{html}");
        assert!(html.contains("echo hi"), "{html}");
    }

    #[test]
    fn a_jsx_expression_attribute_containing_markup_does_not_split_the_tag() {
        // This is what broke the real site: the `>` inside the <svg> ended the
        // Card tag early, the rest of the document escaped its container, and
        // the page scrolled sideways by thirty thousand pixels.
        let html = md(
            "<Card\n  title=\"GoCardless\"\n  icon={\n    <svg width=\"32\" height=\"32\"><path d=\"M1 2\"/></svg>\n  }\n>\n  Body text.\n</Card>\n",
        );
        assert!(html.contains(r#"<div class="mdx mdx-card">"#), "{html}");
        assert!(html.contains("GoCardless"), "{html}");
        assert!(html.contains("Body text."), "{html}");
        assert!(!html.contains("<svg"), "icon markup leaked into the page: {html}");
        assert_eq!(
            html.matches("<div").count(),
            html.matches("</div>").count(),
            "unbalanced divs: {html}"
        );
    }

    #[test]
    fn module_statements_do_not_reach_the_page() {
        let html = md("export function openSearch() {\n  document.getElementById('x').click();\n}\n\nReal prose.\n");
        assert!(!html.contains("openSearch"), "{html}");
        assert!(!html.contains("getElementById"), "{html}");
        assert!(html.contains("Real prose."), "{html}");
    }

    #[test]
    fn jsx_expression_children_are_dropped_but_prose_braces_are_not() {
        let html = md("<div>{ const d = x ? 'a' : 'b'; }</div>\n\nUse {curly} braces in prose.\n");
        assert!(!html.contains("const d"), "expression leaked: {html}");
        assert!(html.contains("{curly}"), "prose braces were eaten: {html}");
    }

    #[test]
    fn a_fenced_block_keeps_its_javascript() {
        // Only unfenced JavaScript is module code; inside a fence it is the
        // documentation's subject matter.
        let html = md("```js\nexport function demo() { return 1; }\n```\n");
        assert!(html.contains("export"), "{html}");
        assert!(html.contains("demo"), "{html}");
    }

    #[test]
    fn tags_inside_fenced_code_are_left_alone() {
        // Every JSX sample in the documentation depends on this.
        let html = md("```jsx\n<Info>not a callout</Info>\n```\n");
        assert!(html.contains("&lt;Info&gt;"), "{html}");
        assert!(!html.contains("callout-info"), "{html}");
    }

    #[test]
    fn a_tag_after_a_code_block_is_still_rewritten() {
        let html = md("```\n<Info>x</Info>\n```\n\n<Info>\n  real\n</Info>\n");
        assert!(html.contains("callout-info"), "{html}");
        assert_eq!(html.matches("callout-info").count(), 1, "{html}");
    }

    #[test]
    fn front_matter_is_split_from_the_body() {
        let (front, body) = front_matter("---\ntitle: Hi\n---\n\n# Heading\n").unwrap();
        assert_eq!(front.title, "Hi");
        assert_eq!(body, "# Heading");
    }

    #[test]
    fn a_file_without_front_matter_is_all_body() {
        let (front, body) = front_matter("# Heading\n").unwrap();
        assert_eq!(front.title, "");
        assert_eq!(body, "# Heading");
    }

    #[test]
    fn unknown_front_matter_keys_are_ignored() {
        let (front, _) =
            front_matter("---\ntitle: Hi\nmode: wide\nopenapi: get /x\n---\nBody\n").unwrap();
        assert_eq!(front.title, "Hi");
        assert_eq!(front.slug, None);
    }

    #[test]
    fn a_quoted_value_continued_flush_left_is_still_read() {
        let (front, body) =
            front_matter("---\ntitle: Hi\ndescription: \"one\ntwo\"\n---\nBody\n").unwrap();
        assert_eq!(front.description, "one two");
        assert_eq!(body, "Body");
    }

    #[test]
    fn callouts_become_divs_with_their_markdown_intact() {
        let html = render_markdown(&rewrite_components("<Info>\n  Deducted **before** tax.\n</Info>\n"));
        assert!(html.contains(r#"<div class="callout callout-info">"#), "{html}");
        // The blank-line insertion is what makes this bold rather than literal.
        assert!(html.contains("<strong>before</strong>"), "{html}");
    }

    #[test]
    fn unknown_components_keep_their_content() {
        let html = render_markdown(&rewrite_components("<Frame>\n  A diagram\n</Frame>\n"));
        assert!(html.contains("mdx-frame"), "{html}");
        assert!(html.contains("A diagram"), "{html}");
    }

    #[test]
    fn the_theme_class_matches_the_prefix_the_stylesheet_is_generated_with() {
        assert_eq!(THEME_CLASS, format!("{CLASS_PREFIX}code"));
    }

    #[test]
    fn a_tagged_block_comes_out_highlighted() {
        let html = render_markdown("```ruby\ndef charge(amount)\n  amount * 2\nend\n```\n");
        assert!(html.contains(r#"class="hl-code language-ruby""#), "{html}");
        // `def` is a keyword in the ruby syntax, so it must land in its own span.
        assert!(html.contains(r#"<span class="hl-keyword"#), "{html}");
        assert!(html.contains("charge"), "{html}");
    }

    #[test]
    fn an_unknown_language_still_renders_its_content() {
        let html = render_markdown("```wingdings\nnot a real language\n```\n");
        assert!(html.contains("not a real language"), "{html}");
        assert!(html.contains("language-wingdings"), "{html}");
    }

    #[test]
    fn an_untagged_block_still_renders_its_content() {
        let html = render_markdown("```\nlago create <thing>\n```\n");
        assert!(html.contains("lago create"), "{html}");
        // Plain text goes through the highlighter too, so escaping has to
        // survive the detour.
        assert!(html.contains("&lt;thing&gt;"), "{html}");
    }

    #[test]
    fn the_stylesheet_covers_the_classes_that_are_emitted() {
        let css = highlight_stylesheet("base16-ocean.dark").unwrap();
        assert!(css.contains(".hl-code {"), "{css}");
        assert!(css.contains(".hl-keyword"), "{css}");
        assert!(highlight_stylesheet("no-such-theme").is_err());
    }
}

