use anyhow::{Context, Result};
use clap::Parser;
use crossterm::event::{Event, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::stdout;
use std::time::Duration;
use tokio::sync::mpsc;

mod app;
mod cli;
mod input;
mod process;
mod rpc;
mod state;
mod ui;
mod utils;
mod worker;

use app::{Action, App, Modal, WorkerMessage};
use cli::Cli;
use process::ProcessManager;
use state::AppStateStorage;
use utils::clean_path;
use worker::AppWorker;

#[tokio::main]
async fn main() -> Result<()> {
    // Respect RUST_LOG; default to silent
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    let cli = Cli::parse();

    // 1. Ensure download dir and .torrent metadata dir exist
    if !cli.dir.exists() {
        std::fs::create_dir_all(&cli.dir)
            .with_context(|| format!("Failed to create download directory {:?}", cli.dir))?;
    }

    let canonical = std::fs::canonicalize(&cli.dir).unwrap_or_else(|_| cli.dir.clone());
    let base_dir = clean_path(&canonical);

    let meta_dir = AppStateStorage::torrent_meta_dir(&base_dir);
    if !meta_dir.exists() {
        std::fs::create_dir_all(&meta_dir)
            .with_context(|| format!("Failed to create metadata directory {:?}", meta_dir))?;
    }

    // 2. Load or initialize state.json
    let mut state = AppStateStorage::load(&base_dir)
        .with_context(|| format!("Failed to initialize app state in {:?}", meta_dir))?;

    // Startup sync: prune missing payload torrents & detect untracked .torrent files
    let startup_scan = state
        .sync_on_startup(&base_dir)
        .context("Failed to synchronize state and .torrent directory on startup")?;

    // 3. Spawn the managed aria2c process
    println!("Spawning aria2c and connecting at {}...", cli.rpc_url);
    let process_mgr = ProcessManager::init(
        &cli.aria2_bin,
        &cli.rpc_url,
        cli.rpc_secret.clone(),
        &base_dir,
    )
    .await
    .context("Failed to initialize or spawn aria2c process")?;

    // 4. Set up channels between the UI and background worker
    let (action_tx, action_rx) = mpsc::channel::<Action>(100);
    let (worker_tx, worker_rx) = mpsc::channel(100);

    let worker = AppWorker {
        base_dir: base_dir.clone(),
        process_mgr,
        state,
        pending_magnet_tags: std::collections::HashMap::new(),
        action_rx,
        worker_tx,
    };

    tokio::spawn(async move {
        worker.run().await;
    });

    // 5. Install a panic hook that always restores the terminal
    let orig_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen, crossterm::cursor::Show);
        orig_hook(panic_info);
    }));

    // 6. Enter alternate screen / raw mode
    enable_raw_mode().context("Failed to enable terminal raw mode")?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen, crossterm::cursor::Hide)
        .context("Failed to enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("Failed to create Ratatui terminal")?;

    let mut app = App::new(base_dir, action_tx.clone(), worker_rx);

    // Show the new-torrents prompt if untracked files were found on startup
    if !startup_scan.new_untracked.is_empty() {
        app.modal = Modal::NewTorrentsFound {
            torrents: startup_scan.new_untracked,
            selected_index: 0,
        };
    }

    // Trigger an immediate sync
    let _ = action_tx.send(Action::Refresh).await;

    // 7. Main event + render loop
    let res = run_app(&mut terminal, &mut app).await;

    // 8. Restore terminal
    let _ = disable_raw_mode();
    let _ = execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        crossterm::cursor::Show
    );

    // Signal worker to shut down aria2c and wait briefly
    let _ = action_tx.send(Action::Quit).await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    res
}

async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
) -> Result<()> {
    // Wait briefly (up to ~300ms) for the initial SyncUpdate so frame 0 does not flash an empty table
    if app.items.is_empty() {
        let _ = tokio::time::timeout(Duration::from_millis(300), async {
            while let Some(msg) = app.worker_rx.recv().await {
                let is_sync = matches!(msg, WorkerMessage::SyncUpdate { .. });
                app.apply_worker_message(msg);
                if is_sync {
                    break;
                }
            }
        })
        .await;
    }

    while !app.should_quit {
        // Apply any worker messages before rendering
        app.process_worker_messages();

        terminal.draw(|f| ui::render(f, app))?;

        if crossterm::event::poll(Duration::from_millis(40))? {
            if let Event::Key(key) = crossterm::event::read()? {
                // On Windows, ignore key-release events
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                input::handle_key_event(app, key.code, key.modifiers).await;
            }
        }
    }

    Ok(())
}
