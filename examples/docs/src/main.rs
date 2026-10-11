//! The three-layout example site.
//!
//! `main` is the whole site definition: layouts bound to prefixes, routes
//! bound to paths. prerender folds one around the other and writes both trees.

mod views {
    rsx::load_dir!("src/views");
}

use prerender::{LayoutCtx, Site};
use rsx::Template;

/// Bridges prerender's `Html` children to rsx's `Element` children. Layouts are
/// rendered innermost first, so by the time one runs its children are already
/// markup and go in verbatim.
macro_rules! layout {
    ($component:path) => {
        |ctx: LayoutCtx<'_>| {
            use $component as component;
            component {
                layout: ctx.prefix,
                path: ctx.path,
                children: rsx::raw(ctx.children),
            }
            .render_to_string()
        }
    };
}

macro_rules! page {
    ($component:path) => {
        || {
            use $component as component;
            component {}.render_to_string()
        }
    };
}

fn main() -> std::process::ExitCode {
    prerender::run(
        Site::new()
            // Relative to the crate, not the shell: the site definition should
            // not depend on where the binary happens to be run from.
            .static_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/assets"))
            // Shallowest to deepest, though registration order does not matter.
            .layout("/", layout!(views::layouts::Root))
            .layout("/docs", layout!(views::layouts::Docs))
            .layout("/docs/guides", layout!(views::layouts::Guides))
            .route("/", page!(views::pages::Home))
            .route("/about", page!(views::pages::About))
            .route("/docs", page!(views::pages::DocsIndex))
            .route("/docs/reference", page!(views::pages::Reference))
            .route("/docs/guides", page!(views::pages::GuidesIndex))
            .route("/docs/guides/install", page!(views::pages::Install))
            .route("/docs/guides/deploy", page!(views::pages::Deploy)),
    )
}
