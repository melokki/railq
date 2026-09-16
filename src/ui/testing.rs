//! Deterministic Ratatui capture helpers used by integration and regression tests.
//!
//! Keeping these helpers outside the shell module prevents test-only terminal
//! plumbing from obscuring the production presentation state and input routing.

use ratatui::{Terminal, backend::TestBackend, style::Color};

use crate::model::GameState;

use super::{Shell, render_frame};

/// Renders the current shell into a deterministic text copy of Ratatui's buffer.
///
/// This uses the same frame renderer as the live terminal while avoiding
/// terminal modes, clocks, and disk I/O.
pub fn capture_rendered_buffer(
    shell: &Shell,
    state: &GameState,
    columns: u16,
    rows: u16,
) -> String {
    let mut rendered_shell = shell.clone();
    capture_rendered_buffer_mut(&mut rendered_shell, state, columns, rows)
}

/// Renders a deterministic buffer while retaining presentation state changes
/// such as a stateful Table viewport.
pub fn capture_rendered_buffer_mut(
    shell: &mut Shell,
    state: &GameState,
    columns: u16,
    rows: u16,
) -> String {
    if columns == 0 || rows == 0 {
        return String::new();
    }

    let backend = TestBackend::new(columns, rows);
    let mut terminal = match Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(error) => match error {},
    };
    match terminal.draw(|frame| render_frame(frame, shell, state)) {
        Ok(_) => {}
        Err(error) => match error {},
    }

    let width = usize::from(columns);
    let height = usize::from(rows);
    let content = terminal.backend().buffer().content();
    let mut rendered = String::with_capacity((width + 1).saturating_mul(height));
    for row in content.chunks(width).take(height) {
        for cell in row {
            rendered.push_str(cell.symbol());
        }
        rendered.push('\n');
    }
    rendered
}

/// Returns the foreground and background colors painted at one cell by the
/// same deterministic renderer used for UI regression captures.
pub fn capture_rendered_cell_colors(
    shell: &Shell,
    state: &GameState,
    columns: u16,
    rows: u16,
    column: u16,
    row: u16,
) -> Option<(Color, Color)> {
    if columns == 0 || rows == 0 || column >= columns || row >= rows {
        return None;
    }
    let mut rendered_shell = shell.clone();
    let backend = TestBackend::new(columns, rows);
    let mut terminal = Terminal::new(backend).ok()?;
    terminal
        .draw(|frame| render_frame(frame, &mut rendered_shell, state))
        .ok()?;
    let index = usize::from(row)
        .checked_mul(usize::from(columns))?
        .checked_add(usize::from(column))?;
    let cell = terminal.backend().buffer().content().get(index)?;
    Some((cell.fg, cell.bg))
}
