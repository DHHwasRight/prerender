//! Four nested layouts, to show what a partial navigation actually replaces.
//!
//! ```text
//! /                      shell + sidebar        never re-sent
//! /alpha                 section bar + board
//! /alpha/card-2          expanded card + sibling list
//! /alpha/card-2/details  one panel
//! ```
//!
//! Each click crosses a different number of those, and the client replaces
//! only what sits below the deepest layer the two pages share:
//!
//! | from | to | replaced |
//! |---|---|---|
//! | `/alpha` | `/beta` | everything but the sidebar |
//! | `/alpha` | `/alpha/card-2/overview` | the board — grid becomes a split |
//! | `/alpha/card-2/…` | `/alpha/card-5/…` | the split |
//! | `/alpha/card-2/overview` | `/alpha/card-2/details` | one panel |

mod data;
mod views {
    rsx::load_dir!("src/views");
}

use std::path::Path;
use std::rc::Rc;

use prerender::{LayoutCtx, Site};
use rsx::Template;

use crate::data::TABS;

fn main() -> std::process::ExitCode {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let sections = Rc::new(data::sections());

    let mut site = Site::new().static_dir(root.join("assets"));

    {
        let for_render = Rc::clone(&sections);
        let for_oob = Rc::clone(&sections);
        site = site.layout_oob(
            "/",
            move |ctx: LayoutCtx<'_>| {
                views::layouts::Shell {
                    layout: ctx.prefix,
                    path: ctx.path,
                    sections: &for_render,
                    children: rsx::raw(ctx.children),
                }
                .render_to_string()
            },
            // The shell is skipped by every partial, so the sidebar highlight
            // would otherwise never move.
            move |ctx: LayoutCtx<'_>| {
                views::layouts::RailOob { path: ctx.path, sections: &for_oob }.render_to_string()
            },
        );
    }

    for (index, section) in sections.iter().enumerate() {
        let sections_for_layout = Rc::clone(&sections);
        site = site.layout(&section.url(), move |ctx: LayoutCtx<'_>| {
            views::layouts::SectionLayout {
                layout: ctx.prefix,
                section: &sections_for_layout[index],
                children: rsx::raw(ctx.children),
            }
            .render_to_string()
        });

        // The section index is the grid.
        let sections_for_grid = Rc::clone(&sections);
        site = site.route(&section.url(), move || {
            views::pages::Grid { section: &sections_for_grid[index] }.render_to_string()
        });

        for (card_index, card) in section.cards.iter().enumerate() {
            let for_card = Rc::clone(&sections);
            let for_card_oob = Rc::clone(&sections);
            site = site.layout_oob(
                &card.url(section),
                move |ctx: LayoutCtx<'_>| {
                    let section = &for_card[index];
                    let card = &section.cards[card_index];
                    views::layouts::CardLayout {
                        layout: ctx.prefix,
                        path: ctx.path,
                        section,
                        card,
                        children: rsx::raw(ctx.children),
                    }
                    .render_to_string()
                },
                // Switching panels leaves the tab strip and the sibling list
                // in place, so their highlights arrive out of band.
                move |ctx: LayoutCtx<'_>| {
                    let section = &for_card_oob[index];
                    let card = &section.cards[card_index];
                    views::layouts::CardOob { path: ctx.path, section, card }.render_to_string()
                },
            );

            // Opening a card lands on its first panel.
            site = site.redirect(card.url(section), card.tab_url(section, TABS[0].0));

            for (slug, label) in TABS {
                let sections_for_panel = Rc::clone(&sections);
                site = site.route(&card.tab_url(section, slug), move || {
                    let card = &sections_for_panel[index].cards[card_index];
                    views::pages::Panel { card, tab: label }.render_to_string()
                });
            }
        }
    }

    // The front door is the first section, since there is nothing else to show.
    site = site.redirect("/", sections[0].url());

    prerender::run(site)
}
