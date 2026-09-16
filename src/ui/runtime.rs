//! Crossterm/Ratatui process loop for the terminal UI.
//!
//! This module owns terminal modes, event polling, and the reconciliation loop.
//! `Shell` remains presentation state and emits application commands without
//! knowing how terminal I/O is configured.

use std::{
    error::Error,
    fmt,
    io::{self, Stdout},
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    time::Duration,
};

use crossterm::{
    cursor::{Hide, Show},
    event::{self, Event},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::{
    app::{AppCommand, AppCommandResult},
    model::GameState,
    sim::time::SettledJourney,
};

use super::{Shell, ShellAction, render_frame};

/// One command accepted by the terminal shell at the application boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TerminalCommand {
    /// Reconcile elapsed demand and due Journey arrivals before presentation or input.
    Reconcile,
    /// Execute one presentation-independent player command.
    Player(AppCommand),
    /// Archive the Bankrupt Player Company save and start a fresh game.
    RestartAfterBankruptcy,
}

/// State returned by the runtime command boundary after a durable application action.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalCommandOutcome {
    state: GameState,
    player_result: Option<AppCommandResult>,
}

impl TerminalCommandOutcome {
    /// Returns a state produced by elapsed-time reconciliation.
    pub fn reconciled(state: GameState) -> Self {
        Self {
            state,
            player_result: None,
        }
    }

    /// Returns a state and typed result produced by one player command.
    pub fn player(state: GameState, player_result: AppCommandResult) -> Self {
        Self {
            state,
            player_result: Some(player_result),
        }
    }

    /// Returns a fresh state produced by a confirmed Bankruptcy restart.
    pub fn restarted(state: GameState) -> Self {
        Self {
            state,
            player_result: None,
        }
    }

    fn into_parts(self) -> (GameState, Option<AppCommandResult>) {
        (self.state, self.player_result)
    }
}


/// How frequently the terminal checks for elapsed arrivals while no key is pressed.
const ARRIVAL_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// An error from the terminal runtime or its supplied reconciliation boundary.
#[derive(Debug)]
pub enum RunError<E> {
    /// Terminal input, output, or mode configuration failed.
    Terminal(io::Error),
    /// Reconciliation failed before the shell could accept input.
    Reconcile(E),
    /// The supplied runtime boundary returned an outcome inconsistent with the command.
    Protocol(&'static str),
}

impl<E: fmt::Display> fmt::Display for RunError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Terminal(error) => write!(formatter, "terminal error: {error}"),
            Self::Reconcile(error) => {
                write!(formatter, "could not reconcile elapsed time: {error}")
            }
            Self::Protocol(message) => {
                write!(formatter, "terminal command protocol error: {message}")
            }
        }
    }
}

impl<E: Error + 'static> Error for RunError<E> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Terminal(error) => Some(error),
            Self::Reconcile(error) => Some(error),
            Self::Protocol(_) => None,
        }
    }
}

/// Runs the terminal shell until the player exits.
///
/// `command` is invoked while idle and immediately before every input event,
/// so due Journeys settle before the next action is accepted. The terminal is
/// restored on ordinary errors and while unwinding a panic.
pub fn run_terminal<E>(
    initial_state: GameState,
    command: impl FnMut(TerminalCommand) -> Result<TerminalCommandOutcome, E>,
) -> Result<(), RunError<E>>
where
    E: fmt::Display,
{
    run_terminal_with_arrivals(initial_state, Vec::new(), command)
}

/// Runs the terminal shell with arrivals committed while loading a save.
pub fn run_terminal_with_arrivals<E>(
    initial_state: GameState,
    startup_arrivals: Vec<SettledJourney>,
    mut command: impl FnMut(TerminalCommand) -> Result<TerminalCommandOutcome, E>,
) -> Result<(), RunError<E>>
where
    E: fmt::Display,
{
    let mut terminal = TerminalSession::enter().map_err(RunError::Terminal)?;
    let result = catch_unwind(AssertUnwindSafe(|| {
        run_event_loop(&mut terminal, initial_state, startup_arrivals, &mut command)
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
    startup_arrivals: Vec<SettledJourney>,
    command: &mut impl FnMut(TerminalCommand) -> Result<TerminalCommandOutcome, E>,
) -> Result<(), RunError<E>>
where
    E: fmt::Display,
{
    let mut shell = Shell::new();
    shell.publish_settled_arrivals(&state, &startup_arrivals);

    loop {
        terminal
            .draw(|frame| render_frame(frame, &mut shell, &state))
            .map_err(RunError::Terminal)?;
        let before_reconciliation = state.clone();
        let (reconciled_state, player_result) = command(TerminalCommand::Reconcile)
            .map_err(RunError::Reconcile)?
            .into_parts();
        if player_result.is_some() {
            return Err(RunError::Protocol(
                "reconciliation returned a player command result",
            ));
        }
        shell.publish_committed_arrivals(&before_reconciliation, &reconciled_state);
        state = reconciled_state;

        if !event::poll(ARRIVAL_POLL_INTERVAL).map_err(RunError::Terminal)? {
            continue;
        }

        match event::read().map_err(RunError::Terminal)? {
            Event::Key(key) => {
                let before_reconciliation = state.clone();
                let (reconciled_state, player_result) = command(TerminalCommand::Reconcile)
                    .map_err(RunError::Reconcile)?
                    .into_parts();
                if player_result.is_some() {
                    return Err(RunError::Protocol(
                        "reconciliation returned a player command result",
                    ));
                }
                shell.publish_committed_arrivals(&before_reconciliation, &reconciled_state);
                state = reconciled_state;
                match shell.handle_key(key, &state) {
                    ShellAction::Exit => return Ok(()),
                    ShellAction::Player(player_command) => {
                        let before_command = state.clone();
                        match command(TerminalCommand::Player(player_command.clone())) {
                            Ok(outcome) => {
                                let (next_state, player_result) = outcome.into_parts();
                                let Some(player_result) = player_result else {
                                    return Err(RunError::Protocol(
                                        "player command returned no application result",
                                    ));
                                };
                                state = next_state;
                                shell.confirm_player_command_saved(&player_result, &state);
                                shell.publish_committed_arrivals(&before_command, &state);
                            }
                            Err(error) => {
                                shell.reject_player_command(&player_command, error.to_string());
                            }
                        }
                    }
                    ShellAction::RestartAfterBankruptcy => {
                        match command(TerminalCommand::RestartAfterBankruptcy) {
                            Ok(outcome) => {
                                let (next_state, player_result) = outcome.into_parts();
                                if player_result.is_some() {
                                    return Err(RunError::Protocol(
                                        "bankruptcy restart returned a player command result",
                                    ));
                                }
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

/// RAII guard for the terminal modes owned by the runtime.
pub(super) struct TerminalSession {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    active: bool,
}

impl TerminalSession {
    pub(super) fn enter() -> io::Result<Self> {
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

    pub(super) fn draw(&mut self, render: impl FnOnce(&mut ratatui::Frame)) -> io::Result<()> {
        self.terminal.draw(render).map(|_| ())
    }

    pub(super) fn restore(&mut self) -> io::Result<()> {
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
