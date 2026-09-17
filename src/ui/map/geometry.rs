//! Low-level terminal-grid geometry used by the operational Map renderer.
//!
//! This module owns rail masks/glyphs and collision-safe writes to the character
//! grid. Higher-level Map code decides *what* to draw; this module decides how
//! that geometry is represented in terminal cells.

use ratatui::style::{Modifier, Style};

use crate::ui::theme;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MapInk {
    Empty,
    Rail,
    RailAccent,
    RailLabel,
    Connected,
    ConnectedAdjacent,
    Unconnected,
    Selected,
    Cursor,
    Ready,
    Train,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct MapCell {
    pub(super) ch: char,
    pub(super) ink: MapInk,
}

impl Default for MapCell {
    fn default() -> Self {
        Self {
            ch: ' ',
            ink: MapInk::Empty,
        }
    }
}

pub(super) const RAIL_LEFT: u8 = 1;
pub(super) const RAIL_RIGHT: u8 = 2;
const RAIL_UP: u8 = 4;
const RAIL_DOWN: u8 = 8;

pub(super) fn can_place_text(grid: &[Vec<MapCell>], x: i32, y: i32, text: &str) -> bool {
    let Ok(y) = usize::try_from(y) else {
        return false;
    };
    let Some(row) = grid.get(y) else {
        return false;
    };
    let Ok(start_x) = usize::try_from(x) else {
        return false;
    };
    let text_width = text.chars().count();
    let Some(end_x) = start_x.checked_add(text_width) else {
        return false;
    };
    if end_x > row.len() {
        return false;
    }

    row[start_x..end_x]
        .iter()
        .all(|cell| cell.ink == MapInk::Empty)
}

pub(super) fn draw_orthogonal_rail(
    masks: &mut [Vec<u8>],
    accents: &mut [Vec<bool>],
    doubles: &mut [Vec<bool>],
    start: (i32, i32),
    end: (i32, i32),
    accent: bool,
    double_track: bool,
) {
    let corner = (end.0, start.1);
    draw_segment(masks, accents, doubles, start, corner, accent, double_track);
    draw_segment(masks, accents, doubles, corner, end, accent, double_track);
}

fn draw_segment(
    masks: &mut [Vec<u8>],
    accents: &mut [Vec<bool>],
    doubles: &mut [Vec<bool>],
    start: (i32, i32),
    end: (i32, i32),
    accent: bool,
    double_track: bool,
) {
    let (mut x, mut y) = start;
    while (x, y) != end {
        let next = if x < end.0 {
            (x + 1, y)
        } else if x > end.0 {
            (x - 1, y)
        } else if y < end.1 {
            (x, y + 1)
        } else {
            (x, y - 1)
        };
        add_rail_connection(masks, accents, doubles, (x, y), next, accent, double_track);
        x = next.0;
        y = next.1;
    }
}

fn add_rail_connection(
    masks: &mut [Vec<u8>],
    accents: &mut [Vec<bool>],
    doubles: &mut [Vec<bool>],
    from: (i32, i32),
    to: (i32, i32),
    accent: bool,
    double_track: bool,
) {
    let (from_bit, to_bit) = match (to.0 - from.0, to.1 - from.1) {
        (1, 0) => (RAIL_RIGHT, RAIL_LEFT),
        (-1, 0) => (RAIL_LEFT, RAIL_RIGHT),
        (0, 1) => (RAIL_DOWN, RAIL_UP),
        (0, -1) => (RAIL_UP, RAIL_DOWN),
        _ => return,
    };
    add_rail_bit(
        masks,
        accents,
        doubles,
        from,
        from_bit,
        accent,
        double_track,
    );
    add_rail_bit(masks, accents, doubles, to, to_bit, accent, double_track);
}

fn add_rail_bit(
    masks: &mut [Vec<u8>],
    accents: &mut [Vec<bool>],
    doubles: &mut [Vec<bool>],
    (x, y): (i32, i32),
    bit: u8,
    accent: bool,
    double_track: bool,
) {
    let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y)) else {
        return;
    };
    let Some(row) = masks.get_mut(y) else {
        return;
    };
    let Some(mask) = row.get_mut(x) else {
        return;
    };
    *mask |= bit;
    if accent {
        if let Some(row) = accents.get_mut(y) {
            if let Some(value) = row.get_mut(x) {
                *value = true;
            }
        }
    }
    if double_track {
        if let Some(row) = doubles.get_mut(y) {
            if let Some(value) = row.get_mut(x) {
                *value = true;
            }
        }
    }
}

pub(super) fn rail_glyph(mask: u8, accent: bool, double_track: bool) -> char {
    if double_track {
        double_rail_glyph(mask)
    } else if accent {
        heavy_rail_glyph(mask)
    } else {
        light_rail_glyph(mask)
    }
}

fn double_rail_glyph(mask: u8) -> char {
    match mask {
        m if m == (RAIL_LEFT | RAIL_RIGHT) => '═',
        m if m == (RAIL_UP | RAIL_DOWN) => '║',
        m if m == (RAIL_RIGHT | RAIL_DOWN) => '╔',
        m if m == (RAIL_LEFT | RAIL_DOWN) => '╗',
        m if m == (RAIL_RIGHT | RAIL_UP) => '╚',
        m if m == (RAIL_LEFT | RAIL_UP) => '╝',
        m if m == (RAIL_LEFT | RAIL_RIGHT | RAIL_DOWN) => '╦',
        m if m == (RAIL_LEFT | RAIL_RIGHT | RAIL_UP) => '╩',
        m if m == (RAIL_UP | RAIL_DOWN | RAIL_RIGHT) => '╠',
        m if m == (RAIL_UP | RAIL_DOWN | RAIL_LEFT) => '╣',
        m if m == (RAIL_LEFT | RAIL_RIGHT | RAIL_UP | RAIL_DOWN) => '╬',
        m if m & (RAIL_LEFT | RAIL_RIGHT) != 0 && m & (RAIL_UP | RAIL_DOWN) != 0 => '╬',
        m if m & (RAIL_LEFT | RAIL_RIGHT) != 0 => '═',
        m if m & (RAIL_UP | RAIL_DOWN) != 0 => '║',
        _ => '·',
    }
}

fn heavy_rail_glyph(mask: u8) -> char {
    match mask {
        m if m == (RAIL_LEFT | RAIL_RIGHT) => '━',
        m if m == (RAIL_UP | RAIL_DOWN) => '┃',
        m if m == (RAIL_RIGHT | RAIL_DOWN) => '┏',
        m if m == (RAIL_LEFT | RAIL_DOWN) => '┓',
        m if m == (RAIL_RIGHT | RAIL_UP) => '┗',
        m if m == (RAIL_LEFT | RAIL_UP) => '┛',
        m if m == (RAIL_LEFT | RAIL_RIGHT | RAIL_DOWN) => '┳',
        m if m == (RAIL_LEFT | RAIL_RIGHT | RAIL_UP) => '┻',
        m if m == (RAIL_UP | RAIL_DOWN | RAIL_RIGHT) => '┣',
        m if m == (RAIL_UP | RAIL_DOWN | RAIL_LEFT) => '┫',
        m if m == (RAIL_LEFT | RAIL_RIGHT | RAIL_UP | RAIL_DOWN) => '╋',
        m if m & (RAIL_LEFT | RAIL_RIGHT) != 0 && m & (RAIL_UP | RAIL_DOWN) != 0 => '╋',
        m if m & (RAIL_LEFT | RAIL_RIGHT) != 0 => '━',
        m if m & (RAIL_UP | RAIL_DOWN) != 0 => '┃',
        _ => '·',
    }
}

fn light_rail_glyph(mask: u8) -> char {
    match mask {
        m if m == (RAIL_LEFT | RAIL_RIGHT) => '─',
        m if m == (RAIL_UP | RAIL_DOWN) => '│',
        m if m == (RAIL_RIGHT | RAIL_DOWN) => '┌',
        m if m == (RAIL_LEFT | RAIL_DOWN) => '┐',
        m if m == (RAIL_RIGHT | RAIL_UP) => '└',
        m if m == (RAIL_LEFT | RAIL_UP) => '┘',
        m if m == (RAIL_LEFT | RAIL_RIGHT | RAIL_DOWN) => '┬',
        m if m == (RAIL_LEFT | RAIL_RIGHT | RAIL_UP) => '┴',
        m if m == (RAIL_UP | RAIL_DOWN | RAIL_RIGHT) => '├',
        m if m == (RAIL_UP | RAIL_DOWN | RAIL_LEFT) => '┤',
        m if m == (RAIL_LEFT | RAIL_RIGHT | RAIL_UP | RAIL_DOWN) => '┼',
        m if m & (RAIL_LEFT | RAIL_RIGHT) != 0 && m & (RAIL_UP | RAIL_DOWN) != 0 => '┼',
        m if m & (RAIL_LEFT | RAIL_RIGHT) != 0 => '─',
        m if m & (RAIL_UP | RAIL_DOWN) != 0 => '│',
        _ => '·',
    }
}

pub(super) fn put_cell(grid: &mut [Vec<MapCell>], x: i32, y: i32, ch: char, ink: MapInk) {
    let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y)) else {
        return;
    };
    if let Some(cell) = grid.get_mut(y).and_then(|row| row.get_mut(x)) {
        *cell = MapCell { ch, ink };
    }
}

pub(super) fn put_text(grid: &mut [Vec<MapCell>], x: i32, y: i32, text: &str, ink: MapInk) {
    for (offset, ch) in text.chars().enumerate() {
        let offset = i32::try_from(offset).unwrap_or(i32::MAX);
        put_cell(grid, x.saturating_add(offset), y, ch, ink);
    }
}

pub(super) fn map_ink_style(ink: MapInk) -> Style {
    match ink {
        MapInk::Empty => theme::panel(),
        MapInk::Rail => theme::secondary().add_modifier(Modifier::DIM),
        MapInk::RailAccent => theme::focused_title(),
        MapInk::RailLabel => theme::focused_border(),
        MapInk::Connected => theme::secondary().add_modifier(Modifier::DIM),
        MapInk::ConnectedAdjacent => theme::primary_value().add_modifier(Modifier::BOLD),
        MapInk::Unconnected => theme::secondary().add_modifier(Modifier::DIM),
        MapInk::Selected => theme::focused_title(),
        MapInk::Cursor => theme::warning().add_modifier(Modifier::BOLD),
        MapInk::Ready => theme::success().add_modifier(Modifier::BOLD),
        MapInk::Train => theme::warning().add_modifier(Modifier::BOLD),
    }
}
