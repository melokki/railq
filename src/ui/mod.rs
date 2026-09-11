//! Terminal shell and shared navigation state.
//!
//! This module deliberately owns terminal I/O only. Game actions remain at the
//! application boundary, where callers supply an elapsed-time reconciliation
//! callback before the shell accepts each input event.

use std::{
    error::Error,
    fmt,
    io::{self, Stdout, Write},
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute, queue,
    style::Print,
    terminal::{
        self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
        enable_raw_mode,
    },
};

use crate::{
    APPLICATION_NAME,
    model::{GameState, RailStationId, TrainId, UtcSeconds},
};

pub mod dispatch;
pub mod map;
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
}

/// Presentation-only state shared by the four primary views.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Shell {
    active_view: View,
    dispatch_flow: Option<dispatch::DispatchFlow>,
    notice: Option<String>,
}

impl Shell {
    /// Creates a shell with Map as the primary view.
    pub const fn new() -> Self {
        Self {
            active_view: View::Map,
            dispatch_flow: None,
            notice: None,
        }
    }

    /// Returns the selected primary view.
    pub fn active_view(&self) -> View {
        self.active_view
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

        match key.code {
            KeyCode::Char('m' | 'M') => self.active_view = View::Map,
            KeyCode::Char('t' | 'T') => self.active_view = View::Trains,
            KeyCode::Char('c' | 'C') => self.active_view = View::Company,
            KeyCode::Char('b' | 'B') => self.active_view = View::BuyTrains,
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

/// RAII guard for the terminal modes owned by the shell.
struct TerminalSession {
    stdout: Stdout,
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

        Ok(Self {
            stdout,
            active: true,
        })
    }

    fn draw(&mut self, shell: &Shell, state: &GameState) -> io::Result<()> {
        let (columns, rows) = terminal::size()?;
        queue!(
            self.stdout,
            MoveTo(0, 0),
            Clear(ClearType::All),
            Print(format!(
                "{APPLICATION_NAME} — {}\n\n",
                shell.active_view.label()
            )),
        )?;

        if let Some(hint) = shell.resize_hint(columns, rows) {
            queue!(self.stdout, Print(hint))?;
        } else {
            queue!(
                self.stdout,
                Print(
                    "[M] Map  [T] Trains  [C] Company  [B] Buy Trains  [D] Manual Dispatch  [Q] Quit\n\n"
                ),
            )?;
            match shell.active_view {
                View::Map => {
                    queue!(self.stdout, Print(map::render(state)))?;
                    if let Some(flow) = &shell.dispatch_flow {
                        queue!(self.stdout, Print("\n"), Print(flow.render(state)))?;
                    }
                }
                _ => queue!(
                    self.stdout,
                    Print("This view's details follow in its dedicated task.")
                )?,
            }
            if let Some(notice) = &shell.notice {
                queue!(self.stdout, Print(format!("\n{notice}\n")))?;
            }
        }
        self.stdout.flush()
    }

    fn restore(&mut self) -> io::Result<()> {
        if !self.active {
            return Ok(());
        }
        self.active = false;

        let screen_result = execute!(self.stdout, Show, LeaveAlternateScreen);
        let raw_mode_result = disable_raw_mode();
        screen_result.and(raw_mode_result)
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

    use crate::{
        model::{RailStationId, UtcSeconds},
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
}
