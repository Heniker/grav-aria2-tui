use anyhow::{Context, Result};
use clap::Parser;
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
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
mod process;
mod rpc;
mod state;
mod ui;

use app::{Action, App, AppWorker, Modal, strip_quotes};
use cli::Cli;
use process::ProcessManager;
use state::AppStateStorage;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // 1. Ensure download dir and .torrent metadata dir exist
    let base_dir = std::fs::canonicalize(&cli.dir).unwrap_or_else(|_| cli.dir.clone());
    if !base_dir.exists() {
        std::fs::create_dir_all(&base_dir)
            .with_context(|| format!("Failed to create download directory {:?}", base_dir))?;
    }

    let meta_dir = AppStateStorage::torrent_meta_dir(&base_dir);
    if !meta_dir.exists() {
        std::fs::create_dir_all(&meta_dir)
            .with_context(|| format!("Failed to create metadata directory {:?}", meta_dir))?;
    }

    // 2. Load or initialize state.json (and scan any existing .torrent files)
    let state = AppStateStorage::load(&base_dir)
        .with_context(|| format!("Failed to initialize app state in {:?}", meta_dir))?;

    // 3. Connect to or spawn aria2c
    println!("Connecting to aria2c RPC at {}...", cli.rpc_url);
    let process_mgr = ProcessManager::init(
        &cli.aria2_bin,
        &cli.rpc_url,
        cli.rpc_secret.clone(),
        &base_dir,
    )
    .await
    .context("Failed to initialize or spawn aria2c process")?;

    let spawned_by_us = process_mgr.spawned_by_us;

    // 4. Setup channels for background worker and UI
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

    // Spawn background worker task
    tokio::spawn(async move {
        worker.run().await;
    });

    // 5. Setup panic hook to always restore terminal
    let orig_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen, crossterm::cursor::Show);
        orig_hook(panic_info);
    }));

    // 6. Enter raw mode and alternate screen
    enable_raw_mode().context("Failed to enable terminal raw mode")?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen, crossterm::cursor::Hide)
        .context("Failed to enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("Failed to create Ratatui terminal")?;

    let mut app = App::new(base_dir, spawned_by_us, action_tx.clone(), worker_rx);

    // Initial sync trigger
    let _ = action_tx.send(Action::Refresh).await;

    // 7. Event & Render loop
    let res = run_app(&mut terminal, &mut app).await;

    // 8. Restore terminal cleanly
    let _ = disable_raw_mode();
    let _ = execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        crossterm::cursor::Show
    );

    // Ensure worker cleans up and kills spawned aria2c if needed
    let _ = action_tx.send(Action::Quit).await;
    // Allow worker a brief moment to shut down aria2c
    tokio::time::sleep(Duration::from_millis(300)).await;

    res
}

async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
) -> Result<()> {
    while !app.should_quit {
        // Drain any messages from background worker
        app.process_worker_messages();

        // Draw TUI
        terminal.draw(|f| ui::render(f, app))?;

        // Non-blocking poll for crossterm events
        if crossterm::event::poll(Duration::from_millis(40))? {
            if let Event::Key(key) = crossterm::event::read()? {
                // Windows Key Event Guard: ignore key releases
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                handle_key_event(app, key.code, key.modifiers).await;
            }
        }
    }

    Ok(())
}

async fn handle_key_event(app: &mut App, code: KeyCode, _modifiers: KeyModifiers) {
    match &mut app.modal {
        Modal::None => match code {
            KeyCode::Char('q') => {
                app.should_quit = true;
                let _ = app.action_tx.send(Action::Quit).await;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                app.next_torrent();
            }
            KeyCode::Char('k') | KeyCode::Up => {
                app.prev_torrent();
            }
            KeyCode::Tab | KeyCode::Char(']') => {
                app.next_tag_filter();
            }
            KeyCode::BackTab | KeyCode::Char('[') => {
                app.prev_tag_filter();
            }
            KeyCode::Char('s') | KeyCode::Char(' ') => {
                if let Some(item) = app.selected_torrent() {
                    let _ = app
                        .action_tx
                        .send(Action::ToggleStartPause(item.info_hash.clone()))
                        .await;
                }
            }
            KeyCode::Char('S') => {
                // Batch start all visible torrents in current tag view
                let hashes: Vec<String> = app
                    .filtered_items()
                    .into_iter()
                    .map(|it| it.info_hash.clone())
                    .collect();
                let _ = app.action_tx.send(Action::BatchStart(hashes)).await;
            }
            KeyCode::Char('P') => {
                // Batch pause all visible torrents in current tag view
                let hashes: Vec<String> = app
                    .filtered_items()
                    .into_iter()
                    .map(|it| it.info_hash.clone())
                    .collect();
                let _ = app.action_tx.send(Action::BatchPause(hashes)).await;
            }
            KeyCode::Char('a') => {
                app.modal = Modal::AddTorrent {
                    path_or_magnet: String::new(),
                    tags: String::new(),
                    active_field: 0,
                    cursor_pos: 0,
                };
            }
            KeyCode::Char('f') => {
                if let Some(item) = app.selected_torrent() {
                    let _ = app
                        .action_tx
                        .send(Action::FetchFilesForSelection(item.info_hash.clone()))
                        .await;
                }
            }
            KeyCode::Char('t') => {
                if let Some(item) = app.selected_torrent() {
                    app.modal = Modal::EditTags {
                        info_hash: item.info_hash.clone(),
                        tags: item.tags.join(", "),
                        cursor_pos: 0,
                    };
                }
            }
            KeyCode::Char('o') => {
                if let Some(item) = app.selected_torrent() {
                    app.modal = Modal::TorrentOptions {
                        info_hash: item.info_hash.clone(),
                        max_download_limit: item.max_download_limit.clone(),
                        max_upload_limit: item.max_upload_limit.clone(),
                        seed_ratio: item.seed_ratio.clone(),
                        seed_time: item.seed_time.clone(),
                        active_field: 0,
                        cursor_pos: 0,
                    };
                }
            }
            KeyCode::Char('g') => {
                app.modal = Modal::GlobalOptions {
                    max_overall_download_limit: app.global_dl_limit.clone(),
                    max_overall_upload_limit: app.global_ul_limit.clone(),
                    default_seed_ratio: "1.0".to_string(),
                    default_seed_time: "0".to_string(),
                    active_field: 0,
                    cursor_pos: 0,
                };
            }
            KeyCode::Char('v') => {
                if let Some(item) = app.selected_torrent() {
                    let _ = app
                        .action_tx
                        .send(Action::VerifyRecheck(item.info_hash.clone()))
                        .await;
                }
            }
            KeyCode::Char('d') | KeyCode::Delete => {
                if let Some(item) = app.selected_torrent() {
                    app.modal = Modal::DeleteConfirm {
                        info_hash: item.info_hash.clone(),
                        name: item.name.clone(),
                        delete_files_from_disk: false,
                    };
                }
            }
            _ => {}
        },
        Modal::AddTorrent {
            path_or_magnet,
            tags,
            active_field,
            ..
        } => match code {
            KeyCode::Esc => {
                app.modal = Modal::None;
            }
            KeyCode::Tab | KeyCode::Down => {
                *active_field = (*active_field + 1) % 2;
            }
            KeyCode::BackTab | KeyCode::Up => {
                *active_field = if *active_field == 0 { 1 } else { 0 };
            }
            KeyCode::Enter => {
                let target = path_or_magnet.trim().to_string();
                let clean = strip_quotes(&target);
                let tag_list: Vec<String> = tags
                    .split(',')
                    .map(|t| t.trim().to_lowercase())
                    .filter(|t| !t.is_empty())
                    .collect();

                if clean.starts_with("magnet:") {
                    let _ = app
                        .action_tx
                        .send(Action::AddTorrentMagnet {
                            magnet: clean,
                            tags: tag_list,
                        })
                        .await;
                } else if !clean.is_empty() {
                    let _ = app
                        .action_tx
                        .send(Action::AddTorrentPath {
                            path: clean,
                            tags: tag_list,
                        })
                        .await;
                }
                app.modal = Modal::None;
            }
            KeyCode::Backspace => {
                if *active_field == 0 {
                    path_or_magnet.pop();
                } else {
                    tags.pop();
                }
            }
            KeyCode::Char(c) => {
                if *active_field == 0 {
                    path_or_magnet.push(c);
                } else {
                    tags.push(c);
                }
            }
            _ => {}
        },
        Modal::FileSelection {
            info_hash,
            files,
            selected_index,
            ..
        } => match code {
            KeyCode::Esc => {
                app.modal = Modal::None;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if !files.is_empty() && *selected_index + 1 < files.len() {
                    *selected_index += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if *selected_index > 0 {
                    *selected_index -= 1;
                }
            }
            KeyCode::Char(' ') => {
                if let Some(item) = files.get_mut(*selected_index) {
                    item.selected = !item.selected;
                }
            }
            KeyCode::Char('a') => {
                for f in files.iter_mut() {
                    f.selected = true;
                }
            }
            KeyCode::Char('n') => {
                for f in files.iter_mut() {
                    f.selected = false;
                }
            }
            KeyCode::Enter => {
                let selected_indices: Vec<String> = files
                    .iter()
                    .filter(|f| f.selected)
                    .map(|f| f.index.clone())
                    .collect();
                let indices_str = selected_indices.join(",");
                let _ = app
                    .action_tx
                    .send(Action::ApplyFileSelection {
                        info_hash: info_hash.clone(),
                        selected_indices: indices_str,
                    })
                    .await;
                app.modal = Modal::None;
            }
            _ => {}
        },
        Modal::EditTags {
            info_hash, tags, ..
        } => match code {
            KeyCode::Esc => {
                app.modal = Modal::None;
            }
            KeyCode::Enter => {
                let tag_list: Vec<String> = tags
                    .split(',')
                    .map(|t| t.trim().to_lowercase())
                    .filter(|t| !t.is_empty())
                    .collect();
                let _ = app
                    .action_tx
                    .send(Action::UpdateTags {
                        info_hash: info_hash.clone(),
                        tags: tag_list,
                    })
                    .await;
                app.modal = Modal::None;
            }
            KeyCode::Backspace => {
                tags.pop();
            }
            KeyCode::Char(c) => {
                tags.push(c);
            }
            _ => {}
        },
        Modal::TorrentOptions {
            info_hash,
            max_download_limit,
            max_upload_limit,
            seed_ratio,
            seed_time,
            active_field,
            ..
        } => match code {
            KeyCode::Esc => {
                app.modal = Modal::None;
            }
            KeyCode::Tab | KeyCode::Down => {
                *active_field = (*active_field + 1) % 4;
            }
            KeyCode::BackTab | KeyCode::Up => {
                *active_field = (*active_field + 3) % 4;
            }
            KeyCode::Enter => {
                let _ = app
                    .action_tx
                    .send(Action::UpdateTorrentOptions {
                        info_hash: info_hash.clone(),
                        max_dl: max_download_limit.trim().to_string(),
                        max_ul: max_upload_limit.trim().to_string(),
                        ratio: seed_ratio.trim().to_string(),
                        time: seed_time.trim().to_string(),
                    })
                    .await;
                app.modal = Modal::None;
            }
            KeyCode::Backspace => match *active_field {
                0 => {
                    max_download_limit.pop();
                }
                1 => {
                    max_upload_limit.pop();
                }
                2 => {
                    seed_ratio.pop();
                }
                3 => {
                    seed_time.pop();
                }
                _ => {}
            },
            KeyCode::Char(c) => match *active_field {
                0 => max_download_limit.push(c),
                1 => max_upload_limit.push(c),
                2 => seed_ratio.push(c),
                3 => seed_time.push(c),
                _ => {}
            },
            _ => {}
        },
        Modal::GlobalOptions {
            max_overall_download_limit,
            max_overall_upload_limit,
            default_seed_ratio,
            default_seed_time,
            active_field,
            ..
        } => match code {
            KeyCode::Esc => {
                app.modal = Modal::None;
            }
            KeyCode::Tab | KeyCode::Down => {
                *active_field = (*active_field + 1) % 4;
            }
            KeyCode::BackTab | KeyCode::Up => {
                *active_field = (*active_field + 3) % 4;
            }
            KeyCode::Enter => {
                let _ = app
                    .action_tx
                    .send(Action::UpdateGlobalOptions {
                        max_dl: max_overall_download_limit.trim().to_string(),
                        max_ul: max_overall_upload_limit.trim().to_string(),
                        ratio: default_seed_ratio.trim().to_string(),
                        time: default_seed_time.trim().to_string(),
                    })
                    .await;
                app.modal = Modal::None;
            }
            KeyCode::Backspace => match *active_field {
                0 => {
                    max_overall_download_limit.pop();
                }
                1 => {
                    max_overall_upload_limit.pop();
                }
                2 => {
                    default_seed_ratio.pop();
                }
                3 => {
                    default_seed_time.pop();
                }
                _ => {}
            },
            KeyCode::Char(c) => match *active_field {
                0 => max_overall_download_limit.push(c),
                1 => max_overall_upload_limit.push(c),
                2 => default_seed_ratio.push(c),
                3 => default_seed_time.push(c),
                _ => {}
            },
            _ => {}
        },
        Modal::DeleteConfirm {
            info_hash,
            delete_files_from_disk,
            ..
        } => match code {
            KeyCode::Esc => {
                app.modal = Modal::None;
            }
            KeyCode::Char('1') => {
                *delete_files_from_disk = false;
            }
            KeyCode::Char('2') => {
                *delete_files_from_disk = true;
            }
            KeyCode::Up | KeyCode::Down => {
                *delete_files_from_disk = !*delete_files_from_disk;
            }
            KeyCode::Enter => {
                let _ = app
                    .action_tx
                    .send(Action::DeleteTorrent {
                        info_hash: info_hash.clone(),
                        delete_files: *delete_files_from_disk,
                    })
                    .await;
                app.modal = Modal::None;
            }
            _ => {}
        },
        Modal::Message { .. } => match code {
            KeyCode::Enter | KeyCode::Esc => {
                app.modal = Modal::None;
            }
            _ => {}
        },
    }
}
