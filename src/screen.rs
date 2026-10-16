use ratatui::layout::Rect;

#[derive(Clone, Debug, Default)]
pub struct Screen {
    pub nav_hits: Vec<(Rect, usize)>,
    pub nav_area: Option<Rect>,
    pub nav_height: usize,
    pub nav_offset: usize,
    pub filter: Option<Rect>,
    pub transcript: Option<Rect>,
    pub transcript_height: usize,
    pub transcript_lines: usize,
    pub transcript_width: usize,
    pub composer: Option<Rect>,
    pub composer_box: Option<Rect>,
    pub composer_width: usize,
    pub composer_first: usize,
}

pub fn contains(rect: Rect, column: u16, row: u16) -> bool {
    column >= rect.x
        && row >= rect.y
        && column < rect.x.saturating_add(rect.width)
        && row < rect.y.saturating_add(rect.height)
}

pub fn hit(hits: &[(Rect, usize)], column: u16, row: u16) -> Option<usize> {
    hits.iter()
        .rev()
        .find(|(rect, _)| contains(*rect, column, row))
        .map(|(_, index)| *index)
}
