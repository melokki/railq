//! Terminal shell and shared navigation state.
//!
//! This module deliberately owns terminal I/O only. Game actions remain at the
//! application boundary, where callers supply an elapsed-time reconciliation
//! callback before the shell accepts each input event.

use std::{
    error::Error,
    fmt,
    io::{self, Stdout},
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crossterm::{
    cursor::{Hide, Show},
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Layout},
    style::{Color, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph, Tabs, Wrap},
};

use crate::{
    APPLICATION_NAME,
    model::{GameState, RailStationId, TrainId, UtcSeconds},
    sim::finance::{FinancialStatus, evaluate_financial_recovery},
};

pub mod company;
pub mod dispatch;
pub mod fleet;
pub mod map;
pub mod market;
pub mod start;

/// How frequently the shell checks for elapsed arrivals while no key is pressed.
pub const ARRIVAL_POLL_INTERVAL: Duration = Duration::from_millis(250);

const MINIMUM_COLUMNS: u16 = 64;
const MINIMUM_ROWS: u16 = 16;

/// The four primary views in the RailQ terminal shell.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum View {
    /// The Rail Network overview and operational home.
    #[default]
    Map,
    /// The Player Company's Fleet.
    Trains,
    /// The Player Company's financial position.
    Company,
    /// The Train catalogue and purchase flow.
    BuyTrains,
}

impl View {
    const fn label(self) -> &'static str {
        match self {
            Self::Map => "Map",
            Self::Trains => "Trains",
            Self::Company => "Company",
            Self::BuyTrains => "Buy Trains",
        }
    }
}

/// The outcome of a keyboard event handled by the shell.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ShellAction {
    /// Continue running; the active view may have changed.
    Continue,
    /// Exit the terminal shell.
    Exit,
    /// Confirmed player input requiring an application-boundary Manual Dispatch.
    ManualDispatch {
        train_id: TrainId,
        destination_station_id: RailStationId,
    },
    /// Confirmed player input requiring an application-boundary Train purchase.
    PurchaseTrain {
        catalogue_index: usize,
        delivery_station_id: RailStationId,
    },
    /// Confirmed player input requiring an application-boundary Train resale.
    SellTrain { train_id: TrainId },
    /// Confirmed Bankruptcy restart requiring an archived-save application action.
    RestartAfterBankruptcy,
}

/// One command accepted by the terminal shell at the application boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalCommand {
    /// Reconcile elapsed demand and due Journey arrivals before presentation or input.
    Reconcile { now: UtcSeconds },
    /// Revalidate and authorise a player-requested Manual Dispatch.
    ManualDispatch {
        train_id: TrainId,
        destination_station_id: RailStationId,
        now: UtcSeconds,
    },
    /// Revalidate and purchase a selected catalogue Train.
    PurchaseTrain {
        catalogue_index: usize,
        delivery_station_id: RailStationId,
        now: UtcSeconds,
    },
    /// Revalidate and resell a selected READY Train.
    SellTrain { train_id: TrainId, now: UtcSeconds },
    /// Archive the Bankrupt Player Company save and start a fresh game.
    RestartAfterBankruptcy { world_seed: u64, now: UtcSeconds },
}

/// Presentation-only state shared by the four primary views.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Shell {
    active_view: View,
    dispatch_flow: Option<dispatch::DispatchFlow>,
    fleet_flow: Option<fleet::FleetFlow>,
    market_flow: Option<market::MarketFlow>,
    notice: Option<String>,
    help_visible: bool,
    restart_confirmation: bool,
}

impl Shell {
    /// Creates a shell with Map as the primary view.
    pub const fn new() -> Self {
        Self {
            active_view: View::Map,
            dispatch_flow: None,
            fleet_flow: None,
            market_flow: None,
            notice: None,
            help_visible: false,
            restart_confirmation: false,
        }
    }

    /// Returns the selected primary view.
    pub fn active_view(&self) -> View {
        self.active_view
    }

    /// Returns whether the keyboard help overlay is currently visible.
    pub fn help_visible(&self) -> bool {
        self.help_visible
    }

    /// Routes global navigation and exit keys.
    pub fn handle_key(&mut self, key: KeyEvent, state: &GameState) -> ShellAction {
        if key.kind != KeyEventKind::Press {
            return ShellAction::Continue;
        }

        if matches!(key.code, KeyCode::Char('q' | 'Q'))
            || (matches!(key.code, KeyCode::Char('c' | 'C'))
                && key.modifiers.contains(KeyModifiers::CONTROL))
        {
            return ShellAction::Exit;
        }

        if self.help_visible {
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('?' | 'h' | 'H')) {
                self.help_visible = false;
            }
            return ShellAction::Continue;
        }

        if matches!(key.code, KeyCode::Char('?' | 'h' | 'H')) {
            self.help_visible = true;
            return ShellAction::Continue;
        }

        if is_bankrupt(state) {
            if self.restart_confirmation {
                return match key.code {
                    KeyCode::Enter => ShellAction::RestartAfterBankruptcy,
                    KeyCode::Esc => {
                        self.restart_confirmation = false;
                        self.notice = Some("Safe restart cancelled; the Bankrupt Player Company save is unchanged.".into());
                        ShellAction::Continue
                    }
                    _ => ShellAction::Continue,
                };
            }
            if matches!(key.code, KeyCode::Char('r' | 'R')) {
                self.restart_confirmation = true;
                self.notice = None;
            } else {
                self.notice = Some(
                    "Bankruptcy prevents normal operations. Press R for a safe restart or Q to exit."
                        .into(),
                );
            }
            return ShellAction::Continue;
        }

        self.restart_confirmation = false;

        if let Some(flow) = &mut self.dispatch_flow {
            return match flow.handle_key(key, state) {
                dispatch::DispatchFlowAction::Continue => ShellAction::Continue,
                dispatch::DispatchFlowAction::Cancel => {
                    self.dispatch_flow = None;
                    self.notice = Some("Manual Dispatch cancelled; no changes were made.".into());
                    ShellAction::Continue
                }
                dispatch::DispatchFlowAction::Confirm {
                    train_id,
                    destination_station_id,
                } => ShellAction::ManualDispatch {
                    train_id,
                    destination_station_id,
                },
            };
        }

        if let Some(flow) = &mut self.fleet_flow {
            return match flow.handle_key(key, state) {
                fleet::FleetFlowAction::Continue => ShellAction::Continue,
                fleet::FleetFlowAction::Cancel => {
                    self.fleet_flow = None;
                    self.notice = Some("Train resale cancelled; no changes were made.".into());
                    ShellAction::Continue
                }
                fleet::FleetFlowAction::Confirm { train_id } => ShellAction::SellTrain { train_id },
            };
        }

        if let Some(flow) = &mut self.market_flow {
            return match flow.handle_key(key, state) {
                market::MarketFlowAction::Continue => ShellAction::Continue,
                market::MarketFlowAction::Cancel => {
                    self.market_flow = None;
                    self.notice = Some("Train purchase cancelled; no changes were made.".into());
                    ShellAction::Continue
                }
                market::MarketFlowAction::Confirm {
                    catalogue_index,
                    delivery_station_id,
                } => ShellAction::PurchaseTrain {
                    catalogue_index,
                    delivery_station_id,
                },
            };
        }

        match key.code {
            KeyCode::Char('m' | 'M') => self.active_view = View::Map,
            KeyCode::Char('t' | 'T') => self.active_view = View::Trains,
            KeyCode::Char('c' | 'C') => self.active_view = View::Company,
            KeyCode::Char('b' | 'B') => self.active_view = View::BuyTrains,
            KeyCode::Enter if self.active_view == View::BuyTrains => {
                match market::MarketFlow::start(state) {
                    Ok(flow) => {
                        self.market_flow = Some(flow);
                        self.notice = None;
                    }
                    Err(message) => self.notice = Some(message.into()),
                }
            }
            KeyCode::Enter if self.active_view == View::Trains => {
                match fleet::FleetFlow::start(state) {
                    Ok(flow) => {
                        self.fleet_flow = Some(flow);
                        self.notice = None;
                    }
                    Err(message) => self.notice = Some(message.into()),
                }
            }
            KeyCode::Char('d' | 'D') if self.active_view == View::Map => {
                match dispatch::DispatchFlow::start(state) {
                    Ok(flow) => {
                        self.dispatch_flow = Some(flow);
                        self.notice = None;
                    }
                    Err(message) => self.notice = Some(message.into()),
                }
            }
            _ => {}
        }
        ShellAction::Continue
    }

    /// Keeps a rejected confirmation visible to explain the actual current-state cause.
    pub fn reject_manual_dispatch(&mut self, error: impl Into<String>) {
        if let Some(flow) = &mut self.dispatch_flow {
            flow.reject(error);
        } else {
            self.notice = Some(error.into());
        }
    }

    /// Closes a successful proposal after the application boundary persisted it.
    pub fn confirm_manual_dispatch(&mut self) {
        self.dispatch_flow = None;
        self.notice = Some("Manual Dispatch authorised and saved.".into());
    }

    /// Keeps a rejected purchase visible to explain the actual current-state cause.
    pub fn reject_purchase_train(&mut self, error: impl Into<String>) {
        if let Some(flow) = &mut self.market_flow {
            flow.reject(error);
        } else {
            self.notice = Some(error.into());
        }
    }

    /// Closes a successful purchase proposal after the application boundary persisted it.
    pub fn confirm_purchase_train(&mut self) {
        self.market_flow = None;
        self.notice = Some("Train purchase authorised and saved to the Fleet.".into());
    }

    /// Keeps a rejected resale visible to explain the actual current-state cause.
    pub fn reject_train_resale(&mut self, error: impl Into<String>) {
        if let Some(flow) = &mut self.fleet_flow {
            flow.reject(error);
        } else {
            self.notice = Some(error.into());
        }
    }

    /// Closes a saved resale and shows the actual proceeds credited to Company Funds.
    pub fn confirm_train_resale(&mut self, proceeds: crate::model::Money) {
        self.fleet_flow = None;
        self.notice = Some(format!(
            "Train resold and saved. Sale proceeds of {} were added to Company Funds.",
            format_money(proceeds)
        ));
    }

    /// Shows the persisted outcome of an explicitly confirmed Bankruptcy restart.
    pub fn confirm_restart_after_bankruptcy(&mut self) {
        self.active_view = View::Map;
        self.dispatch_flow = None;
        self.fleet_flow = None;
        self.market_flow = None;
        self.restart_confirmation = false;
        self.notice = Some(
            "Fresh game saved. The former Player Company save was preserved in a restart backup."
                .into(),
        );
    }

    /// Keeps a failed safe restart actionable without losing the prior save.
    pub fn reject_restart_after_bankruptcy(&mut self, error: impl Into<String>) {
        self.restart_confirmation = true;
        self.notice = Some(format!(
            "Safe restart failed; the existing save was not overwritten: {}",
            error.into()
        ));
    }

    /// Returns a readable instruction when the terminal cannot safely fit a view.
    pub fn resize_hint(&self, columns: u16, rows: u16) -> Option<&'static str> {
        if columns < MINIMUM_COLUMNS || rows < MINIMUM_ROWS {
            Some("Terminal too small — resize to at least 64 columns by 16 rows.")
        } else {
            None
        }
    }
}

/// An error from the terminal shell or its supplied reconciliation boundary.
#[derive(Debug)]
pub enum RunError<E> {
    /// Terminal input, output, or mode configuration failed.
    Terminal(io::Error),
    /// Reconciliation failed before the shell could accept input.
    Reconcile(E),
}

impl<E: fmt::Display> fmt::Display for RunError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Terminal(error) => write!(formatter, "terminal error: {error}"),
            Self::Reconcile(error) => {
                write!(formatter, "could not reconcile elapsed time: {error}")
            }
        }
    }
}

impl<E: Error + 'static> Error for RunError<E> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Terminal(error) => Some(error),
            Self::Reconcile(error) => Some(error),
        }
    }
}

/// Runs the four-view terminal shell until the player exits.
///
/// `command` is invoked while idle and immediately before every input event,
/// so a Player Company's due Journeys settle before the next action is accepted.
/// It also owns persisted Manual Dispatch confirmation. The terminal is restored
/// on ordinary errors and while unwinding a panic.
pub fn run_terminal<E>(
    initial_state: GameState,
    mut command: impl FnMut(TerminalCommand) -> Result<GameState, E>,
) -> Result<(), RunError<E>>
where
    E: fmt::Display,
{
    let mut terminal = TerminalSession::enter().map_err(RunError::Terminal)?;
    let result = catch_unwind(AssertUnwindSafe(|| {
        run_event_loop(&mut terminal, initial_state, &mut command)
    }));
    let restore_result = terminal.restore();

    match result {
        Ok(Ok(())) => restore_result.map_err(RunError::Terminal),
        Ok(Err(error)) => {
            restore_result.map_err(RunError::Terminal)?;
            Err(error)
        }
        Err(payload) => {
            let _ = restore_result;
            resume_unwind(payload);
        }
    }
}

fn run_event_loop<E>(
    terminal: &mut TerminalSession,
    mut state: GameState,
    command: &mut impl FnMut(TerminalCommand) -> Result<GameState, E>,
) -> Result<(), RunError<E>>
where
    E: fmt::Display,
{
    let mut shell = Shell::new();

    loop {
        terminal.draw(&shell, &state).map_err(RunError::Terminal)?;
        state = command(TerminalCommand::Reconcile {
            now: current_utc_seconds(),
        })
        .map_err(RunError::Reconcile)?;

        if !event::poll(ARRIVAL_POLL_INTERVAL).map_err(RunError::Terminal)? {
            continue;
        }

        match event::read().map_err(RunError::Terminal)? {
            Event::Key(key) => {
                state = command(TerminalCommand::Reconcile {
                    now: current_utc_seconds(),
                })
                .map_err(RunError::Reconcile)?;
                match shell.handle_key(key, &state) {
                    ShellAction::Exit => return Ok(()),
                    ShellAction::ManualDispatch {
                        train_id,
                        destination_station_id,
                    } => match command(TerminalCommand::ManualDispatch {
                        train_id,
                        destination_station_id,
                        now: current_utc_seconds(),
                    }) {
                        Ok(next_state) => {
                            state = next_state;
                            shell.confirm_manual_dispatch();
                        }
                        Err(error) => shell.reject_manual_dispatch(error.to_string()),
                    },
                    ShellAction::PurchaseTrain {
                        catalogue_index,
                        delivery_station_id,
                    } => match command(TerminalCommand::PurchaseTrain {
                        catalogue_index,
                        delivery_station_id,
                        now: current_utc_seconds(),
                    }) {
                        Ok(next_state) => {
                            state = next_state;
                            shell.confirm_purchase_train();
                        }
                        Err(error) => shell.reject_purchase_train(error.to_string()),
                    },
                    ShellAction::SellTrain { train_id } => {
                        let expected_proceeds = resale_proceeds_for(&state, train_id);
                        match command(TerminalCommand::SellTrain {
                            train_id,
                            now: current_utc_seconds(),
                        }) {
                            Ok(next_state) => {
                                state = next_state;
                                shell.confirm_train_resale(expected_proceeds);
                            }
                            Err(error) => shell.reject_train_resale(error.to_string()),
                        }
                    }
                    ShellAction::RestartAfterBankruptcy => {
                        match command(TerminalCommand::RestartAfterBankruptcy {
                            world_seed: restart_seed(),
                            now: current_utc_seconds(),
                        }) {
                            Ok(next_state) => {
                                state = next_state;
                                shell.confirm_restart_after_bankruptcy();
                            }
                            Err(error) => shell.reject_restart_after_bankruptcy(error.to_string()),
                        }
                    }
                    ShellAction::Continue => {}
                }
            }
            Event::Resize(_, _) => {}
            _ => {}
        }
    }
}

fn current_utc_seconds() -> UtcSeconds {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    UtcSeconds::from_unix_seconds(i64::try_from(seconds).unwrap_or(i64::MAX))
}

fn restart_seed() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let nanos = u64::try_from(nanos).unwrap_or(u64::MAX);
    nanos ^ u64::from(std::process::id())
}

fn is_bankrupt(state: &GameState) -> bool {
    matches!(
        evaluate_financial_recovery(state),
        Ok(evaluation) if evaluation.status == FinancialStatus::Bankruptcy
    )
}

/// Draws the complete dashboard with Ratatui widgets. Crossterm supplies the
/// cross-platform terminal backend and events; Ratatui owns layout and paint.
fn render_frame(frame: &mut ratatui::Frame, shell: &Shell, state: &GameState) {
    let area = frame.area();
    if let Some(hint) = shell.resize_hint(area.width, area.height) {
        frame.render_widget(
            Paragraph::new(hint)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(APPLICATION_NAME),
                )
                .wrap(Wrap { trim: true }),
            area,
        );
        return;
    }

    let [navigation_area, content_area, footer_area] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(8),
        Constraint::Length(3),
    ])
    .areas(area);
    let views = [View::Map, View::Trains, View::Company, View::BuyTrains];
    let selected = views
        .iter()
        .position(|view| *view == shell.active_view)
        .unwrap_or(0);
    let titles = views
        .iter()
        .map(|view| Line::from(view.label()))
        .collect::<Vec<_>>();
    frame.render_widget(
        Tabs::new(titles)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(APPLICATION_NAME),
            )
            .select(selected)
            .highlight_style(Style::default().fg(Color::Cyan).bold())
            .divider(" | "),
        navigation_area,
    );

    let now = state.last_processed_at;
    let content = if shell.help_visible {
        help_text()
    } else if is_bankrupt(state) {
        bankruptcy_text(shell.restart_confirmation)
    } else {
        match shell.active_view {
            View::Map => match &shell.dispatch_flow {
                Some(flow) => flow.render(state),
                None => map::render_at(state, now),
            },
            View::Trains => match &shell.fleet_flow {
                Some(flow) => flow.render(state, now),
                None => fleet::render_at(state, now),
            },
            View::BuyTrains => match &shell.market_flow {
                Some(flow) => flow.render(state),
                None => market::render(state),
            },
            View::Company => company::render(state),
        }
    };
    frame.render_widget(
        Paragraph::new(content)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(shell.active_view.label()),
            )
            .wrap(Wrap { trim: false }),
        content_area,
    );

    let controls = if shell.help_visible {
        "[? / H / Esc] Close help  [Q] Quit"
    } else if is_bankrupt(state) {
        if shell.restart_confirmation {
            "[Enter] Confirm safe restart  [Esc] Cancel  [Q] Quit"
        } else {
            "[R] Safe restart  [Q] Quit  [?] Help"
        }
    } else {
        "[M] Map  [T] Fleet  [C] Company  [B] Buy Trains  [D] Dispatch on Map  [Enter] Select / confirm  [Esc] Cancel  [?] Help  [Q] Quit"
    };
    let footer = match &shell.notice {
        Some(notice) => format!("{notice}\n{controls}"),
        None => controls.into(),
    };
    frame.render_widget(
        Paragraph::new(footer)
            .style(Style::default().fg(Color::Gray))
            .wrap(Wrap { trim: true }),
        footer_area,
    );
}

fn help_text() -> String {
    [
        "Keyboard help",
        "",
        "[M] Map — inspect the Region and press [D] to begin a Manual Dispatch.",
        "[T] Fleet — press [Enter] to select a READY Train for resale.",
        "[C] Company — inspect Company Funds, receipts, Insolvency, and recovery options.",
        "[B] Buy Trains — press [Enter] to choose a diesel Train and delivery Rail Station.",
        "[Up]/[Down] or [J]/[K] change a selection; [Enter] advances or confirms; [Esc] cancels.",
        "[Q] or Ctrl-C exits RailQ. During Bankruptcy, [R] begins a confirmed safe restart that preserves the old save.",
        "",
        "Press [?], [H], or [Esc] to return.",
    ]
    .join("\n")
}

fn bankruptcy_text(restart_confirmation: bool) -> String {
    if restart_confirmation {
        [
            "BANKRUPTCY",
            "No finite sell, retain, rebuy, and dispatch option can return the Player Company to operation.",
            "",
            "Safe restart will create a fresh game only after preserving this Player Company save in a unique backup file.",
            "Press Enter to confirm the safe restart, Esc to keep the Bankrupt save, or Q to exit.",
        ]
        .join("\n")
    } else {
        [
            "BANKRUPTCY",
            "No finite sell, retain, rebuy, and dispatch option can return the Player Company to operation.",
            "Normal operations are disabled. You may exit safely or start a fresh game.",
            "",
            "Press R to begin a safe restart. The existing Player Company save will be preserved; it is never silently overwritten.",
            "Press Q to exit or ? for keyboard help.",
        ]
        .join("\n")
    }
}

fn format_money(money: crate::model::Money) -> String {
    let cents = i128::from(money.cents());
    let sign = if cents < 0 { "-" } else { "" };
    let cents = cents.abs();
    format!("{sign}${}.{:02}", cents / 100, cents % 100)
}

fn resale_proceeds_for(state: &GameState, train_id: TrainId) -> crate::model::Money {
    state
        .player_company
        .fleet
        .trains
        .iter()
        .find(|train| train.id == train_id)
        .and_then(|train| {
            train
                .original_purchase_price
                .cents()
                .checked_mul(70)
                .map(|cents| crate::model::Money::from_cents(cents / 100))
        })
        .unwrap_or(crate::model::Money::ZERO)
}

/// RAII guard for the terminal modes owned by the shell.
struct TerminalSession {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    active: bool,
}

impl TerminalSession {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen, Hide) {
            let _ = execute!(stdout, Show, LeaveAlternateScreen);
            let _ = disable_raw_mode();
            return Err(error);
        }

        let terminal = match Terminal::new(CrosstermBackend::new(stdout)) {
            Ok(terminal) => terminal,
            Err(error) => {
                let mut stdout = io::stdout();
                let _ = execute!(stdout, Show, LeaveAlternateScreen);
                let _ = disable_raw_mode();
                return Err(error);
            }
        };

        Ok(Self {
            terminal,
            active: true,
        })
    }

    fn draw(&mut self, shell: &Shell, state: &GameState) -> io::Result<()> {
        self.terminal
            .draw(|frame| render_frame(frame, shell, state))
            .map(|_| ())
    }

    fn restore(&mut self) -> io::Result<()> {
        if !self.active {
            return Ok(());
        }
        self.active = false;

        let raw_mode_result = disable_raw_mode();
        let screen_result = execute!(self.terminal.backend_mut(), Show, LeaveAlternateScreen);
        let cursor_result = self.terminal.show_cursor();
        raw_mode_result.and(screen_result).and(cursor_result)
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend};

    use crate::{
        model::{Money, RailStationId, UtcSeconds},
        sim::{fleet::purchase_train, world::create_new_game},
    };

    use super::{Shell, ShellAction, View};

    #[test]
    fn routes_the_four_primary_views() {
        let mut shell = Shell::new();
        let state = create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0));

        for (key, expected_view) in [
            ('t', View::Trains),
            ('c', View::Company),
            ('b', View::BuyTrains),
            ('m', View::Map),
        ] {
            assert_eq!(
                shell.handle_key(
                    KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE),
                    &state
                ),
                ShellAction::Continue
            );
            assert_eq!(shell.active_view(), expected_view);
        }
    }

    #[test]
    fn routes_quit_and_control_c_to_clean_exit() {
        let mut shell = Shell::new();
        let state = create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0));

        assert_eq!(
            shell.handle_key(
                KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
                &state
            ),
            ShellAction::Exit
        );
        assert_eq!(
            shell.handle_key(
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
                &state
            ),
            ShellAction::Exit
        );
    }

    #[test]
    fn small_terminal_gets_a_resize_hint() {
        assert!(Shell::new().resize_hint(63, 16).is_some());
        assert!(Shell::new().resize_hint(64, 15).is_some());
        assert!(Shell::new().resize_hint(64, 16).is_none());
    }

    #[test]
    fn narrow_terminal_frame_renders_resize_hint_without_panicking() {
        let backend = TestBackend::new(40, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        let shell = Shell::new();
        let state = create_new_game(42, "Narrow Passenger", UtcSeconds::from_unix_seconds(1_000));

        terminal
            .draw(|frame| super::render_frame(frame, &shell, &state))
            .unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(rendered.contains("Terminal too small"));
        assert!(rendered.contains("RailQ"));
    }

    #[test]
    fn map_routes_a_keyboard_manual_dispatch_to_the_application_boundary() {
        let mut state = create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0));
        let train_id = purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
        let mut shell = Shell::new();
        let press = |shell: &mut Shell, key| {
            shell.handle_key(KeyEvent::new(key, KeyModifiers::NONE), &state)
        };

        assert_eq!(press(&mut shell, KeyCode::Char('d')), ShellAction::Continue);
        assert_eq!(press(&mut shell, KeyCode::Enter), ShellAction::Continue);
        assert_eq!(press(&mut shell, KeyCode::Down), ShellAction::Continue);
        assert_eq!(press(&mut shell, KeyCode::Enter), ShellAction::Continue);
        assert_eq!(
            press(&mut shell, KeyCode::Enter),
            ShellAction::ManualDispatch {
                train_id,
                destination_station_id: RailStationId::new(3),
            }
        );
    }

    #[test]
    fn buy_trains_routes_only_an_explicit_confirmation_to_the_application_boundary() {
        let state = create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0));
        let mut shell = Shell::new();
        let press = |shell: &mut Shell, key| {
            shell.handle_key(KeyEvent::new(key, KeyModifiers::NONE), &state)
        };

        assert_eq!(press(&mut shell, KeyCode::Char('b')), ShellAction::Continue);
        assert_eq!(press(&mut shell, KeyCode::Enter), ShellAction::Continue);
        assert_eq!(press(&mut shell, KeyCode::Down), ShellAction::Continue);
        assert_eq!(press(&mut shell, KeyCode::Enter), ShellAction::Continue);
        assert_eq!(press(&mut shell, KeyCode::Enter), ShellAction::Continue);
        assert_eq!(
            press(&mut shell, KeyCode::Enter),
            ShellAction::PurchaseTrain {
                catalogue_index: 1,
                delivery_station_id: RailStationId::new(1),
            }
        );
    }

    #[test]
    fn fleet_resale_routes_only_a_confirmed_ready_train_to_the_application_boundary() {
        let mut state = create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0));
        let train_id = purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
        let mut shell = Shell::new();
        let press = |shell: &mut Shell, key| {
            shell.handle_key(KeyEvent::new(key, KeyModifiers::NONE), &state)
        };

        assert_eq!(press(&mut shell, KeyCode::Char('t')), ShellAction::Continue);
        assert_eq!(press(&mut shell, KeyCode::Enter), ShellAction::Continue);
        assert_eq!(press(&mut shell, KeyCode::Enter), ShellAction::Continue);
        assert_eq!(
            press(&mut shell, KeyCode::Enter),
            ShellAction::SellTrain { train_id }
        );
    }

    #[test]
    fn help_explains_all_keyboard_reachable_core_actions() {
        let state = create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0));
        let mut shell = Shell::new();

        assert_eq!(
            shell.handle_key(
                KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
                &state
            ),
            ShellAction::Continue
        );
        assert!(shell.help_visible());
        let help = super::help_text();
        for instruction in [
            "[M] Map",
            "[T] Fleet",
            "[C] Company",
            "[B] Buy Trains",
            "[D]",
            "[Q]",
        ] {
            assert!(help.contains(instruction));
        }
        shell.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &state);
        assert!(!shell.help_visible());
    }

    #[test]
    fn bankruptcy_blocks_normal_actions_but_allows_exit_and_confirmed_safe_restart() {
        let mut state = create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0));
        state.player_company.funds = Money::ZERO;
        let mut shell = Shell::new();

        assert_eq!(
            shell.handle_key(
                KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE),
                &state
            ),
            ShellAction::Continue
        );
        assert_eq!(shell.active_view(), View::Map);
        assert_eq!(
            shell.handle_key(
                KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
                &state
            ),
            ShellAction::Continue
        );
        assert_eq!(
            shell.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &state),
            ShellAction::RestartAfterBankruptcy
        );
        assert!(super::bankruptcy_text(true).contains("preserving this Player Company save"));
        assert!(super::bankruptcy_text(true).contains("Press Enter to confirm"));

        let mut exiting_shell = Shell::new();
        assert_eq!(
            exiting_shell.handle_key(
                KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
                &state
            ),
            ShellAction::Exit
        );
    }
}
