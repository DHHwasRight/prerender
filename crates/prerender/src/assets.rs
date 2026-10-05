//! Release-time asset processing: minify CSS and JavaScript, and inline them.
//!
//! A docs site ships a handful of small files whose request cost outweighs
//! their transfer cost, so at release they are folded into the document.
//!
//! That tradeoff is worth stating plainly, because it is not free. Inlining
//! removes a round trip from the first paint of every page, but the bytes stop
//! being cacheable: a file served from `/app.css` is fetched once and reused
//! for the rest of the session, whereas the same rules inlined into the head
//! are re-sent with every document. The exchange pays off for a few kilobytes
//! of critical CSS and the navigation script, and stops paying somewhere above
//! that — so inlining is opt-in per asset rather than applied to everything in
//! the static directory.
//!
//! This generator tilts the balance further toward inlining than most: after
//! the first load, navigations fetch partials, which carry no `<head>` at all.
//! The inlined bytes are therefore sent once per full document — which, for a
//! visitor who arrives and then navigates, is once per visit.

use std::collections::HashMap;

use anyhow::{anyhow, Result};
use lightningcss::stylesheet::{MinifyOptions, ParserOptions, PrinterOptions, StyleSheet};
use lightningcss::targets::{Browsers, Targets};
use oxc_allocator::Allocator;
use oxc_codegen::{Codegen, CodegenOptions};
use oxc_minifier::{Minifier, MinifierOptions};
use oxc_parser::Parser;
use oxc_span::SourceType;

/// Encodes a browser version the way Lightning CSS stores them: one `u32` of
/// `major.minor.patch`, a byte each.
const fn version(major: u32, minor: u32) -> u32 {
    (major << 16) | (minor << 8)
}

/// Roughly the last two years of mainstream browser releases.
///
/// The alternative — leaving `targets` unset — is not "no opinion", it is the
/// strongest possible opinion: Lightning CSS then assumes every feature in the
/// source is supported everywhere and emits no prefixes, no fallbacks, and no
/// lowering. Modern CSS in, modern CSS out, broken for anyone a release or two
/// behind. Naming a floor instead means the fallbacks that get emitted are
/// exactly the ones these versions need and nothing older, which is both
/// smaller and honest about who the site is for.
///
/// Two years is the window where evergreen browsers have long since updated
/// and the remaining users are on frozen corporate images and old iPhones that
/// stopped getting iOS updates. Widen it by lowering these numbers; the cost
/// is bytes, not correctness.
fn browser_targets() -> Targets {
    Targets::from(Browsers {
        android: Some(version(130, 0)),
        chrome: Some(version(130, 0)),
        edge: Some(version(130, 0)),
        firefox: Some(version(132, 0)),
        // No version of IE is in any window that includes a 2024 release.
        ie: None,
        ios_saf: Some(version(18, 0)),
        opera: Some(version(115, 0)),
        safari: Some(version(18, 0)),
        samsung: Some(version(27, 0)),
    })
}

/// Minifies CSS.
///
/// `filename` only labels diagnostics; nothing is read from disk. In
/// particular `@import` is left as written rather than resolved — pulling in
/// the imported file is the bundler's job and needs filesystem access this
/// function deliberately does not have.
pub fn minify_css(source: &str, filename: &str) -> Result<String> {
    let targets = browser_targets();

    let mut sheet = StyleSheet::parse(
        source,
        ParserOptions { filename: filename.to_string(), ..ParserOptions::default() },
    )
    .map_err(|e| anyhow!("{filename}: {e}"))?;

    // The structural pass: merging longhands into shorthands, dropping rules
    // that later rules override, collapsing `calc()`. Printing with
    // `minify: true` only removes whitespace, so skipping this step would
    // leave most of the win on the table.
    sheet
        .minify(MinifyOptions { targets, ..MinifyOptions::default() })
        .map_err(|e| anyhow!("{filename}: {e}"))?;

    let out = sheet
        .to_css(PrinterOptions { minify: true, targets, ..PrinterOptions::default() })
        .map_err(|e| anyhow!("{filename}: {e}"))?;

    Ok(out.code)
}

/// Minifies JavaScript.
///
/// The module/script distinction comes from the extension, so a `.cjs` file is
/// not parsed as a module and a `.mjs` one is. An unrecognised extension is
/// treated as a module, which is the only guess that can parse `import`.
pub fn minify_js(source: &str, filename: &str) -> Result<String> {
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(filename).unwrap_or_else(|_| SourceType::mjs());

    let mut parsed = Parser::new(&allocator, source, source_type).parse();

    // oxc recovers from syntax errors and hands back a partial AST, so an
    // empty `diagnostics` is the only signal that the input was actually
    // valid. Minifying a recovered AST would silently ship a file with
    // statements missing, which is worse than failing the build.
    if let Some(first) = parsed.diagnostics.first() {
        return Err(anyhow!("{filename}: {first}"));
    }

    let ret = Minifier::new(MinifierOptions::default()).minify(&allocator, &mut parsed.program);

    // The mangler records its renames in `scoping`, not in the AST, so codegen
    // has to be handed them back or the output comes out unmangled.
    Ok(Codegen::new()
        .with_options(CodegenOptions::minify())
        .with_scoping(ret.scoping)
        .build(&parsed.program)
        .code)
}

/// The assets a document is allowed to absorb, keyed by the URL path their
/// tags reference.
///
/// Minification happens here, when an asset is registered, rather than in
/// [`inline_into_html`]: a site has many pages and few assets, and minifying
/// per page would redo the same work once per route.
#[derive(Debug, Default, Clone)]
pub struct InlineAssets {
    css: HashMap<String, String>,
    js: HashMap<String, String>,
}

impl InlineAssets {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a stylesheet. `href` must match the attribute as written in
    /// the markup — no URL resolution is attempted, because the generator
    /// writes both sides and a mismatch is a typo rather than a case to
    /// handle.
    pub fn css(mut self, href: impl Into<String>, source: &str) -> Result<Self> {
        let href = href.into();
        let code = minify_css(source, &href)?;
        self.css.insert(href, code);
        Ok(self)
    }

    /// Registers a script, keyed by its `src` attribute.
    pub fn js(mut self, src: impl Into<String>, source: &str) -> Result<Self> {
        let src = src.into();
        let code = minify_js(source, &src)?;
        self.js.insert(src, code);
        Ok(self)
    }

    pub fn is_empty(&self) -> bool {
        self.css.is_empty() && self.js.is_empty()
    }
}

/// Replaces `<link rel="stylesheet">` and `<script src>` tags with the
/// registered contents, inline.
///
/// Only tags whose URL is in `assets` are touched. Anything else — a favicon
/// link, a preload hint, an analytics script on someone else's domain — is
/// copied through byte for byte, so this is safe to run over a whole document
/// without enumerating what it contains.
///
/// Attributes are dropped on the way in, with two exceptions that still mean
/// something once the content is inline: `media` on a stylesheet, and `type`
/// on a script, which decides whether the script is a module (and therefore
/// still deferred). `defer` and `async` are *not* kept, because they have no
/// effect on an inline classic script — that is a real change in execution
/// order, and the reason inlining is a per-asset decision.
pub fn inline_into_html(html: &str, assets: &InlineAssets) -> String {
    if assets.is_empty() {
        return html.to_string();
    }

    let mut out = String::with_capacity(html.len());
    let mut i = 0;
    while let Some(offset) = html[i..].find('<') {
        let start = i + offset;
        out.push_str(&html[i..start]);
        let rest = &html[start..];

        match inline_link(rest, assets).or_else(|| inline_script(rest, assets)) {
            Some((replacement, consumed)) => {
                out.push_str(&replacement);
                i = start + consumed;
            }
            None => {
                out.push('<');
                i = start + 1;
            }
        }
    }
    out.push_str(&html[i..]);
    out
}

/// Whether `rest` opens the named tag, checking that what follows the name is
/// a boundary so `<scriptish>` does not match `script`.
fn opens_tag(rest: &str, name: &str) -> bool {
    let Some(after) = rest.get(1..1 + name.len()) else { return false };
    if !after.eq_ignore_ascii_case(name) {
        return false;
    }
    match rest.as_bytes().get(1 + name.len()) {
        Some(c) => c.is_ascii_whitespace() || *c == b'>' || *c == b'/',
        None => false,
    }
}

/// The index of the `>` closing the tag that starts at 0, skipping any `>`
/// that sits inside a quoted attribute value.
fn tag_end(rest: &str) -> Option<usize> {
    let bytes = rest.as_bytes();
    let mut quote = 0u8;
    for (i, &c) in bytes.iter().enumerate() {
        match quote {
            0 if c == b'"' || c == b'\'' => quote = c,
            0 if c == b'>' => return Some(i),
            q if c == q => quote = 0,
            _ => {}
        }
    }
    None
}

/// The value of `want` in a tag's text, or `None` if the attribute is absent.
/// A valueless attribute (`defer`) reports as an empty string, which is what
/// HTML says it means.
fn attr<'a>(tag: &'a str, want: &str) -> Option<&'a str> {
    let bytes = tag.as_bytes();
    let mut i = 0;
    while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b'/') {
            i += 1;
        }
        let name_start = i;
        while i < bytes.len()
            && !bytes[i].is_ascii_whitespace()
            && bytes[i] != b'='
            && bytes[i] != b'/'
        {
            i += 1;
        }
        if i == name_start {
            return None;
        }
        let name = &tag[name_start..i];

        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value = "";
        if bytes.get(i) == Some(&b'=') {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            match bytes.get(i) {
                Some(&q @ (b'"' | b'\'')) => {
                    i += 1;
                    let value_start = i;
                    while i < bytes.len() && bytes[i] != q {
                        i += 1;
                    }
                    value = &tag[value_start..i];
                    i += 1;
                }
                _ => {
                    let value_start = i;
                    while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                        i += 1;
                    }
                    value = &tag[value_start..i];
                }
            }
        }

        if name.eq_ignore_ascii_case(want) {
            return Some(value);
        }
    }
    None
}

/// `rel` is a space-separated token list, so `rel="preload stylesheet"` has to
/// match while `rel="stylesheetish"` must not.
fn has_rel(tag: &str, want: &str) -> bool {
    attr(tag, "rel")
        .is_some_and(|rel| rel.split_ascii_whitespace().any(|t| t.eq_ignore_ascii_case(want)))
}

/// Case-insensitive substring search, so `</SCRIPT>` closes a script.
fn find_ignoring_case(haystack: &str, needle: &str, from: usize) -> Option<usize> {
    let hay = haystack.as_bytes();
    let pat = needle.as_bytes();
    if pat.len() > hay.len() {
        return None;
    }
    (from..=hay.len() - pat.len()).find(|&i| hay[i..i + pat.len()].eq_ignore_ascii_case(pat))
}

fn inline_link(rest: &str, assets: &InlineAssets) -> Option<(String, usize)> {
    if !opens_tag(rest, "link") {
        return None;
    }
    let end = tag_end(rest)?;
    let tag = &rest[1..end];
    if !has_rel(tag, "stylesheet") {
        return None;
    }
    let css = assets.css.get(attr(tag, "href")?)?;

    let media = match attr(tag, "media") {
        Some(m) if !m.is_empty() => format!(" media=\"{m}\""),
        _ => String::new(),
    };
    Some((format!("<style{media}>{css}</style>"), end + 1))
}

fn inline_script(rest: &str, assets: &InlineAssets) -> Option<(String, usize)> {
    if !opens_tag(rest, "script") {
        return None;
    }
    let end = tag_end(rest)?;
    let tag = &rest[1..end];
    let js = assets.js.get(attr(tag, "src")?)?;

    // A `src` script ignores whatever sits between its tags, but the close tag
    // still has to be consumed or it would be left dangling after the
    // replacement.
    let close = find_ignoring_case(rest, "</script", end + 1)?;
    let close_end = rest[close..].find('>')? + close + 1;

    let ty = match attr(tag, "type") {
        Some(t) if !t.is_empty() => format!(" type=\"{t}\""),
        _ => String::new(),
    };
    Some((format!("<script{ty}>{js}</script>"), close_end))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CSS: &str = r#"
        /* a comment that should not survive */
        .button {
            color: #ff0000;
            margin-top: 10px;
            margin-right: 10px;
            margin-bottom: 10px;
            margin-left: 10px;
        }
        .button {
            background-color: rgb(0, 0, 255);
        }
    "#;

    #[test]
    fn css_shrinks_and_keeps_its_meaning() {
        let out = minify_css(CSS, "app.css").unwrap();
        assert!(out.len() < CSS.len() / 2, "{out}");
        assert!(!out.contains("a comment"));
        // Longhands merged, colours shortened: the structural pass ran, not
        // just whitespace removal.
        assert!(out.contains("margin:10px"), "{out}");
        assert!(out.contains("red") || out.contains("#f00"), "{out}");
        // Still parseable, which is the real check that nothing was mangled.
        assert_eq!(minify_css(&out, "app.css").unwrap(), out);
    }

    #[test]
    fn css_syntax_error_is_an_error_naming_the_file() {
        let err = minify_css("@media (", "broken.css").unwrap_err().to_string();
        assert!(err.contains("broken.css"), "{err}");
    }

    #[test]
    fn js_minifies() {
        let source = "
            // dropped
            export function greet(nameOfPerson) {
                const greeting = 'hello ' + nameOfPerson;
                return greeting;
            }
        ";
        let out = minify_js(source, "app.js").unwrap();
        assert!(out.len() < source.len() / 2, "{out}");
        assert!(!out.contains("dropped"));
        assert!(!out.contains("nameOfPerson"), "locals should be mangled: {out}");
        // The export is the module's contract and must keep its name.
        assert!(out.contains("greet"), "{out}");
    }

    #[test]
    fn js_constant_folds() {
        let out = minify_js("console.log(1 + 2);", "app.js").unwrap();
        assert_eq!(out, "console.log(3);");
    }

    #[test]
    fn js_syntax_error_is_an_error_rather_than_a_panic() {
        let err = minify_js("function (", "broken.js").unwrap_err().to_string();
        assert!(err.contains("broken.js"), "{err}");

        // oxc recovers from this one and still yields an AST; it must be
        // refused anyway rather than silently minified into something else.
        assert!(minify_js("const = 1;", "broken.js").is_err());
    }

    #[test]
    fn js_module_syntax_in_an_unknown_extension_still_parses() {
        assert!(minify_js("export const a = 1;", "inline-asset").is_ok());
    }

    fn assets() -> InlineAssets {
        InlineAssets::new()
            .css("/app.css", ".a{color:#ff0000}")
            .unwrap()
            .js("/nav.js", "export const ready = true;")
            .unwrap()
    }

    #[test]
    fn inlining_replaces_known_tags_and_leaves_the_rest_alone() {
        let html = concat!(
            "<head>",
            r#"<link rel="icon" href="/favicon.ico">"#,
            r#"<link rel="stylesheet" href="/app.css">"#,
            r#"<link rel="stylesheet" href="/vendor.css">"#,
            r#"<script src="/nav.js"></script>"#,
            r#"<script src="https://example.com/a.js"></script>"#,
            "</head><body><p>a < b</p></body>",
        );
        let out = inline_into_html(html, &assets());

        assert!(out.contains("<style>.a{color:red}</style>"), "{out}");
        assert!(out.contains("<script>"), "{out}");
        assert!(!out.contains(r#"href="/app.css""#), "{out}");
        assert!(!out.contains(r#"src="/nav.js""#), "{out}");

        // Untouched: a non-stylesheet rel, an unregistered URL, a third-party
        // script, and text that merely looks like markup.
        assert!(out.contains(r#"<link rel="icon" href="/favicon.ico">"#), "{out}");
        assert!(out.contains(r#"<link rel="stylesheet" href="/vendor.css">"#), "{out}");
        assert!(out.contains(r#"<script src="https://example.com/a.js"></script>"#), "{out}");
        assert!(out.contains("<p>a < b</p>"), "{out}");
    }

    #[test]
    fn inlining_keeps_the_attributes_that_still_mean_something() {
        let html = concat!(
            r#"<link rel="stylesheet" media="print" href="/app.css">"#,
            r#"<script type="module" defer src="/nav.js"></script>"#,
        );
        let out = inline_into_html(html, &assets());
        assert!(out.contains(r#"<style media="print">"#), "{out}");
        assert!(out.contains(r#"<script type="module">"#), "{out}");
        assert!(!out.contains("defer"), "{out}");
    }

    #[test]
    fn inlining_tolerates_markup_shapes_it_did_not_write() {
        // Uppercase tags, single quotes, attribute order, a `>` inside a
        // quoted value, and a mixed-case close tag.
        let html = concat!(
            "<LINK HREF='/app.css' REL='stylesheet' title='a > b'>",
            "<SCRIPT SRC='/nav.js'></SCRIPT>",
        );
        let out = inline_into_html(html, &assets());
        assert!(out.starts_with("<style>.a{color:red}</style>"), "{out}");
        assert!(out.ends_with("</script>"), "{out}");
        assert!(!out.contains("SRC="), "{out}");
    }

    #[test]
    fn a_script_without_a_close_tag_is_left_alone() {
        let html = r#"<script src="/nav.js">"#;
        assert_eq!(inline_into_html(html, &assets()), html);
    }

    #[test]
    fn an_empty_asset_set_is_a_passthrough() {
        let html = r#"<link rel="stylesheet" href="/app.css">"#;
        assert_eq!(inline_into_html(html, &InlineAssets::new()), html);
    }
}
