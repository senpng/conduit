//! Interactive terminal console for Conduit (`conduitctl tui`).

mod action;
mod app;
mod clipboard;
mod draw;
mod event;
mod forms;
mod input;
mod msg;
mod net;
mod theme;
mod widgets;

use std::io::{self, stdout};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use crossterm::event::{Event, EventStream, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures_util::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

use app::App;
use event::map_key;

/// Run the fullscreen interactive console until the user quits.
pub async fn run(console_addr: &str) -> Result<()> {
    if !io::IsTerminal::is_terminal(&io::stdout()) {
        bail!("TUI requires an interactive terminal (stdout is not a TTY)");
    }

    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut app = App::new(console_addr, tx);
    app.request_refresh();

    let mut terminal = setup_terminal().context("setup terminal")?;
    let result = run_loop(&mut terminal, &mut app, &mut rx).await;
    restore_terminal(&mut terminal).context("restore terminal")?;
    result
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(out);
    let mut terminal = Terminal::new(backend)?;
    // Forms draw their own caret; the hardware cursor just floats and confuses.
    terminal.hide_cursor()?;
    // Best-effort restore on panic
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        let _ = execute!(io::stdout(), crossterm::cursor::Show);
        original_hook(info);
    }));
    Ok(terminal)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    rx: &mut mpsc::UnboundedReceiver<msg::Msg>,
) -> Result<()> {
    // Drives spinner animation + OAuth polling; input no longer rides on it.
    let mut tick = tokio::time::interval(Duration::from_millis(200));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // Async key/resize stream so input is handled the instant it arrives
    // (previously keys were polled only on the 200ms tick → sluggish typing).
    let mut events = EventStream::new();

    loop {
        terminal.draw(|f| draw::draw(f, app))?;

        if app.should_quit {
            break;
        }

        tokio::select! {
            biased;
            msg = rx.recv() => {
                if let Some(msg) = msg {
                    app.apply_msg(msg);
                }
            }
            maybe_event = events.next() => {
                match maybe_event {
                    Some(Ok(Event::Key(key))) => {
                        if key.kind == KeyEventKind::Press {
                            if let Some(action) = map_key(&app.mode, app.tab, key) {
                                app.handle_action(action);
                            }
                        }
                    }
                    // Resize / focus / paste / mouse: redraw happens next loop.
                    Some(Ok(_)) => {}
                    // Read error or stream end: stop the console.
                    Some(Err(_)) | None => {
                        app.should_quit = true;
                    }
                }
            }
            _ = tick.tick() => {
                app.handle_action(action::Action::Tick);
            }
        }
    }
    Ok(())
}
