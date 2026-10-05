// The three nested layouts. Each marks itself with `dp-layout` set to the
// prefix it was registered under: that marker is how the client finds the
// deepest layer two pages share, and so how it decides what to replace.

/// Exact match is the current page; an ancestor is merely on the way to it.
/// Without the distinction, "Overview" lights up while you read a guide three
/// levels below it.
///
/// Plain Rust, not markup: in a template body `if` opens a markup conditional,
/// so `let x = if ...` is parsed as one and the binding never materialises.
fn nav_class(href: &str, path: &str) -> &'static str {
    if path == href {
        "nav-link is-current"
    } else if href != "/" && path.starts_with(&format!("{href}/")) {
        "nav-link is-ancestor"
    } else {
        "nav-link"
    }
}

pub fn NavLink(href: &str, label: &str, path: &str) -> Element {
    <a href={href} class={nav_class(href, path)}>{label}</a>
}

/// For the root layout's nav only.
///
/// The root layout is the one layer a partial never contains, so it is never
/// re-rendered during a swap. Its highlight therefore has to be identical for
/// every page in a section, or it goes stale the moment you navigate within
/// one. Being inside the section is what counts, not being the section index.
fn section_class(href: &str, path: &str) -> &'static str {
    let inside = path == href || (href != "/" && path.starts_with(&format!("{href}/")));
    if inside { "nav-link is-current" } else { "nav-link" }
}

pub fn SectionLink(href: &str, label: &str, path: &str) -> Element {
    <a href={href} class={section_class(href, path)}>{label}</a>
}

// Level 1. The only layout outside the swap region: its header and footer are
// never re-sent, because the partial build starts one layer in.
pub fn Root(layout: &str, path: &str, children: Element) -> Element {
    <!DOCTYPE html>
    <html lang="en">
        <head>
            <meta charset="utf-8" />
            <meta name="viewport" content="width=device-width, initial-scale=1" />
            <title>prerender — three nested layouts</title>
            <link rel="stylesheet" href="/site.css" />
        </head>
        <body dp-layout={layout} dp-boost dp-target="#app">
            <header class="site-header">
                <strong class="brand">prerender</strong>
                <nav class="site-nav">
                    <SectionLink href="/" label="Home" path={path} />
                    <SectionLink href="/docs" label="Docs" path={path} />
                    <SectionLink href="/about" label="About" path={path} />
                </nav>
            </header>
            // Everything below here is what a partial replaces.
            <main id="app">{children}</main>
            <footer class="site-footer">
                <span>Navigate with JavaScript off: every page is a real document.</span>
            </footer>
            <script src="/dpswap.min.js"></script>
            <script src="/boot.js"></script>
        </body>
    </html>
}

// Level 2. Wraps everything under /docs.
pub fn Docs(layout: &str, path: &str, children: Element) -> Element {
    <div dp-layout={layout} class="docs">
        <aside class="docs-sidebar">
            <h2>Documentation</h2>
            <NavLink href="/docs" label="Overview" path={path} />
            <NavLink href="/docs/reference" label="Reference" path={path} />
            <NavLink href="/docs/guides" label="Guides" path={path} />
        </aside>
        <div class="docs-body">{children}</div>
    </div>
}

// Level 3. Wraps everything under /docs/guides.
pub fn Guides(layout: &str, path: &str, children: Element) -> Element {
    <div dp-layout={layout} class="guides">
        <nav class="guides-tabs">
            <NavLink href="/docs/guides" label="All guides" path={path} />
            <NavLink href="/docs/guides/install" label="Install" path={path} />
            <NavLink href="/docs/guides/deploy" label="Deploy" path={path} />
        </nav>
        <article class="guide-body">{children}</article>
    </div>
}
