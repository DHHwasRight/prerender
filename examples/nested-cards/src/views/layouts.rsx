use crate::data::{Card, Section, TABS};

// Four layers, each registered against a path prefix. What a navigation
// replaces is decided by how many of them the two pages share, so every click
// in this example swaps a different amount of the document.

/// Layer 1. The shell: sidebar and the outlet everything else lives in. It is
/// the one layer a partial never contains, so it is never re-sent.
pub fn Shell(layout: &str, path: &str, sections: &[Section], children: Element) -> Element {
    <!DOCTYPE html>
    <html lang="en">
        <head>
            <meta charset="utf-8" />
            <meta name="viewport" content="width=device-width, initial-scale=1" />
            <title>Nested partial loading</title>
            <link rel="stylesheet" href="/site.css" />
        </head>
        <body dp-boost dp-target="#app">
            <div class="frame">
                <aside class="rail">
                    <div class="rail-mark"></div>
                    <nav class="rail-nav">
                        for section in sections {
                            <a href={section.url()} class={rail_class(&section.url(), path)}>
                                <span class="dot" style={dot_style(section)}></span>
                                {section.name}
                            </a>
                        }
                    </nav>
                    <div class="rail-foot"></div>
                </aside>
                // The marker goes on the outlet, not the body: a swap replaces
            // the marked element's children, so everything rendered beside it
            // -- the whole sidebar -- is preserved by construction.
            <main id="app" dp-layout={layout} class="stage">{children}</main>
            </div>
            <script src="/dpswap.min.js"></script>
            <script src="/boot.js"></script>
        </body>
    </html>
}

/// Layer 2. One section. Holds the toolbar and the area the cards live in;
/// moving between sections replaces this whole thing, moving within one does
/// not.
pub fn SectionLayout(layout: &str, section: &Section, children: Element) -> Element {
    <div class="section">
        // Outside the marker, so opening a card does not rebuild the toolbar.
        <header class="bar">
            <span class="bar-title">{section.name}</span>
            <span class="bar-fill"></span>
            <span class="chip"></span>
            <span class="chip"></span>
        </header>
        // The board holds either the grid or the split; which one is decided
        // by the page below, so the container needs no state of its own.
        <div dp-layout={layout} class="board">{children}</div>
    </div>
}

/// Layer 3. One expanded card: the detail panel at two thirds and its siblings
/// collapsed to a list at one third. Switching cards replaces this; switching
/// panels inside a card does not.
pub fn CardLayout(
    layout: &str,
    path: &str,
    section: &Section,
    card: &Card,
    siblings: &[&Card],
    children: Element,
) -> Element {
    <div class="split">
        <article class="detail" style={tint_style(card)}>
            <header class="detail-head">
                <span class="swatch" style={swatch_style(card)}></span>
                <span class="detail-title">{card.name}</span>
                <a class="close" href={section.url()} aria-label="Back to the grid"></a>
            </header>
            <nav class="tabs">
                for (slug, label) in TABS {
                    <a href={card.tab_url(section, slug)} class={tab_class(&card.tab_url(section, slug), path)}>
                        {label}
                    </a>
                }
            </nav>
            // The marker sits here, so moving between panels replaces one
            // panel and leaves the card header, its tabs and the sibling list
            // exactly where they are.
            <div dp-layout={layout} class="detail-body">{children}</div>
        </article>

        <aside class="siblings">
            for other in siblings {
                <a href={other.url(section)} class={sibling_class(&other.url(section), path)}>
                    <span class="swatch sm" style={swatch_style(other)}></span>
                    <span class="sibling-name">{other.name}</span>
                </a>
            }
        </aside>
    </div>
}

// --------------------------------------------------------------------- helpers
//
// Branching that produces a value lives in plain functions: inside a template
// body `if` opens a markup conditional, so `let x = if …` never binds.

fn rail_class(href: &str, path: &str) -> &'static str {
    if path == href || path.starts_with(&format!("{href}/")) {
        "rail-link is-current"
    } else {
        "rail-link"
    }
}

fn tab_class(href: &str, path: &str) -> &'static str {
    if path == href { "tab is-current" } else { "tab" }
}

fn sibling_class(href: &str, path: &str) -> &'static str {
    if path.starts_with(&format!("{href}/")) { "sibling is-current" } else { "sibling" }
}

fn dot_style(section: &Section) -> String {
    let colour = section.cards.first().map_or("#cdb4db", |c| c.colour());
    format!("background:{colour}")
}

fn swatch_style(card: &Card) -> String {
    format!("background:{}", card.colour())
}

fn tint_style(card: &Card) -> String {
    format!("--tint:{}", card.colour())
}
