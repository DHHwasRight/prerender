//! The site's content, which is deliberately nothing: coloured rectangles and
//! placeholder lines. The point of this example is the navigation, so anything
//! readable would only distract from which part of the page was replaced.

/// The five-colour palette every rectangle is drawn from.
pub const PALETTE: [&str; 5] = ["#cdb4db", "#ffc8dd", "#ffafcc", "#bde0fe", "#a2d2ff"];

pub struct Section {
    pub slug: &'static str,
    pub name: &'static str,
    pub cards: Vec<Card>,
}

pub struct Card {
    pub slug: String,
    pub name: String,
    /// Index into [`PALETTE`].
    pub tint: usize,
    /// How many placeholder lines the summary draws, so the cards differ.
    pub lines: usize,
}

/// Panels inside an expanded card. These are the deepest level, and switching
/// between them replaces the least of the page.
pub const TABS: [(&str, &str); 3] = [
    ("overview", "Overview"),
    ("details", "Details"),
    ("activity", "Activity"),
];

pub fn sections() -> Vec<Section> {
    [
        ("alpha", "Alpha"),
        ("beta", "Beta"),
        ("gamma", "Gamma"),
        ("delta", "Delta"),
    ]
    .into_iter()
    .enumerate()
    .map(|(s, (slug, name))| Section {
        slug,
        name,
        cards: (1..=6)
            .map(|c| Card {
                slug: format!("card-{c}"),
                name: format!("{name} {c}"),
                // Offset per section so neighbouring sections do not repeat
                // the same sequence of colours.
                tint: (s * 2 + c) % PALETTE.len(),
                lines: 2 + (c % 3),
            })
            .collect(),
    })
    .collect()
}

impl Section {
    pub fn url(&self) -> String {
        format!("/{}", self.slug)
    }
}

impl Card {
    pub fn url(&self, section: &Section) -> String {
        format!("/{}/{}", section.slug, self.slug)
    }
    pub fn tab_url(&self, section: &Section, tab: &str) -> String {
        format!("/{}/{}/{}", section.slug, self.slug, tab)
    }
    pub fn colour(&self) -> &'static str {
        PALETTE[self.tint]
    }
}
