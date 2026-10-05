# prerender

A static site generator for prerendered partial navigation.

Every route is built twice: as a full document, and as the content below the
outermost layout. With JavaScript, clicking a link fetches the second form and
swaps it in. Without JavaScript — a crawler, a reader who turned it off, the
first load of any page — the browser gets the first. Neither is derived from
the other at runtime, so the server stays a static file server.

The two forms live at the same URL. A request carrying `DP-Partial: true` is
served from the partial tree; everything else gets the document.

```
dist/docs/guides/install/index.html          1664 bytes, <!DOCTYPE> down
dist-partial/docs/guides/install/index.html   948 bytes, the content alone
```

## Three nested layouts

A layout is registered against a path prefix and wraps every route beneath it.
Rendering folds from the innermost layout outward, which is what makes the
partial cheap: it is the same fold, started one layer in.

```rust
Site::new()
    .layout("/",             layout!(views::layouts::Root))
    .layout("/docs",         layout!(views::layouts::Docs))
    .layout("/docs/guides",  layout!(views::layouts::Guides))
    .route("/docs/guides/install", page!(views::pages::Install))
```

Layouts mark themselves with the prefix they were registered under:

```rust
pub fn Docs(layout: &str, path: &str, children: Element) -> Element {
    <div dp-layout={layout} class="docs">…{children}</div>
}
```

That marker is the whole trick. A partial contains every layout below the root,
because the server has no idea where the visitor came from. The client does —
the old DOM is right there. [dp-swap](../../crystal/htmx) walks both trees down
their `dp-layout` markers, finds the last layer whose prefix matches on both
sides, and replaces only what is below it:

| navigation | deepest shared layer | what is replaced |
|---|---|---|
| `/docs/guides/install` → `/docs/guides/deploy` | `/docs/guides` | the guide body and tabs |
| `/docs/reference` → `/docs/guides/install` | `/docs` | the whole guides section |
| `/about` → `/docs/guides/install` | none | the entire outlet |

The replaced subtree is also what gets the animation, so the flash shows you
exactly how much of the page changed. `prerender build` fails when a layout
forgets its marker, because the result otherwise looks like a working site with
the wrong thing animating.

## Templates are Rust

Templates come from [rsx](../templ), which compiles them into the binary. A
site is therefore a Rust crate, not a directory an interpreter walks, and the
"CLI" is the site's own binary:

```
cargo run -p docs-site -- build      # render both trees
cargo run -p docs-site -- check      # verify every layout marks itself
cargo run -p docs-site -- nginx      # print a runnable nginx config
```

This is the unusual part of the design. It buys compile-time checking of every
template against real types, and costs the ability to edit a template without
a rebuild.

## Serving it

`prerender nginx` emits a complete config — not a snippet — so it runs as printed:

```sh
cargo run -p docs-site -- build
cargo run -p docs-site -- nginx --prefix .run --port 8099 > .run/nginx.conf
nginx -p "$PWD/.run" -c "$PWD/.run/nginx.conf"
```

```nginx
map $http_dp_partial $prerender_root {
    default   …/dist;
    ~*^true$  …/dist-partial;
}
server {
    root $prerender_root;
    add_header Vary DP-Partial always;
    location / { try_files $uri $uri/index.html =404; }
}
```

Two details that are not decoration. The match is `~*` because header values are
not the client's to normalise. And `Vary` is mandatory: two different bodies
share one URL, so a cache that saw a fragment will otherwise hand it to the next
visitor asking for a document.

## Why a header rather than a second URL

dp-swap's original design gives each route a second URL (`/_fragment.html`),
which needs no server support at all. The header keeps one URL per route, at
the cost of requiring a server that routes on it.

A third option — having the client send how many layout levels it already has,
so the server returns only the missing ones — would cut the wasted bytes, since
a partial currently includes layouts the client is about to discard. It needs
one prerendered tree per level and a manifest so the client knows the layout
boundaries. The client already computes the shared depth in order to narrow the
swap, so sending it is a small addition if those bytes ever show up in a
profile. They have not yet.

## Example

[`examples/docs`](examples/docs) is the three-layout site described above:
seven routes across three levels, each page naming what it replaces when you
arrive from somewhere specific. Build it, serve it, then turn JavaScript off
and confirm nothing about the site changes except that navigation stops being
partial.

## Status

A working skeleton, not a finished generator. Absent: markdown or any content
format, incremental builds, a dev server with reload, sitemaps, feeds, asset
hashing, and cache invalidation of any kind.
