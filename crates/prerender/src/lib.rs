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
}

struct RouteDef {
    path: String,
    segments: Vec<String>,
    render: RouteFn,
}

/// Splits a URL path into non-empty segments. `/docs/guides/` and
/// `/docs/guides` are the same route.
fn segments(path: &str) -> Vec<String> {
    path.split('/').filter(|s| !s.is_empty()).map(str::to_string).collect()
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
        let mut chain: Vec<&LayoutDef> =
            self.layouts.iter().filter(|l| covers(&l.segments, path)).collect();
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
        let mut html = (route.render)()?;
        for layout in chain.iter().skip(skip).rev() {
            html = (layout.render)(LayoutCtx {
                prefix: &layout.prefix,
                children: html,
                path: &route.path,
            })?;
        }
        Ok(html)
    }

    /// Writes both trees. Returns what was produced, for the caller to report.
    pub fn build(&self, out: &Path, partial_out: &Path) -> io::Result<Report> {
        let mut report = Report::default();
        for route in &self.routes {
            let full = self
                .render(route, 0)
                .map_err(|e| io::Error::other(format!("{}: {e}", route.path)))?;
            let partial = self
                .render(route, 1)
                .map_err(|e| io::Error::other(format!("{} (partial): {e}", route.path)))?;

            write_page(out, &route.segments, &full)?;
            write_page(partial_out, &route.segments, &partial)?;
            report.pages += 1;
            report.full_bytes += full.len();
            report.partial_bytes += partial.len();
        }
        for dir in &self.statics {
            report.assets += copy_dir(dir, out)?;
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
                        problems.push(format!(
                            "layout {} dropped its children",
                            layout.prefix
                        ));
                    }
                }
            }
        }
        if problems.is_empty() { Ok(()) } else { Err(problems) }
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
             worker_processes 2;\n\
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
             \x20   sendfile     on;\n\
             \n\
             \x20   client_body_temp_path {prefix}/tmp;\n\
             \x20   proxy_temp_path       {prefix}/tmp-proxy;\n\
             \x20   fastcgi_temp_path     {prefix}/tmp-fastcgi;\n\
             \x20   uwsgi_temp_path       {prefix}/tmp-uwsgi;\n\
             \x20   scgi_temp_path        {prefix}/tmp-scgi;\n\
             \n\
             \x20   # The match is case-insensitive because HTTP header values\n\
             \x20   # are not the client's to normalise.\n\
             \x20   map {var} $prerender_root {{\n\
             \x20       default   {full};\n\
             \x20       ~*^true$  {partial};\n\
             \x20   }}\n\
             \n\
             \x20   server {{\n\
             \x20       listen {port};\n\
             \x20       root   $prerender_root;\n\
             \n\
             \x20       # Not optional. Two different bodies share one URL, so a\n\
             \x20       # cache that saw a fragment will otherwise hand it to the\n\
             \x20       # next visitor asking for a document.\n\
             \x20       add_header Vary {header} always;\n\
             \n\
             \x20       location / {{\n\
             \x20           try_files $uri $uri/index.html =404;\n\
             \x20       }}\n\
             \x20   }}\n\
             }}\n",
            header = cfg.header,
            var = var,
            prefix = cfg.prefix.display(),
            mime = cfg.mime_types.display(),
            port = cfg.port,
            full = cfg.full.display(),
            partial = cfg.partial.display(),
        );
        s
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

fn copy_dir(src: &Path, dst: &Path) -> io::Result<usize> {
    let mut copied = 0;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let target = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            fs::create_dir_all(&target)?;
            copied += copy_dir(&entry.path(), &target)?;
        } else {
            fs::create_dir_all(dst)?;
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
}

const USAGE: &str = "\
usage: <site> <command> [options]

commands:
  build            render both trees
  nginx            print an nginx config for the built trees
  check            verify every layout marks itself with dp-layout

options:
  --out <DIR>          full documents        [default: dist]
  --partial-out <DIR>  partials              [default: dist-partial]
  --header <NAME>      partial request header [default: DP-Partial]
  --prefix <DIR>       nginx runtime dir      [default: .run]
  --port <PORT>        nginx listen port      [default: 8080]
  --mime-types <FILE>  nginx mime.types       [default: found from `nginx -V`]
";

/// Locates nginx's own mime.types, so the generated config does not hardcode a
/// path that is wrong on every machine but the one it was written on.
fn nginx_mime_types() -> String {
    let out = std::process::Command::new("nginx").arg("-V").output();
    if let Ok(out) = out {
        let text = String::from_utf8_lossy(&out.stderr);
        if let Some(conf) = text.split_whitespace().find_map(|t| t.strip_prefix("--conf-path=")) {
            if let Some(dir) = Path::new(conf).parent() {
                return dir.join("mime.types").display().to_string();
            }
        }
    }
    "/etc/nginx/mime.types".to_string()
}

/// Argument parsing and the three subcommands, so a site's `main` is a call.
pub fn run(site: Site) -> ExitCode {
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "build".into());
    let mut out = PathBuf::from("dist");
    let mut partial_out = PathBuf::from("dist-partial");
    let mut header = "DP-Partial".to_string();
    let mut prefix = PathBuf::from(".run");
    let mut port: u16 = 8080;
    let mut mime_types = nginx_mime_types();

    while let Some(arg) = args.next() {
        let mut value = || args.next().unwrap_or_default();
        match arg.as_str() {
            "--out" => out = PathBuf::from(value()),
            "--partial-out" => partial_out = PathBuf::from(value()),
            "--header" => header = value(),
            "--prefix" => prefix = PathBuf::from(value()),
            "--mime-types" => mime_types = value(),
            "--port" => port = value().parse().unwrap_or(8080),
            other => {
                eprintln!("unknown option {other:?}\n\n{USAGE}");
                return ExitCode::FAILURE;
            }
        }
    }

    match command.as_str() {
        "check" => match site.check() {
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
        "nginx" => {
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
        "build" => {
            // A layout without its marker builds a site that looks right and
            // behaves wrong, so this is checked rather than documented.
            if let Err(problems) = site.check() {
                for p in problems {
                    eprintln!("error: {p}");
                }
                return ExitCode::FAILURE;
            }
            match site.build(&out, &partial_out) {
                Ok(report) => {
                    println!(
                        "built {} pages, {} assets\n  {:>10} bytes  {}\n  {:>10} bytes  {}",
                        report.pages,
                        report.assets,
                        report.full_bytes,
                        out.display(),
                        report.partial_bytes,
                        partial_out.display(),
                    );
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("build failed: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        other => {
            eprintln!("unknown command {other:?}\n\n{USAGE}");
            ExitCode::FAILURE
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
        let prefixes: Vec<&str> =
            site.chain_for(&route.segments).iter().map(|l| l.prefix.as_str()).collect();
        assert_eq!(prefixes, ["/", "/docs", "/docs/guides"]);
        assert_eq!(site.render(route, 0).unwrap(), "<r><d><g>P</g></d></r>");
        // The partial is the same markup with the root layer dropped.
        assert_eq!(site.render(route, 1).unwrap(), "<d><g>P</g></d>");
    }

    #[test]
    fn check_rejects_an_unmarked_layout() {
        let site = Site::new().layout("/docs", |c| Ok(format!("<div>{}</div>", c.children)));
        assert!(site.check().is_err());
        let site = Site::new()
            .layout("/docs", |c| Ok(format!("<div dp-layout=\"/docs\">{}</div>", c.children)));
        assert!(site.check().is_ok());
    }
}
