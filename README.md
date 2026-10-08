# prerender

A static site generator for prerendered partial navigation.

Every route is built several times: once as a full document, and once for each
number of layout layers a visitor might already be holding. With JavaScript,
clicking a link fetches the smallest of those forms that still covers what
changed, and swaps it in. Without JavaScript — a crawler, a reader who turned
it off, the first load of any page — the browser gets the document. Neither
form is derived from the other at runtime, so the server stays a static file
server.

All the forms live at the same URL. A request carrying a `DP-Partial` header is
served from a partial tree; everything else gets the document.

![A navigation from the shell down to a single panel, each step ringing the part
of the page that was replaced](examples/nested-cards/docs/tour.webp)

Four clicks, each one layer deeper. The ring marks exactly what the client
replaced: the section, then the board, then one panel — and on the way back
out, the board again. Everything outside the ring was never re-sent.
Recorded by [`scripts/record.sh`](examples/nested-cards/scripts/record.sh).

```
dist/delta/card-6/activity/index.html             2932 bytes   <!DOCTYPE> down
dist-partial/1/delta/card-6/activity/index.html   2468 bytes   below the shell
dist-partial/2/…                                  1791 bytes   below the section
dist-partial/3/…                                   425 bytes   one panel
```

## Quick start

A site is a Rust crate, so there is nothing to install — clone and run one of
the examples:

```sh
git clone https://github.com/DHHwasRight/prerender && cd prerender
cargo run -p nested-cards -- build     # render every tree
cd examples/nested-cards && ./serve.sh # write an nginx config and serve it
```

## Layouts

A layout is registered against a path prefix and wraps every route beneath it.
Rendering folds from the innermost layout outward, which is what makes the
partials cheap: each is the same fold, started one layer further in.

```rust
prerender::run(
    Site::new()
        .layout("/",            |ctx| views::layouts::Root { … }.render_to_string())
        .layout("/docs",        |ctx| views::layouts::Docs { … }.render_to_string())
        .layout("/docs/guides", |ctx| views::layouts::Guides { … }.render_to_string())
        .route("/docs/guides/install", || views::pages::Install {}.render_to_string()),
)
```

Each layout is handed a `LayoutCtx` — the prefix it was registered under, the
path being rendered, and the markup it must wrap — and must mark itself with
that prefix:

```rust
pub fn Docs(layout: &str, children: Element) -> Element {
    <div class="docs">
        <aside>…sidebar…</aside>
        <div dp-layout={layout}>{children}</div>
    </div>
}
```

That marker is the whole trick. A partial contains every layout below the one
it starts at, because the server does not know where the visitor came from.
The client does — the old DOM is right there.
[dp-swap](https://github.com/DHHwasRight/htmx) walks both trees down their
`dp-layout` markers, finds the last layer whose prefix matches on both sides,
and replaces only what is below it:

| navigation | deepest shared layer | what is replaced |
|---|---|---|
| `/docs/guides/install` → `/docs/guides/deploy` | `/docs/guides` | the guide body |
| `/docs/reference` → `/docs/guides/install` | `/docs` | the whole guides section |
| `/about` → `/docs/guides/install` | none | the entire outlet |

The marker goes on the element wrapping **exactly** the children. Anything a
layout renders beside its children is replaced along with them, so a sidebar
inside the marked element loses its scroll position on every navigation —
which looks like a working site that flickers, rather than one that breaks.
`prerender check` fails the build when a layout forgets its marker or drops its
children; it cannot catch a marker in the wrong place.

## Asking for less

The client counts the layers it already holds and sends the number:

```
GET /delta/card-6/activity      DP-Partial: 3
```

so the server can answer from `dist-partial/3` — the panel alone, 425 bytes
rather than 2932. Nothing is configured for this and there is no manifest: the
markers in the live document name the prefixes the current page sits under, and
`build` writes one tree per depth up to the deepest layout chain in the site.
A server that does not understand the number can treat any value as "give me
the fragment" and serve depth 1.

## Chrome that is not swapped

The deeper the swap, the more of the page keeps its old state — including the
sidebar link that is still highlighted for the section you left. A layout can
be registered with a second renderer whose output is applied out of band:

```rust
site.layout_oob(
    "/",
    move |ctx| views::layouts::Shell { … }.render_to_string(),
    // The shell is skipped by every partial, so the sidebar highlight
    // would otherwise never move.
    move |ctx| views::layouts::RailOob { path: ctx.path, … }.render_to_string(),
)
```

The OOB renderer emits elements carrying `dp-swap-oob` and an `id`, and the
client applies them by `id` before the main swap. Only the **deepest skipped**
layout contributes OOB markup to a given partial, so a response never carries
replacements for chrome the client is about to discard anyway.

## Serving it

`prerender nginx` emits a complete config — not a snippet — so it runs as
printed:

```sh
cargo run -p docs-site -- build
cargo run -p docs-site -- nginx --prefix .run --port 8099 > .run/nginx.conf
nginx -p "$PWD/.run" -c "$PWD/.run/nginx.conf"
```

```nginx
map $http_dp_partial $prerender_root {
    default   …/dist;
    ~*^true$  …/dist-partial/1;
    "1"       …/dist-partial/1;
    "2"       …/dist-partial/2;
    "3"       …/dist-partial/3;
}
server {
    root $prerender_root;
    add_header Vary DP-Partial always;
    location / { try_files $uri $uri/index.html @full; }
    location @full { root …/dist; try_files $uri $uri/index.html =404; }
}
```

Four details there are load-bearing rather than decorative:

- The match is `~*` because header values are not the client's to normalise.
- `Vary` is mandatory: several bodies share one URL, so a cache that saw a
  fragment will otherwise hand it to the next visitor asking for a document.
- A partial request that misses falls through to `@full` rather than to a 404,
  because a route that exists only at a shallower depth should still answer.
- The 404 page is deliberately not a `try_files` candidate. When it was, a
  partial request for a missing page got a whole document back and the client
  spliced a second navbar into the page.

`Site::redirect` adds exact-match `location` blocks, which nginx resolves
before any prefix location, so they cost nothing on the pages that are not
redirects.

## Examples

| Example | What it is |
|---|---|
| [`examples/docs`](examples/docs) | Three layouts, seven routes. The smallest thing that shows the shape. |
| [`examples/nested-cards`](examples/nested-cards) | Four layers, 76 pages, depth negotiation and out-of-band highlights, with [Playwright tests](examples/nested-cards/tests/partial.spec.ts) that assert what each navigation replaced by marking live elements and checking which marks survived. |

The files worth reading first are
[`nested-cards/src/main.rs`](examples/nested-cards/src/main.rs) for how a site
is assembled and
[`nested-cards/src/views/layouts.rsx`](examples/nested-cards/src/views/layouts.rsx)
for where the markers go.

## Content and assets

Two modules sit beside the generator for sites that are not hand-written:

- `prerender::content` reads a directory of markdown with YAML front matter
  into `Doc { url, slug, front, html }`, via comrak, with syntax highlighting
  applied at build time so no highlighter ships to the browser. It also
  rewrites the MDX-style component tags a Mintlify export is full of, which is
  narrow and exists because a real corpus needed it.
- `prerender::assets` minifies CSS with lightningcss and JavaScript with oxc,
  and inlines both into the HTML — worth having when a page is 2 KB and a
  round trip for a stylesheet costs more than the stylesheet.

## Why a header rather than a second URL

dp-swap's original design gives each route a second URL
(`/page/_fragment.html`), which needs no server support at all. The header
keeps one URL per route and lets the depth ride along with the request, at the
cost of requiring a server that routes on it. Both are supported by the client;
this generator targets the second.

## Status

A working generator with real sites built on it, but not a finished product.
Absent: incremental builds, a dev server with reload, sitemaps, feeds, asset
hashing, and cache invalidation of any kind.
