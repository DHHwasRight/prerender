// Page bodies. A page renders only its own content; prerender folds the layouts
// around it.

pub fn Explainer(from: &str, swaps: &str) -> Element {
    <p class="explainer">
        <span class="tag">Try it</span>
        "Navigating here from " <code>{from}</code> " replaces " <strong>{swaps}</strong> "."
    </p>
}

pub fn Home() -> Element {
    <section class="page">
        <h1>Prerendered partial navigation</h1>
        <p>
            "Every route is built twice: once as a whole document, once as the
             content below the outermost layout. Clicking a link fetches the
             second form and swaps it in. With JavaScript off you get the first,
             and nothing else changes."
        </p>
        <p>
            "Open the network panel and move between the guides. The responses
             are static files; the server picks which tree to read from by
             looking at one request header."
        </p>
    </section>
}

pub fn About() -> Element {
    <section class="page">
        <h1>About</h1>
        <p>
            "This site exists to exercise three nested layouts. Moving between
             sections replaces a different amount of the page each time, and the
             highlight shows exactly how much."
        </p>
        <Explainer from="/docs/guides/install" swaps="all three layers" />
    </section>
}

pub fn DocsIndex() -> Element {
    <section class="page">
        <h1>Documentation</h1>
        <p>"The sidebar beside this text belongs to the "<code>/docs</code>" layout."</p>
        <Explainer from="/about" swaps="the docs layout and the page" />
    </section>
}

pub fn Reference() -> Element {
    <section class="page">
        <h1>Reference</h1>
        <p>
            "Still inside "<code>/docs</code>", so moving here from the overview
             leaves the sidebar alone and replaces only this column."
        </p>
        <Explainer from="/docs" swaps="only the page body" />
    </section>
}

pub fn GuidesIndex() -> Element {
    <section class="page">
        <h1>Guides</h1>
        <p>"The tab strip above belongs to the "<code>/docs/guides</code>" layout."</p>
        <Explainer from="/docs/reference" swaps="the guides layout and the page" />
    </section>
}

pub fn Install() -> Element {
    <section class="page">
        <h1>Install</h1>
        <pre><code>"cargo install prerender"</code></pre>
        <p>"Three layers deep. Both the sidebar and the tab strip stay put."</p>
        <Explainer from="/docs/guides/deploy" swaps="only the page body" />
    </section>
}

pub fn Deploy() -> Element {
    <section class="page">
        <h1>Deploy</h1>
        <pre><code>"cargo run -p docs-site -- build"</code></pre>
        <p>
            "Copy "<code>dist</code>" and "<code>dist-partial</code>" to the
             server and point the generated nginx config at them."
        </p>
        <Explainer from="/docs/guides/install" swaps="only the page body" />
    </section>
}
