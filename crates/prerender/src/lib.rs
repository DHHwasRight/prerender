//! A static site generator for prerendered partial navigation.
//!
//! Every route is built more than once: as a full document, and as the content
//! below the root layout. A navigation with JavaScript fetches the second form
//! and swaps it in; a navigation without JavaScript, or a crawler, gets the
//! first. Neither is derived from the other at runtime, so the server stays a
//! static file server.
//!
//! Templates come from [`rsx`], which compiles them into the binary, so a site
//! is a Rust crate rather than a directory of files an interpreter walks. That
//! makes this an unusual shape for a static site generator: the "CLI" is the
//! site's own binary, and [`run`] gives it `build` and `nginx` subcommands.
//!
//! ```ignore
//! fn main() -> std::process::ExitCode {
//!     prerender::run(
//!         prerender::Site::new()
//!             .layout("/", |ctx| views::layouts::root(ctx))
//!             .layout("/docs", |ctx| views::layouts::docs(ctx))
//!             .route("/", || views::pages::home())
//!             .route("/docs", || views::pages::docs_index()),
//!     )
//! }
//! ```
//!
//! ## Layouts
//!
//! A layout is registered against a path prefix and wraps every route beneath
//! it. Rendering folds from the innermost layout outward, which is what makes
//! a partial cheap to produce: it is the same fold, started one layer in.
//!
//! Layouts must mark themselves in the markup with the prefix they were
//! registered under:
//!
//! ```ignore
//! pub fn Docs(layout: &str, children: Element) -> Element {
//!     <div dp-layout={layout} class="docs">{children}</div>
//! }
//! ```
//!
//! The client uses those markers to find the deepest layout common to the old
//! and new page, and replaces only what is below it. That is why navigating
//! between two pages in the same section swaps just the page body, while
//! arriving from outside the section swaps the whole section. [`check`] fails
//! the build when a layout forgets its marker, because the failure is
//! otherwise silent and only shows up as the wrong thing animating.

use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use walkdir::WalkDir;

pub mod assets;
pub mod content;

pub use content::{Doc, FrontMatter};
pub use rsx;

/// Rendered markup. Layouts receive and return it, so a layer can be rendered
/// without knowing the concrete type of what it wraps.
pub type Html = String;

/// What a layout is handed: the prefix it was registered under, and the
/// already-rendered markup it must wrap.
pub struct LayoutCtx<'a> {
    /// The registered prefix, e.g. `/docs/guides`. Put it on the root element
    /// as `dp-layout` so the client can match layers across a navigation.
    pub prefix: &'a str,
    /// Everything this layout wraps, already rendered.
    pub children: Html,
    /// The path of the page being rendered, for navigation highlighting.
    pub path: &'a str,
}

type LayoutFn = Box<dyn Fn(LayoutCtx<'_>) -> rsx::Result<Html>>;
type RouteFn = Box<dyn Fn() -> rsx::Result<Html>>;

struct LayoutDef {
    prefix: String,
    segments: Vec<String>,
    render: LayoutFn,
    /// Markup to send when this layout is *skipped*, so the parts of it that
    /// depend on the current page can still be refreshed. See
    /// [`Site::layout_oob`].
    oob: Option<LayoutFn>,
}

struct RouteDef {
    path: String,
    segments: Vec<String>,
    render: RouteFn,
}

/// Splits a URL path into non-empty segments. `/docs/guides/` and
/// `/docs/guides` are the same route.
fn segments(path: &str) -> Vec<String> {
    path.split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Whether `prefix` covers `path`, compared segment by segment so that
/// `/docsfoo` is not considered to be inside `/docs`.
fn covers(prefix: &[String], path: &[String]) -> bool {
    prefix.len() <= path.len() && prefix.iter().zip(path).all(|(a, b)| a == b)
}

#[derive(Default)]
pub struct Site {
    layouts: Vec<LayoutDef>,
    routes: Vec<RouteDef>,
    statics: Vec<PathBuf>,
    generated: Vec<(String, String)>,
    redirects: Vec<(String, String)>,
}

impl Site {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a layout for every route at or below `prefix`. Order of
    /// registration does not matter; layouts are applied shallowest first.
    pub fn layout<F>(mut self, prefix: &str, render: F) -> Self
    where
        F: Fn(LayoutCtx<'_>) -> rsx::Result<Html> + 'static,
    {
        self.layouts.push(LayoutDef {
            prefix: prefix.to_string(),
            segments: segments(prefix),
            render: Box::new(render),
            oob: None,
        });
        self
    }

    /// A layout that also contributes out-of-band markup when it is skipped.
    ///
    /// A partial starts below the layers the client already has, so those
    /// layers are not re-sent — which is the point, and also the problem: a
    /// skipped layout's own chrome may depend on the page being rendered. A
    /// tab strip outside the swapped region keeps highlighting the tab you
    /// just navigated away from.
    ///
    /// `oob` renders replacements for exactly those parts. Each element it
    /// returns must carry `dp-swap-oob` and an `id` matching the element it
    /// replaces; the client applies them before the main swap. The closure is
    /// called only when this layout is skipped, so the full document and any
    /// partial that still contains the layout are unaffected.
    pub fn layout_oob<F, G>(mut self, prefix: &str, render: F, oob: G) -> Self
    where
        F: Fn(LayoutCtx<'_>) -> rsx::Result<Html> + 'static,
        G: Fn(LayoutCtx<'_>) -> rsx::Result<Html> + 'static,
    {
        self.layouts.push(LayoutDef {
            prefix: prefix.to_string(),
            segments: segments(prefix),
            render: Box::new(render),
            oob: Some(Box::new(oob)),
        });
        self
    }

    /// Copies a directory of assets into the full tree.
    ///
    /// Only the full tree: a stylesheet is requested without the partial
    /// header, so it resolves against the default root. Duplicating assets
    /// into the partial tree would only create a second copy nothing asks for.
    pub fn static_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.statics.push(dir.into());
        self
    }

    /// Sends one path to another with a 301.
    ///
    /// A section prefix such as `/guide/plans/charges` is a path segment with
    /// no page of its own, yet content links to it as though it were one. The
    /// alternative to a redirect is building an index page nobody wrote, which
    /// then has to be kept in step with whatever it was supposed to summarise.
    pub fn redirect(mut self, from: impl Into<String>, to: impl Into<String>) -> Self {
        self.redirects.push((from.into(), to.into()));
        self
    }

    /// Every redirect, for a caller that wants to check them.
    pub fn redirects(&self) -> &[(String, String)] {
        &self.redirects
    }

    /// Writes a generated file into the full tree.
    ///
    /// For assets that are produced rather than authored — a syntax-highlight
    /// stylesheet derived from the theme, a sitemap — which would otherwise
    /// have to be committed and kept in sync by hand.
    pub fn static_file(mut self, path: impl Into<String>, contents: impl Into<String>) -> Self {
        self.generated.push((path.into(), contents.into()));
        self
    }

    /// Registers a page. The closure renders the page body only; layouts are
    /// applied around it.
    pub fn route<F>(mut self, path: &str, render: F) -> Self
    where
        F: Fn() -> rsx::Result<Html> + 'static,
    {
        self.routes.push(RouteDef {
            path: path.to_string(),
            segments: segments(path),
            render: Box::new(render),
        });
        self
    }

    /// The layouts wrapping `path`, shallowest first.
    fn chain_for(&self, path: &[String]) -> Vec<&LayoutDef> {
        let mut chain: Vec<&LayoutDef> = self
            .layouts
            .iter()
            .filter(|l| covers(&l.segments, path))
            .collect();
        chain.sort_by_key(|l| l.segments.len());
        chain
    }

    /// Renders one route, applying every layout from `skip` inward.
    ///
    /// `skip = 0` is the full document. `skip = 1` drops the root layout,
    /// which is the partial: the same markup a full page would have inside
    /// `<body>`, so the client can swap it in without rewriting anything.
    fn render(&self, route: &RouteDef, skip: usize) -> rsx::Result<Html> {
        let chain = self.chain_for(&route.segments);
        let skip = skip.min(chain.len());
        let mut html = (route.render)()?;
        for layout in chain.iter().skip(skip).rev() {
            html = (layout.render)(LayoutCtx {
                prefix: &layout.prefix,
                children: html,
                path: &route.path,
            })?;
        }

        // Only the deepest skipped layout contributes. Anything above it is
        // shared with the destination at a level that pins its own chrome: if
        // the section layer is shared then the section has not changed, so the
        // sidebar's highlight cannot have moved and re-sending it is waste.
        // The deepest shared layer is the only one whose chrome depends on
        // something that did change.
        let mut oob = String::new();
        if let Some(layout) = skip.checked_sub(1).and_then(|i| chain.get(i)) {
            if let Some(render_oob) = &layout.oob {
                oob.push_str(&render_oob(LayoutCtx {
                    prefix: &layout.prefix,
                    children: String::new(),
                    path: &route.path,
                })?);
            }
        }
        Ok(oob + &html)
    }

    /// The deepest layout chain of any route, which is how many partial trees
    /// are worth building.
    fn max_depth(&self) -> usize {
        self.routes
            .iter()
            .map(|route| self.chain_for(&route.segments).len())
            .max()
            .unwrap_or(1)
    }

    /// Writes both trees. Returns what was produced, for the caller to report.
    pub fn build(&self, out: &Path, partial_out: &Path) -> io::Result<Report> {
        self.build_with(out, Some(partial_out))
    }

    /// Writes the documents, and the partial trees only when `partial_out` is
    /// given.
    ///
    /// Passing `None` produces an ordinary static site. Partial navigation
    /// needs a server that can route on a request header, and some hosts --
    /// GitHub Pages among them -- have no such thing. There the partial trees
    /// are bytes nothing can ever ask for, and the pages still work, because
    /// every one of them is a complete document by construction.
    pub fn build_with(&self, out: &Path, partial_out: Option<&Path>) -> io::Result<Report> {
        let mut report = Report::default();
        let depths = if partial_out.is_some() {
            self.max_depth()
        } else {
            0
        };
        for route in &self.routes {
            let full = self
                .render(route, 0)
                .map_err(|e| io::Error::other(format!("{}: {e}", route.path)))?;
            write_page(out, &route.segments, &full)?;
            report.pages += 1;
            report.full_bytes += full.len();

            // One tree per number of layers the client may already hold. A
            // client three layers in should not be sent the two it is keeping.
            if let Some(partial_out) = partial_out {
                for depth in 1..=depths {
                    let partial = self.render(route, depth).map_err(|e| {
                        io::Error::other(format!("{} (partial {depth}): {e}", route.path))
                    })?;
                    write_page(
                        &partial_out.join(depth.to_string()),
                        &route.segments,
                        &partial,
                    )?;
                    if depth == 1 {
                        report.partial_bytes += partial.len();
                    }
                }
            }
        }
        report.depths = depths;
        for dir in &self.statics {
            report.assets += copy_dir(dir, out)?;
        }
        for (path, contents) in &self.generated {
            let target = out.join(path.trim_start_matches('/'));
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(target, contents)?;
            report.assets += 1;
        }
        Ok(report)
    }

    /// Fails the build when a layout does not mark itself with `dp-layout`.
    ///
    /// Without the marker the client cannot tell which layers two pages share,
    /// so it silently replaces more of the document than it should — which
    /// looks like a working site with the wrong thing animating.
    pub fn check(&self) -> Result<(), Vec<String>> {
        let mut problems = Vec::new();
        for layout in &self.layouts {
            let probe = (layout.render)(LayoutCtx {
                prefix: &layout.prefix,
                children: "<!--children-->".into(),
                path: &layout.prefix,
            });
            match probe {
                Err(e) => problems.push(format!("layout {} failed to render: {e}", layout.prefix)),
                Ok(html) => {
                    let marker = format!("dp-layout=\"{}\"", layout.prefix);
                    if !html.contains(&marker) {
                        problems.push(format!(
                            "layout {} does not emit {marker} — the client cannot match it",
                            layout.prefix
                        ));
                    }
                    if !html.contains("<!--children-->") {
                        problems.push(format!("layout {} dropped its children", layout.prefix));
                    }
                }
            }
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems)
        }
    }

    /// A complete, runnable nginx config serving both trees.
    ///
    /// Complete rather than a snippet, because the point is to be able to run
    /// it: `nginx -p <prefix> -c <file>` and the site is up.
    pub fn nginx_conf(&self, cfg: NginxConf<'_>) -> String {
        let var = format!("$http_{}", cfg.header.to_lowercase().replace('-', "_"));
        let mut s = String::new();
        let _ = write!(
            s,
            "# Generated by prerender.\n\
             #\n\
             # Both trees hold the same routes. A request carrying `{header}`\n\
             # is served from the partial tree; everything else -- a crawler, a\n\
             # browser with JavaScript off, the first load of any page -- is\n\
             # served the full document. That is the whole mechanism.\n\
             #\n\
             #   nginx -p {prefix} -c {prefix}/nginx.conf\n\
             \n\
             worker_processes auto;\n\
             daemon           off;\n\
             pid              {prefix}/nginx.pid;\n\
             error_log        {prefix}/error.log warn;\n\
             \n\
             events {{ worker_connections 1024; }}\n\
             \n\
             http {{\n\
             \x20   include      {mime};\n\
             \x20   default_type application/octet-stream;\n\
             \x20   access_log   off;\n\
             \n\
             \x20   # Measured, not assumed: caching the open descriptor and its\n\
             \x20   # stat is worth 45-110% on a tree of small files, because\n\
             \x20   # otherwise every request pays an open() and an fstat().\n\
             \x20   open_file_cache          max=10000 inactive=5m;\n\
             \x20   open_file_cache_valid    2m;\n\
             \x20   open_file_cache_min_uses 1;\n\
             \x20   open_file_cache_errors   on;\n\
             \n\
             \x20   # sendfile is a size tradeoff rather than a win: it cannot\n\
             \x20   # coalesce the header with the body, so below roughly 100 KiB\n\
             \x20   # it loses to one writev, and above it wins by a lot. Pages\n\
             \x20   # and assets here are smaller than that. Turn it on if you\n\
             \x20   # start serving large downloads from this root.\n\
             \x20   sendfile off;\n\
             \n\
             \x20   gzip            on;\n\
             \x20   gzip_vary       on;\n\
             \x20   gzip_min_length 1024;\n\
             \x20   gzip_types      text/css text/javascript application/javascript\n\
             \x20                   application/json image/svg+xml;\n\
             \n\
             \x20   client_body_temp_path {prefix}/tmp;\n\
             \x20   proxy_temp_path       {prefix}/tmp-proxy;\n\
             \x20   fastcgi_temp_path     {prefix}/tmp-fastcgi;\n\
             \x20   uwsgi_temp_path       {prefix}/tmp-uwsgi;\n\
             \x20   scgi_temp_path        {prefix}/tmp-scgi;\n\
             \n\
             \x20   # Case-insensitive because a header value is not the\n\
             \x20   # client's to normalise.\n\
             \x20   map {var} $prerender_root {{\n\
             \x20       default   {full};\n\
             {depth_map}\
             \x20   }}\n\
             \n\
             \x20   server {{\n\
             \x20       listen {port};\n\
             \x20       root   $prerender_root;\n\
             {redirects}\
             \n\
             \x20       # Assets are built once, not twice, so they never vary on\n\
             \x20       # the header and are always read from the full tree.\n\
             \x20       # Keeping Vary off them also keeps them cacheable as one\n\
             \x20       # object rather than two.\n\
             \x20       location ~* \\.(css|js|mjs|svg|png|jpe?g|gif|webp|avif|ico|woff2?|ttf|map)$ {{\n\
             \x20           root {full};\n\
             \x20           add_header Cache-Control \"public, max-age=3600\" always;\n\
             \x20           try_files $uri =404;\n\
             \x20       }}\n\
             \n\
             \x20       location / {{\n\
             \x20           # Not optional. Two different bodies share one URL, so\n\
             \x20           # a cache that saw a fragment will otherwise hand it to\n\
             \x20           # the next visitor asking for a document.\n\
             \x20           add_header Vary {header} always;\n\
             \x20           # The trees are rewritten on every deploy and nothing\n\
             \x20           # is content-hashed, so revalidating is the honest\n\
             \x20           # default for HTML.\n\
             \x20           add_header Cache-Control \"no-cache\" always;\n\
             \x20           try_files $uri $uri/index.html @full;\n\
             \x20       }}\n\
             \n\
             \x20       # Anything the partial tree does not hold falls back to the\n\
             \x20       # full one, so a partial request for a path that was never\n\
             \x20       # built twice still resolves instead of 404ing.\n\
             \x20       #\n\
             \x20       # The 404 page is deliberately NOT a try_files candidate.\n\
             \x20       # Listing it there serves it with status 200, so a client\n\
             \x20       # asking for a fragment is handed a whole document and\n\
             \x20       # swaps it into the middle of the page it is showing --\n\
             \x20       # which looks like the header appearing twice. Falling\n\
             \x20       # through to =404 lets error_page answer, which keeps the\n\
             \x20       # status and resolves against the tree that was asked for.\n\
             \x20       location @full {{\n\
             \x20           root {full};\n\
             \x20           add_header Vary {header} always;\n\
             \x20           add_header Cache-Control \"no-cache\" always;\n\
             \x20           try_files $uri $uri/index.html =404;\n\
             \x20       }}\n\
             \n\
             \x20       # A 404 answered to a partial request must itself be a\n\
             \x20       # partial, or the client swaps a whole document into the\n\
             \x20       # middle of the page it is already showing.\n\
             \x20       error_page 404 /404/index.html;\n\
             \x20       location = /404/index.html {{\n\
             \x20           internal;\n\
             \x20           add_header Vary {header} always;\n\
             \x20       }}\n\
             \x20   }}\n\
             }}\n",
            header = cfg.header,
            var = var,
            prefix = cfg.prefix.display(),
            mime = cfg.mime_types.display(),
            port = cfg.port,
            full = cfg.full.display(),
            redirects = self.redirect_block(),
            depth_map = self.depth_map(cfg.partial),
        );
        s
    }

    /// One root per depth the client may ask for.
    ///
    /// The header carries how many layers the client already has, so `3` means
    /// "send me what sits below the third". `true` is kept as an alias for one
    /// level, which is what a client that does not count layers sends.
    fn depth_map(&self, partial: &Path) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "        ~*^true$  {};", partial.join("1").display());
        for depth in 1..=self.max_depth() {
            let _ = writeln!(
                out,
                "        \"{depth}\"       {};",
                partial.join(depth.to_string()).display()
            );
        }
        out
    }

    /// Exact-match redirect locations, which nginx resolves before any prefix
    /// location, so they cost nothing on the pages that are not redirects.
    fn redirect_block(&self) -> String {
        if self.redirects.is_empty() {
            return String::new();
        }
        let mut out = String::from(
            "\n        # Section prefixes that content links to but that have no page.\n",
        );
        for (from, to) in &self.redirects {
            let _ = writeln!(out, "        location = {from} {{ return 301 {to}; }}");
            // Both spellings, since a link may or may not carry the slash.
            let _ = writeln!(out, "        location = {from}/ {{ return 301 {to}; }}");
        }
        out
    }
}

/// Everything the generated nginx config needs that the site does not know.
pub struct NginxConf<'a> {
    pub full: &'a Path,
    pub partial: &'a Path,
    /// Where nginx keeps its pid, logs and temp dirs.
    pub prefix: &'a Path,
    pub mime_types: &'a Path,
    pub header: &'a str,
    pub port: u16,
}

fn write_page(root: &Path, segs: &[String], html: &str) -> io::Result<()> {
    let mut path = root.to_path_buf();
    for seg in segs {
        path.push(seg);
    }
    fs::create_dir_all(&path)?;
    path.push("index.html");
    fs::write(path, html)
}

/// Mirrors `src` into `dst`, returning how many files were copied.
///
/// Empty directories are recreated too: an asset tree is served as-is, and a
/// missing directory is a different site than an empty one.
fn copy_dir(src: &Path, dst: &Path) -> io::Result<usize> {
    let mut copied = 0;
    for entry in WalkDir::new(src) {
        let entry = entry?;
        // Every entry sits under `src`, so the relative path is the position
        // it should take under `dst`.
        let target = dst.join(entry.path().strip_prefix(src).unwrap_or(entry.path()));
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target)?;
        } else {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(entry.path(), &target)?;
            copied += 1;
        }
    }
    Ok(copied)
}

#[derive(Default)]
pub struct Report {
    pub pages: usize,
    pub full_bytes: usize,
    pub partial_bytes: usize,
    pub assets: usize,
    /// How many partial trees were written, one per layout depth.
    pub depths: usize,
}

/// Locates nginx's own mime.types, so the generated config does not hardcode a
/// path that is wrong on every machine but the one it was written on.
fn nginx_mime_types() -> String {
    let out = std::process::Command::new("nginx").arg("-V").output();
    if let Ok(out) = out {
        let text = String::from_utf8_lossy(&out.stderr);
        if let Some(conf) = text
            .split_whitespace()
            .find_map(|t| t.strip_prefix("--conf-path="))
        {
            if let Some(dir) = Path::new(conf).parent() {
                return dir.join("mime.types").display().to_string();
            }
        }
    }
    "/etc/nginx/mime.types".to_string()
}

/// The site binary's command line.
#[derive(Parser)]
#[command(
    name = "<site>",
    about = "Render a prerendered site and serve it from nginx."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,
    // Global, so that both `<site> --out dir` and `<site> build --out dir`
    // parse: the subcommand is optional, and an option must not change meaning
    // depending on whether the user spelled the default one out.
    #[command(flatten)]
    opts: Opts,
}

#[derive(Subcommand)]
enum Cmd {
    /// Render both trees.
    Build,
    /// Print an nginx config for the built trees.
    Nginx,
    /// Verify every layout marks itself with dp-layout.
    Check,
}

#[derive(Args)]
struct Opts {
    /// Where the full documents go.
    #[arg(long, global = true, value_name = "DIR", default_value = "dist")]
    out: PathBuf,
    /// Where the partials go.
    #[arg(
        long,
        global = true,
        value_name = "DIR",
        default_value = "dist-partial"
    )]
    partial_out: PathBuf,
    /// Request header that selects the partial tree.
    #[arg(long, global = true, value_name = "NAME", default_value = "DP-Partial")]
    header: String,
    /// nginx runtime dir, for its pid, logs and temp dirs.
    #[arg(long, global = true, value_name = "DIR", default_value = ".run")]
    prefix: PathBuf,
    /// nginx listen port.
    #[arg(long, global = true, value_name = "PORT", default_value_t = 8080)]
    port: u16,
    /// Build documents only, for a host that cannot route on a header.
    ///
    /// Partial navigation degrades to ordinary page loads; nothing else
    /// changes, because every page is already a complete document.
    #[arg(long, global = true)]
    no_partials: bool,
    /// nginx mime.types [default: found from `nginx -V`]
    ///
    /// Resolved after parsing rather than as a clap default, so that merely
    /// printing `--help` does not shell out to nginx.
    #[arg(long, global = true, value_name = "FILE")]
    mime_types: Option<String>,
}

/// Argument parsing and the three subcommands, so a site's `main` is a call.
pub fn run(site: Site) -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            let _ = e.print();
            // `--help` and `--version` arrive here too, and asking for them is
            // not a failure.
            return if e.use_stderr() {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            };
        }
    };
    let Opts {
        out,
        partial_out,
        header,
        prefix,
        port,
        no_partials,
        mime_types,
    } = cli.opts;
    let mime_types = mime_types.unwrap_or_else(nginx_mime_types);

    match cli.command.unwrap_or(Cmd::Build) {
        Cmd::Check => match site.check() {
            Ok(()) => {
                println!("layouts ok");
                ExitCode::SUCCESS
            }
            Err(problems) => {
                for p in problems {
                    eprintln!("error: {p}");
                }
                ExitCode::FAILURE
            }
        },
        Cmd::Nginx if no_partials => {
            eprintln!(
                "error: --no-partials builds one tree; the generated config exists to \n\
                 choose between two. Serve {} with any static file server.",
                out.display()
            );
            ExitCode::FAILURE
        }
        Cmd::Nginx => {
            let abs = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
            print!(
                "{}",
                site.nginx_conf(NginxConf {
                    full: &abs(&out),
                    partial: &abs(&partial_out),
                    prefix: &abs(&prefix),
                    mime_types: Path::new(&mime_types),
                    header: &header,
                    port,
                })
            );
            ExitCode::SUCCESS
        }
        Cmd::Build => {
            // A layout without its marker builds a site that looks right and
            // behaves wrong, so this is checked rather than documented.
            if let Err(problems) = site.check() {
                for p in problems {
                    eprintln!("error: {p}");
                }
                return ExitCode::FAILURE;
            }
            let partial = (!no_partials).then_some(partial_out.as_path());
            match site.build_with(&out, partial) {
                Ok(report) => {
                    print!(
                        "built {} pages, {} assets\n  {:>10} bytes  {}\n",
                        report.pages,
                        report.assets,
                        report.full_bytes,
                        out.display(),
                    );
                    if report.depths > 0 {
                        println!(
                            "  {:>10} bytes  {}/1  ({} partial depths)",
                            report.partial_bytes,
                            partial_out.display(),
                            report.depths,
                        );
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("build failed: {e}");
                    ExitCode::FAILURE
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covers_compares_segments_not_strings() {
        assert!(covers(&segments("/docs"), &segments("/docs/guides")));
        assert!(!covers(&segments("/docs"), &segments("/docsfoo/x")));
        assert!(covers(&segments("/"), &segments("/anything")));
    }

    #[test]
    fn chain_is_shallowest_first_and_only_matching() {
        let site = Site::new()
            .layout("/docs/guides", |c| Ok(format!("<g>{}</g>", c.children)))
            .layout("/", |c| Ok(format!("<r>{}</r>", c.children)))
            .layout("/docs", |c| Ok(format!("<d>{}</d>", c.children)))
            .layout("/other", |c| Ok(format!("<o>{}</o>", c.children)))
            .route("/docs/guides/x", || Ok("P".into()));

        let route = &site.routes[0];
        let prefixes: Vec<&str> = site
            .chain_for(&route.segments)
            .iter()
            .map(|l| l.prefix.as_str())
            .collect();
        assert_eq!(prefixes, ["/", "/docs", "/docs/guides"]);
        assert_eq!(site.render(route, 0).unwrap(), "<r><d><g>P</g></d></r>");
        // The partial is the same markup with the root layer dropped.
        assert_eq!(site.render(route, 1).unwrap(), "<d><g>P</g></d>");
    }

    #[test]
    fn cli_is_well_formed_and_takes_the_documented_flags() {
        use clap::CommandFactory;
        Cli::command().debug_assert();

        // The subcommand is optional and `build` is what you get without one.
        let cli = Cli::try_parse_from(["site"]).unwrap();
        assert!(cli.command.is_none());
        assert_eq!(cli.opts.out, PathBuf::from("dist"));
        assert_eq!(cli.opts.partial_out, PathBuf::from("dist-partial"));
        assert_eq!(cli.opts.header, "DP-Partial");
        assert_eq!(cli.opts.prefix, PathBuf::from(".run"));
        assert_eq!(cli.opts.port, 8080);
        assert!(cli.opts.mime_types.is_none());

        let cli = Cli::try_parse_from([
            "site",
            "nginx",
            "--out",
            "o",
            "--partial-out",
            "p",
            "--header",
            "X-Partial",
            "--prefix",
            "run",
            "--port",
            "9000",
            "--mime-types",
            "/m",
        ])
        .unwrap();
        assert!(matches!(cli.command, Some(Cmd::Nginx)));
        assert_eq!(cli.opts.out, PathBuf::from("o"));
        assert_eq!(cli.opts.partial_out, PathBuf::from("p"));
        assert_eq!(cli.opts.header, "X-Partial");
        assert_eq!(cli.opts.prefix, PathBuf::from("run"));
        assert_eq!(cli.opts.port, 9000);
        assert_eq!(cli.opts.mime_types.as_deref(), Some("/m"));
    }

    #[test]
    fn check_rejects_an_unmarked_layout() {
        let site = Site::new().layout("/docs", |c| Ok(format!("<div>{}</div>", c.children)));
        assert!(site.check().is_err());
        let site = Site::new().layout("/docs", |c| {
            Ok(format!("<div dp-layout=\"/docs\">{}</div>", c.children))
        });
        assert!(site.check().is_ok());
    }
}

#[cfg(test)]
mod no_partial_tests {
    use super::*;

    fn site() -> Site {
        Site::new()
            .layout("/", |ctx: LayoutCtx<'_>| {
                Ok(format!(
                    "<!DOCTYPE html><html lang=\"en\"><head><title>t</title></head>\
                     <body><main dp-layout=\"{}\">{}</main></body></html>",
                    ctx.prefix, ctx.children
                ))
            })
            .route("/", || Ok("<h1>home</h1>".to_string()))
            .route("/about", || Ok("<h1>about</h1>".to_string()))
    }

    #[test]
    fn no_partials_writes_documents_only() {
        let root = std::env::temp_dir().join("prerender-no-partials");
        let _ = fs::remove_dir_all(&root);
        let out = root.join("dist");
        let partial = root.join("dist-partial");

        let report = site().build_with(&out, None).expect("build");

        assert_eq!(report.pages, 2);
        assert_eq!(report.depths, 0, "no partial trees were asked for");
        assert_eq!(report.partial_bytes, 0);
        assert!(out.join("index.html").is_file());
        assert!(out.join("about/index.html").is_file());
        assert!(
            !partial.exists(),
            "nothing should be written beside the documents"
        );

        // The pages are unchanged: a host with no header routing still gets
        // a whole site, which is the point of the switch.
        let home = fs::read_to_string(out.join("index.html")).expect("read");
        assert!(home.starts_with("<!DOCTYPE html>"), "{home}");
        assert!(home.contains("<h1>home</h1>"), "{home}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn the_documents_are_byte_identical_either_way() {
        // The switch must not be a second rendering path, or the site people
        // publish stops being the site that was tested.
        let root = std::env::temp_dir().join("prerender-both-ways");
        let _ = fs::remove_dir_all(&root);
        let (a, b, p) = (root.join("a"), root.join("b"), root.join("p"));

        site().build_with(&a, None).expect("build a");
        site().build_with(&b, Some(&p)).expect("build b");

        for page in ["index.html", "about/index.html"] {
            assert_eq!(
                fs::read_to_string(a.join(page)).expect("a"),
                fs::read_to_string(b.join(page)).expect("b"),
                "{page} differs between the two modes"
            );
        }
        let _ = fs::remove_dir_all(&root);
    }
}
