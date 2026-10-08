# nested-cards

Four nested layouts, built to show what a partial navigation actually replaces.

![A navigation from the shell down to a single panel, each step ringing the part
of the page that was replaced](docs/tour.webp)

```sh
cargo run -p nested-cards -- build --out dist --partial-out dist-partial
./serve.sh                 # builds, writes an nginx config, serves on :8097
npx playwright test        # proves the claims below
npm run record             # re-records the tour above
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

`assets/boot.js` draws a rounded ring around whichever region was swapped, so
the difference is visible rather than inferred: a tab switch rings the panel
alone, opening a card rings the whole board.

Two details that ring needed. It is an `outline` with an `outline-offset`,
because only an outline can stand away from the element without a colour behind
it — a shadow's spread starts at the border box, so the gap would have to be
painted in whatever is behind, and the swapped region sits on the white panel
in one case and the page ground in another. The radius on the element is what
rounds it.

And the class is removed on `animationend` only when the event came from the
marked element itself. `animationend` bubbles, so the entrance animation of the
content swapped in was ending the highlight first — a 200ms panel fade cut a
700ms ring down to nothing anyone could see.

## The server sends only what is missing

The client counts how many layers it already holds — the markers in the live
document name the prefixes this page sits under, and a prefix either covers the
destination or it does not — and sends that count as `DP-Partial`. The
generator writes one prerendered tree per depth, so the server answers with
what sits below that layer rather than everything below the root:

| request | bytes |
|---|---:|
| no header (a crawler, or JavaScript off) | 2794 |
| `DP-Partial: 1` | 2330 |
| `DP-Partial: 2` | 1653 |
| `DP-Partial: 3` | 425 |

Switching panels costs 15% of the document. No manifest is involved: the
counting is done from the DOM, and the response needs no layout markers because
the client already knows where it asked for.

## Chrome that is not swapped, but still has to change

Keeping the tab strip in place is the right call until you notice it is still
highlighting the panel you left. The strip sits outside the swapped region, so
nothing replaces it.

A skipped layout can contribute out-of-band markup for exactly those parts:

```rust
site.layout_oob(
    card.url(section),
    |ctx| CardLayout { … }.render_to_string(),       // when the layer is sent
    |ctx| CardOob { path: ctx.path, … }.render_to_string(),  // when it is skipped
)
```

`CardOob` renders the tab strip alone, carrying `dp-swap-oob="outerHTML"` and
the id of the element it replaces. The client applies it before the main swap.
The same mechanism moves the sidebar highlight when the shell is skipped.

Only the *deepest* skipped layer contributes. If the section layer is shared
then the section has not changed, so its sidebar highlight cannot have moved
and re-sending it would be waste — before that rule, a panel switch carried a
sidebar and a sibling list that were already correct, and cost 1669 bytes
instead of 425.

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

## The right-hand list keeps the open card

It would be smaller to send only the five cards that are not open. It would
also mean the list reshuffles under the pointer every time you pick from it,
and the card you are reading has no position in it. All six stay; the open one
is marked.

## Without JavaScript

Every route is a complete document, so the whole thing works with scripting
off — the layouts stop being partial and become ordinary page loads. A test
covers it.

## Re-recording the tour

```sh
npm run record
```

[`scripts/record.spec.ts`](scripts/record.spec.ts) drives the same four clicks
the tests make, pausing on each long enough for the 700ms ring to play out,
then [`scripts/record.sh`](scripts/record.sh) converts the result.

Two things it has to do that a test does not. Playwright draws no pointer in
its videos, so one is injected and moved to each target before the click —
without it a viewer sees panels change with nothing appearing to cause it. And
Playwright records WebM only, so the file committed here is converted
afterwards with ffmpeg, which GitHub will animate inline where it will not play
a video from a repository path.
