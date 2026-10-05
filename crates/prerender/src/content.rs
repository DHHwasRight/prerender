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
    static OPEN: OnceLock<Regex> = OnceLock::new();
    static CLOSE: OnceLock<Regex> = OnceLock::new();
    // Attribute values may contain `>`, so the value alternatives are matched
    // before the bare-character fallback.
    let open = OPEN.get_or_init(|| {
        Regex::new(r#"(?s)<([A-Z][A-Za-z0-9]*)((?:\s+[^>"']*(?:"[^"]*"|'[^']*')?)*?)\s*(/?)>"#).unwrap()
    });
    let close = CLOSE.get_or_init(|| Regex::new(r"</([A-Z][A-Za-z0-9]*)>").unwrap());

    let mut out = String::with_capacity(md.len() + md.len() / 8);
    for (line, in_code) in fenced_lines(md) {
        if in_code {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        let rewritten = close.replace_all(line, |caps: &regex::Captures<'_>| {
            closing_html(&caps[1]).to_string()
        });
        let rewritten = open.replace_all(&rewritten, |caps: &regex::Captures<'_>| {
            opening_html(&caps[1], &caps[2], !caps[3].is_empty())
        });
        out.push_str(&rewritten);
        out.push('\n');
    }
    out
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

