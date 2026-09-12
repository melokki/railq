//! Startup and onboarding for a Player Company.
//!
//! This module decides whether a save opens the dashboard or begins
//! onboarding. Terminal input and output remain at the binary boundary, while
//! these types keep startup behaviour deterministic and testable. The themed
//! name field owns only presentation and returns a validated name to the
//! existing preparation and save path at the binary boundary.

use std::{error::Error, fmt, io};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame, Terminal,
    backend::TestBackend,
    layout::Rect,
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};
use unicode_width::UnicodeWidthChar;

use crate::{
    app::{App, AppError, GameStore},
    model::{GameState, UtcSeconds},
    sim::time::SettledJourney,
    sim::world::create_new_game,
};

use super::{TerminalSession, format, theme};

/// The maximum visible length of a Player Company name.
pub const MAXIMUM_COMPANY_NAME_CHARACTERS: usize = 60;

const MINIMUM_FORM_COLUMNS: u16 = 48;
const MINIMUM_FORM_ROWS: u16 = 12;
const MINIMUM_REVIEW_COLUMNS: u16 = 64;
const MINIMUM_REVIEW_ROWS: u16 = 22;

/// A presentation-only editable field for naming a new Player Company.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CompanyNameForm {
    draft: String,
    cursor: usize,
    scroll_cells: u16,
    validation_error: Option<CompanyNameError>,
    can_submit: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum CompanyNameFormAction {
    Continue,
    Submit(CompanyName),
    Cancel,
}

impl CompanyNameForm {
    fn from_name(name: &CompanyName) -> Self {
        let draft = name.as_str().to_owned();
        let cursor = draft.chars().count();
        Self {
            draft,
            cursor,
            scroll_cells: 0,
            validation_error: None,
            can_submit: true,
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> CompanyNameFormAction {
        if key.kind != KeyEventKind::Press {
            return CompanyNameFormAction::Continue;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return CompanyNameFormAction::Cancel;
        }

        match key.code {
            KeyCode::Esc => CompanyNameFormAction::Cancel,
            KeyCode::Enter if self.can_submit => match CompanyName::parse(&self.draft) {
                Ok(name) => CompanyNameFormAction::Submit(name),
                Err(error) => {
                    self.validation_error = Some(error);
                    CompanyNameFormAction::Continue
                }
            },
            KeyCode::Left => {
                self.cursor = self.cursor.saturating_sub(1);
                self.validation_error = None;
                CompanyNameFormAction::Continue
            }
            KeyCode::Right => {
                self.cursor = (self.cursor + 1).min(self.character_count());
                self.validation_error = None;
                CompanyNameFormAction::Continue
            }
            KeyCode::Home => {
                self.cursor = 0;
                self.validation_error = None;
                CompanyNameFormAction::Continue
            }
            KeyCode::End => {
                self.cursor = self.character_count();
                self.validation_error = None;
                CompanyNameFormAction::Continue
            }
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.remove_character(self.cursor);
                }
                self.validation_error = None;
                CompanyNameFormAction::Continue
            }
            KeyCode::Delete => {
                self.remove_character(self.cursor);
                self.validation_error = None;
                CompanyNameFormAction::Continue
            }
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.insert_character(character);
                self.validation_error = None;
                CompanyNameFormAction::Continue
            }
            _ => CompanyNameFormAction::Continue,
        }
    }

    fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();
        frame.render_widget(Block::default().style(theme::terminal()), area);
        self.can_submit = area.width >= MINIMUM_FORM_COLUMNS && area.height >= MINIMUM_FORM_ROWS;
        if !self.can_submit {
            frame.render_widget(
                Paragraph::new(
                    "RailQ\nResize to at least 48 × 12 to name your railway company.\n[Esc] Cancel",
                )
                .style(theme::terminal())
                .wrap(Wrap { trim: true }),
                area,
            );
            return;
        }

        let width = area.width.min(76);
        let height = area.height.min(14);
        let card = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme::focused_border())
            .style(theme::panel())
            .title(Line::from(" NEW RAILWAY COMPANY ").style(theme::focused_title()));
        let inner = block.inner(card);
        frame.render_widget(block, card);

        let input = Rect::new(inner.x, inner.y + 3, inner.width, 3);
        let content_width = input.width.saturating_sub(2);
        self.reveal_cursor(content_width);
        frame.render_widget(
            Paragraph::new(
                Line::from("A passenger operating concession is available in a newly generated region.")
                    .style(theme::primary_value()),
            )
            .wrap(Wrap { trim: true }),
            Rect::new(inner.x, inner.y, inner.width, 2),
        );
        frame.render_widget(
            Paragraph::new(self.visible_input(content_width))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(theme::focused_border())
                        .title(Line::from(" Company name ").style(theme::focused_title())),
                )
                .style(theme::primary_value()),
            input,
        );
        let feedback = self.validation_error.map_or_else(
            || "Name your railway company · Enter Continue · Esc Cancel".to_owned(),
            |error| error.to_string(),
        );
        frame.render_widget(
            Paragraph::new(feedback).style(if self.validation_error.is_some() {
                theme::error()
            } else {
                theme::secondary()
            }),
            Rect::new(inner.x, inner.y + 7, inner.width, 2),
        );
    }

    fn character_count(&self) -> usize {
        self.draft.chars().count()
    }

    fn insert_character(&mut self, character: char) {
        let byte_index = self.byte_index_for_character(self.cursor);
        self.draft.insert(byte_index, character);
        self.cursor += 1;
    }

    fn remove_character(&mut self, character_index: usize) {
        let Some(character) = self.draft.chars().nth(character_index) else {
            return;
        };
        let start = self.byte_index_for_character(character_index);
        self.draft.drain(start..start + character.len_utf8());
    }

    fn byte_index_for_character(&self, character_index: usize) -> usize {
        self.draft
            .char_indices()
            .nth(character_index)
            .map_or(self.draft.len(), |(index, _)| index)
    }

    fn cursor_cells(&self) -> u16 {
        self.draft.chars().take(self.cursor).map(cell_width).sum()
    }

    fn reveal_cursor(&mut self, content_width: u16) {
        let cursor = self.cursor_cells();
        let visible_content = content_width.saturating_sub(1);
        if cursor < self.scroll_cells {
            self.scroll_cells = cursor;
        } else if cursor > self.scroll_cells.saturating_add(visible_content) {
            self.scroll_cells = cursor.saturating_sub(visible_content);
        }
    }

    fn visible_input(&self, content_width: u16) -> Line<'static> {
        let cursor_cells = self.cursor_cells();
        let end = self
            .scroll_cells
            .saturating_add(content_width.saturating_sub(1));
        let mut cells: u16 = 0;
        let mut spans = Vec::new();
        for (index, character) in self.draft.chars().enumerate() {
            let width = cell_width(character);
            let character_end = cells.saturating_add(width);
            if index == self.cursor && cursor_cells >= self.scroll_cells && cursor_cells <= end {
                spans.push(Span::styled("▏", theme::focused_title()));
            }
            if cells >= self.scroll_cells && character_end <= end {
                spans.push(Span::styled(character.to_string(), theme::primary_value()));
            }
            cells = character_end;
        }
        if self.cursor == self.character_count()
            && cursor_cells >= self.scroll_cells
            && cursor_cells <= end
        {
            spans.push(Span::styled("▏", theme::focused_title()));
        }
        Line::from(spans)
    }
}

fn cell_width(character: char) -> u16 {
    u16::try_from(UnicodeWidthChar::width(character).unwrap_or(0)).unwrap_or(u16::MAX)
}

/// Opens the themed name form and returns `None` when onboarding is abandoned.
/// Terminal modes are restored before this function returns, including on I/O failure.
pub fn capture_company_name() -> io::Result<Option<CompanyName>> {
    let mut terminal = TerminalSession::enter()?;
    let mut form = CompanyNameForm::default();
    let result = (|| loop {
        terminal.draw(|frame| form.render(frame))?;
        match event::read()? {
            Event::Key(key) => match form.handle_key(key) {
                CompanyNameFormAction::Continue => {}
                CompanyNameFormAction::Submit(name) => return Ok(Some(name)),
                CompanyNameFormAction::Cancel => return Ok(None),
            },
            Event::Resize(_, _) => {}
            _ => {}
        }
    })();
    let restore_result = terminal.restore();
    match result {
        Ok(name) => {
            restore_result?;
            Ok(name)
        }
        Err(error) => {
            let _ = restore_result;
            Err(error)
        }
    }
}

/// Runs the complete new-game onboarding flow in one terminal session.
///
/// The Region seed and start timestamp remain fixed while the player moves
/// between company-name editing and concession review, so editing the name
/// never rerolls the generated world.
pub fn capture_new_game(
    world_seed: u64,
    started_at: UtcSeconds,
) -> io::Result<Option<GameState>> {
    let mut terminal = TerminalSession::enter()?;
    let mut form = CompanyNameForm::default();
    let mut prepared: Option<GameState> = None;
    let mut reviewing = false;

    let result = (|| loop {
        terminal.draw(|frame| {
            if reviewing {
                if let Some(state) = prepared.as_ref() {
                    render_concession_review(frame, state);
                }
            } else {
                form.render(frame);
            }
        })?;

        match event::read()? {
            Event::Key(key) if reviewing && key.kind == KeyEventKind::Press => {
                if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                    return Ok(None);
                }
                match key.code {
                    KeyCode::Enter => return Ok(prepared.take()),
                    KeyCode::Esc => {
                        if let Some(state) = prepared.as_ref() {
                            if let Ok(name) = CompanyName::parse(&state.player_company.name) {
                                form = CompanyNameForm::from_name(&name);
                            }
                        }
                        reviewing = false;
                    }
                    KeyCode::Char('q' | 'Q') => return Ok(None),
                    _ => {}
                }
            }
            Event::Key(key) if !reviewing => match form.handle_key(key) {
                CompanyNameFormAction::Continue => {}
                CompanyNameFormAction::Submit(name) => {
                    prepared = Some(create_new_game(world_seed, name.0, started_at));
                    reviewing = true;
                }
                CompanyNameFormAction::Cancel => return Ok(None),
            },
            Event::Resize(_, _) => {}
            _ => {}
        }
    })();

    let restore_result = terminal.restore();
    match result {
        Ok(state) => {
            restore_result?;
            Ok(state)
        }
        Err(error) => {
            let _ = restore_result;
            Err(error)
        }
    }
}

fn render_concession_review(frame: &mut Frame, state: &GameState) {
    let area = frame.area();
    frame.render_widget(Block::default().style(theme::terminal()), area);

    if area.width < MINIMUM_REVIEW_COLUMNS || area.height < MINIMUM_REVIEW_ROWS {
        frame.render_widget(
            Paragraph::new(
                "RailQ\nResize to at least 64 × 22 to review the passenger concession.\nEsc Back · q Cancel",
            )
            .style(theme::terminal())
            .wrap(Wrap { trim: true }),
            area,
        );
        return;
    }

    let width = area.width.min(88);
    let height = area.height.min(24);
    let card = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::focused_border())
        .style(theme::panel())
        .title(Line::from(" NEW PASSENGER CONCESSION ").style(theme::focused_title()));
    let inner = block.inner(card);
    frame.render_widget(block, card);

    let network = &state.region.rail_authority.rail_network;
    let network_metres = network
        .rail_lines
        .iter()
        .map(|line| line.distance.metres())
        .sum::<u64>();
    let content = vec![
        Line::from(Span::styled(
            state.region.name.clone(),
            theme::focused_title(),
        )),
        Line::from(""),
        metric_line("Population", grouped_number(state.region.population)),
        metric_line("Settlements", state.region.settlements.len().to_string()),
        metric_line("Rail stations", network.rail_stations.len().to_string()),
        metric_line("Rail network", format::distance(network_metres)),
        Line::from(""),
        Line::from(Span::styled(
            state.region.rail_authority.name.clone(),
            theme::title(),
        )),
        Line::from(vec![
            Span::styled("has awarded ", theme::secondary()),
            Span::styled(state.player_company.name.clone(), theme::primary_value()),
            Span::styled(
                " the passenger operating concession for the public Rail Network.",
                theme::secondary(),
            ),
        ]),
        Line::from(""),
        metric_line("Starting funds", format::money(state.player_company.funds)),
        metric_line("Fleet", format!("{} trains", state.player_company.fleet.trains.len())),
        Line::from(""),
        Line::from(Span::styled("First objective", theme::title())),
        Line::from(Span::styled(
            "Acquire your first passenger train from the Market.",
            theme::primary_value(),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("Enter", theme::focused_title()),
            Span::styled(" Start operations   ", theme::secondary()),
            Span::styled("Esc", theme::focused_title()),
            Span::styled(" Edit company name   ", theme::secondary()),
            Span::styled("q", theme::focused_title()),
            Span::styled(" Cancel", theme::secondary()),
        ]),
    ];

    frame.render_widget(
        Paragraph::new(content)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        inner,
    );
}

fn metric_line(label: &str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<18}"), theme::secondary()),
        Span::styled(value, theme::primary_value()),
    ])
}

fn grouped_number(value: u64) -> String {
    let digits = value.to_string();
    let mut result = String::with_capacity(digits.len().saturating_add(digits.len() / 3));
    for (index, digit) in digits.chars().enumerate() {
        if index != 0 && (digits.len() - index) % 3 == 0 {
            result.push(',');
        }
        result.push(digit);
    }
    result
}

/// Renders a deterministic concession-review buffer for focused UI checks.
pub fn capture_concession_review(state: &GameState, columns: u16, rows: u16) -> String {
    let backend = TestBackend::new(columns, rows);
    let mut terminal = match Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(error) => match error {},
    };
    if terminal
        .draw(|frame| render_concession_review(frame, state))
        .is_err()
    {
        return String::new();
    }
    terminal
        .backend()
        .buffer()
        .content
        .chunks(usize::from(columns))
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Renders a deterministic name-form buffer for focused UI checks and evidence.
pub fn capture_company_name_form(form: &mut CompanyNameForm, columns: u16, rows: u16) -> String {
    let backend = TestBackend::new(columns, rows);
    let mut terminal = match Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(error) => match error {},
    };
    if terminal.draw(|frame| form.render(frame)).is_err() {
        return String::new();
    }
    terminal
        .backend()
        .buffer()
        .content
        .chunks(usize::from(columns))
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// A validated Player Company name suitable for starting a new game.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompanyName(String);

impl CompanyName {
    /// Validates and normalizes a player-entered Player Company name.
    pub fn parse(value: &str) -> Result<Self, CompanyNameError> {
        let value = value.trim();
        if value.is_empty() {
            return Err(CompanyNameError::Empty);
        }
        if value.chars().count() > MAXIMUM_COMPANY_NAME_CHARACTERS {
            return Err(CompanyNameError::TooLong {
                maximum: MAXIMUM_COMPANY_NAME_CHARACTERS,
            });
        }
        if value.chars().any(char::is_control) {
            return Err(CompanyNameError::ContainsControlCharacter);
        }
        Ok(Self(value.to_owned()))
    }

    /// Returns the normalized Player Company name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Why a new Player Company name cannot be used.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompanyNameError {
    /// A name must include at least one non-whitespace character.
    Empty,
    /// Names must fit in the onboarding screen.
    TooLong { maximum: usize },
    /// Control characters would make terminal output ambiguous.
    ContainsControlCharacter,
}

impl fmt::Display for CompanyNameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(formatter, "Enter a railway company name."),
            Self::TooLong { maximum } => {
                write!(
                    formatter,
                    "Use at most {maximum} characters for the railway company name."
                )
            }
            Self::ContainsControlCharacter => {
                write!(
                    formatter,
                    "The railway company name cannot contain control characters."
                )
            }
        }
    }
}

impl Error for CompanyNameError {}

/// The startup destination selected from the local save slot.
#[derive(Debug)]
pub enum Startup<S> {
    /// A validated Player Company is ready for the terminal dashboard.
    Dashboard(StartupDashboard<S>),
    /// No save exists, so the player must name a new Player Company.
    Onboarding(Onboarding<S>),
}

/// The initial dashboard application and arrivals committed while loading.
#[derive(Debug)]
pub struct StartupDashboard<S> {
    app: Box<App<S>>,
    settled_arrivals: Vec<SettledJourney>,
}

impl<S> StartupDashboard<S> {
    /// Consumes the one-time startup handoff before entering the shell.
    pub fn into_parts(self) -> (Box<App<S>>, Vec<SettledJourney>) {
        (self.app, self.settled_arrivals)
    }
}

/// An exclusively owned empty save slot awaiting a new Player Company.
#[derive(Debug)]
pub struct Onboarding<S> {
    store: S,
}

impl<S: GameStore> Onboarding<S> {
    /// Builds a named game so its Region and Concession can be shown before it
    /// is persisted. The caller must use [`Self::save`] before opening play.
    pub fn prepare_company(
        &self,
        company_name: CompanyName,
        world_seed: u64,
        started_at: UtcSeconds,
    ) -> GameState {
        create_new_game(world_seed, company_name.0, started_at)
    }

    /// Persists the prepared Player Company before returning the dashboard.
    pub fn save(self, state: GameState) -> Result<App<S>, StartupError<S::Error>> {
        App::start_new(self.store, state).map_err(StartupError::Save)
    }
}

/// Loads an existing Player Company, or starts onboarding only when no save
/// exists. A malformed save is never treated as a missing save.
pub fn start<S: GameStore>(
    store: S,
    now: UtcSeconds,
) -> Result<Startup<S>, StartupError<S::Error>> {
    match App::load_or_empty(store, now).map_err(StartupError::Load)? {
        Ok(loaded) => {
            let (app, settled_arrivals) = loaded.into_parts();
            Ok(Startup::Dashboard(StartupDashboard {
                app: Box::new(app),
                settled_arrivals,
            }))
        }
        Err(store) => Ok(Startup::Onboarding(Onboarding { store })),
    }
}

/// A startup failure with recovery guidance for the player.
#[derive(Debug)]
pub enum StartupError<E> {
    /// An existing save could not be loaded or reconciled.
    Load(AppError<E>),
    /// A new Player Company could not be persisted.
    Save(AppError<E>),
}

impl<E: fmt::Display> fmt::Display for StartupError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Load(error) => write!(
                formatter,
                "Could not load the existing Player Company: {error}. The save was not reset or overwritten. Repair or move the save aside, then restart RailQ."
            ),
            Self::Save(error) => write!(
                formatter,
                "Could not save the new Player Company: {error}. Resolve the storage problem and try again; the dashboard has not opened."
            ),
        }
    }
}

impl<E: Error + 'static> Error for StartupError<E> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Load(error) | Self::Save(error) => Some(error),
        }
    }
}

/// Formats the generated Region and Concession for non-interactive callers.
pub fn onboarding_summary(state: &GameState) -> String {
    let network = &state.region.rail_authority.rail_network;
    let network_metres = network
        .rail_lines
        .iter()
        .map(|line| line.distance.metres())
        .sum::<u64>();
    format!(
        "Region: {}\nPopulation: {}\nSettlements: {}\nRail stations: {}\nRail network: {}\n\nConcession: {} has awarded {} the right to operate passenger railway services over the public Rail Network.\nStarting funds: {}",
        state.region.name,
        grouped_number(state.region.population),
        state.region.settlements.len(),
        network.rail_stations.len(),
        format::distance(network_metres),
        state.region.rail_authority.name,
        state.player_company.name,
        format::money(state.player_company.funds),
    )
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, error::Error, fmt, fs, path::Path, rc::Rc};

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend};

    use crate::{
        app::GameStore,
        model::{GameState, UtcSeconds},
        sim::world::create_new_game,
    };

    use super::{
        CompanyName, CompanyNameError, CompanyNameForm, CompanyNameFormAction, Startup,
        capture_company_name_form, capture_concession_review, start,
    };
    use crate::ui::theme;

    const STARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(1_700_000_000);

    #[derive(Clone, Debug, Default)]
    struct TestStore {
        saved: Rc<RefCell<Option<GameState>>>,
        load_error: Rc<RefCell<Option<TestStoreError>>>,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum TestStoreError {
        MalformedSave,
    }

    impl fmt::Display for TestStoreError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(formatter, "save is malformed and was preserved")
        }
    }

    impl Error for TestStoreError {}

    impl GameStore for TestStore {
        type Error = TestStoreError;

        fn load(&self) -> Result<Option<GameState>, Self::Error> {
            match *self.load_error.borrow() {
                Some(error) => Err(error),
                None => Ok(self.saved.borrow().clone()),
            }
        }

        fn save(&self, state: &GameState) -> Result<(), Self::Error> {
            self.saved.replace(Some(state.clone()));
            Ok(())
        }
    }

    #[test]
    fn company_name_is_trimmed_and_rejects_invalid_input() {
        assert_eq!(
            CompanyName::parse("  Alden Passenger  ").unwrap().as_str(),
            "Alden Passenger"
        );
        assert_eq!(CompanyName::parse("  "), Err(CompanyNameError::Empty));
        assert_eq!(
            CompanyName::parse("A\nB"),
            Err(CompanyNameError::ContainsControlCharacter)
        );
        assert!(matches!(
            CompanyName::parse(&"A".repeat(61)),
            Err(CompanyNameError::TooLong { maximum: 60 })
        ));
    }

    #[test]
    fn name_form_edits_unicode_at_the_caret_and_keeps_reserved_shortcuts_as_text() {
        let mut form = CompanyNameForm::default();
        let _ = capture_company_name_form(&mut form, 80, 24);
        for character in ['甲', '乙'] {
            assert_eq!(
                form.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE)),
                CompanyNameFormAction::Continue
            );
        }
        form.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        form.handle_key(KeyEvent::new(KeyCode::Char('Q'), KeyModifiers::NONE));
        form.handle_key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
        form.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        form.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        for character in ['q', 'h', 'm', 'b'] {
            form.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }

        assert_eq!(form.draft, "甲qhmb");
        assert_eq!(form.cursor, 5);
        assert_eq!(
            form.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            CompanyNameFormAction::Submit(CompanyName::parse("甲qhmb").unwrap())
        );
    }

    #[test]
    fn name_form_scrolls_by_cells_and_shows_validation_cancellation_and_focused_style() {
        let evidence = Path::new("tmp/ui-ux-plan/evidence/26");
        fs::create_dir_all(evidence).expect("evidence directory");

        let mut form = CompanyNameForm::default();
        let _ = capture_company_name_form(&mut form, 48, 12);
        for _ in 0..30 {
            form.handle_key(KeyEvent::new(KeyCode::Char('界'), KeyModifiers::NONE));
        }
        let compact = capture_company_name_form(&mut form, 48, 12);
        assert!(
            form.scroll_cells > 0,
            "a wide Unicode name should scroll in cells"
        );
        assert!(compact.contains("▏"));
        fs::write(evidence.join("company-name-form-48x12.txt"), &compact)
            .expect("compact evidence");

        let normal = capture_company_name_form(&mut form, 80, 24);
        assert!(normal.contains("NEW RAILWAY COMPANY"));
        fs::write(evidence.join("company-name-form-80x24.txt"), &normal).expect("normal evidence");

        form.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        let wide = capture_company_name_form(&mut form, 120, 40);
        assert_eq!(form.scroll_cells, 0);
        assert!(wide.contains("NEW RAILWAY COMPANY"));
        assert!(wide.contains("Company name"));
        fs::write(evidence.join("company-name-form-120x40.txt"), &wide).expect("wide evidence");

        let mut styled = CompanyNameForm::default();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("test terminal");
        terminal
            .draw(|frame| styled.render(frame))
            .expect("render form");
        assert!(
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .any(|cell| { cell.fg == theme::ACCENT && cell.bg == theme::PANEL })
        );
        assert_eq!(
            styled.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            CompanyNameFormAction::Continue
        );
        assert_eq!(styled.validation_error, Some(CompanyNameError::Empty));
        assert_eq!(
            styled.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            CompanyNameFormAction::Cancel
        );
        assert_eq!(
            styled.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            CompanyNameFormAction::Cancel
        );
    }

    #[test]
    fn returning_from_review_prefills_the_existing_company_name() {
        let name = CompanyName::parse("Northstar Rail").unwrap();
        let form = CompanyNameForm::from_name(&name);

        assert_eq!(form.draft, "Northstar Rail");
        assert_eq!(form.cursor, "Northstar Rail".chars().count());
        assert!(form.validation_error.is_none());
    }

    #[test]
    fn onboarding_shows_region_and_concession_then_persists_before_dashboard() {
        let store = TestStore::default();
        let Startup::Onboarding(onboarding) = start(store.clone(), STARTED_AT).unwrap() else {
            panic!("a missing save starts onboarding");
        };
        let game = onboarding.prepare_company(
            CompanyName::parse("Alden Passenger").unwrap(),
            42,
            STARTED_AT,
        );
        let summary = super::onboarding_summary(&game);
        let review = capture_concession_review(&game, 100, 28);

        assert!(summary.contains(&format!("Region: {}", game.region.name)));
        assert!(summary.contains("Settlements: 10"));
        assert!(summary.contains("Rail stations: 4"));
        assert!(summary.contains("Rail network: 83 km"));
        assert!(summary.contains("Starting funds: $"));
        assert!(review.contains("NEW PASSENGER CONCESSION"));
        assert!(review.contains(&game.region.name));
        assert!(review.contains(&game.region.rail_authority.name));
        assert!(review.contains("First objective"));
        assert!(review.contains("Acquire your first passenger train from the Market."));
        assert!(review.contains("Start operations"));
        assert!(store.saved.borrow().is_none());

        let app = onboarding.save(game.clone()).unwrap();
        assert_eq!(app.state(), &game);
        assert_eq!(*store.saved.borrow(), Some(game));
    }

    #[test]
    fn an_existing_player_company_loads_directly_to_dashboard() {
        let saved_game = create_new_game(42, "Existing Passenger", STARTED_AT);
        let store = TestStore {
            saved: Rc::new(RefCell::new(Some(saved_game.clone()))),
            ..TestStore::default()
        };

        let Startup::Dashboard(app) = start(store, STARTED_AT).unwrap() else {
            panic!("an existing save opens the dashboard");
        };
        let (app, settled_arrivals) = app.into_parts();
        assert_eq!(app.state(), &saved_game);
        assert!(settled_arrivals.is_empty());
    }

    #[test]
    fn malformed_save_has_recovery_guidance_and_never_starts_onboarding() {
        let store = TestStore {
            load_error: Rc::new(RefCell::new(Some(TestStoreError::MalformedSave))),
            ..TestStore::default()
        };

        let error = start(store, STARTED_AT).unwrap_err().to_string();
        assert!(error.contains("save is malformed and was preserved"));
        assert!(error.contains("not reset or overwritten"));
        assert!(error.contains("Repair or move the save aside"));
    }
}
