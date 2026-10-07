# nested-cards

Four nested layouts, built to show what a partial navigation actually replaces.

```sh
cargo run -p nested-cards -- build --out dist --partial-out dist-partial
./serve.sh                 # builds, writes an nginx config, serves on :8097
npx playwright test        # proves the claims below
```

Everything is a rectangle. The only colour comes from five swatches —
`#cdb4db #ffc8dd #ffafcc #bde0fe #a2d2ff` — used to tell cards apart rather
than to decorate, because the point is to watch which rectangles change.

## The layers

```
/                        shell + sidebar                    never re-sent
/alpha                   section bar + board
/alpha/card-2            expanded card + sibling list
/alpha/card-2/details    one panel
```

Clicking a sidebar link shows a grid of cards. Clicking a card expands it into
a two-thirds detail panel with the remaining cards collapsed to a list on the
right. Clicking a tab inside the card swaps just that panel.

Each of those crosses a different number of layers, so each replaces a
different amount of the document:

| from | to | deepest shared layer | replaced |
|---|---|---|---|
| `/alpha` | `/beta` | `/` | the board and its toolbar |
| `/alpha` | `/alpha/card-2/overview` | `/alpha` | the board — grid becomes a split |
| `/alpha/card-2/…` | `/alpha/card-5/…` | `/alpha` | the split |
| `/alpha/card-2/overview` | `/alpha/card-2/details` | `/alpha/card-2` | one panel |

`assets/boot.js` outlines whichever region was swapped, so the difference is
visible rather than inferred.

## Where the marker goes decides what survives

A swap replaces the **children** of the deepest `dp-layout` element the two
pages share. Anything a layout renders *beside* `{children}` is inside those
children, and so is replaced too.

That is the whole of it, and it is easy to get wrong. The first version of
this example put the marker on the layout's outer element:

```rust
<div dp-layout={layout} class="section">
    <header class="bar">…</header>
    <div class="board">{children}</div>
</div>
```

Opening a card then rebuilt the toolbar along with the board — correct, but
wasteful, and the toolbar visibly flickered. Moving the marker onto the
container that wraps exactly the children fixes it:

```rust
<div class="section">
    <header class="bar">…</header>          // outside the marker: preserved
    <div dp-layout={layout} class="board">{children}</div>
</div>
```

The card layer does the same thing one level down: its marker sits on
`.detail-body`, so switching panels leaves the card header, its tabs and the
sibling list untouched. The tests assert each of these by marking live
elements, navigating, and checking which marks survived — a mark survives
exactly when its element was not replaced.

One consequence worth knowing: a layout cannot put state on its own marker
element, because that element is never re-rendered. The grid and the split are
therefore two different children of a neutral `.board`, rather than one element
with a class the layout toggles.

## Without JavaScript

Every route is a complete document, so the whole thing works with scripting
off — the layouts stop being partial and become ordinary page loads. A test
covers it.
