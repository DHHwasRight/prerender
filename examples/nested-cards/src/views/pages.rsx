use crate::data::{Card, Section};

/// The grid of cards for a section. Clicking one opens it, which replaces this
/// grid with the split view — the same layer, a different arrangement.
pub fn Grid(section: &Section) -> Element {
    <div class="grid">
        for card in &section.cards {
            <a class="card" href={card.url(section)} style={card_style(card)}>
                <span class="card-top"></span>
                <span class="card-title">{card.name}</span>
                for _ in 0..card.lines {
                    <span class="line"></span>
                }
                <span class="line short"></span>
            </a>
        }
    </div>
}

/// One panel inside an expanded card. The deepest page there is: switching
/// between panels replaces only this.
pub fn Panel(card: &Card, tab: &str) -> Element {
    <div class="panel">
        <span class="panel-label">{tab}</span>
        for row in panel_rows(tab) {
            <span class={row}></span>
        }
        if tab == "Overview" {
            <div class="blocks">
                for _ in 0..3 {
                    <span class="block" style={block_style(card)}></span>
                }
            </div>
        }
    </div>
}

fn panel_rows(tab: &str) -> Vec<&'static str> {
    // Each panel draws a different shape so a swap is obvious at a glance.
    match tab {
        "Overview" => vec!["line", "line", "line short"],
        "Details" => vec!["line", "line", "line", "line", "line short"],
        _ => vec!["line short", "line", "line short", "line"],
    }
}

fn card_style(card: &Card) -> String {
    format!("--tint:{}", card.colour())
}

fn block_style(card: &Card) -> String {
    format!("background:{}", card.colour())
}
