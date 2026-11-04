use crossterm::event::{KeyCode, KeyModifiers};

use crate::app::{Action, App, Modal};
use crate::utils::{clean_path_str, strip_quotes};

/// Dispatches a key-press to the appropriate modal or main-view handler.
pub async fn handle_key_event(app: &mut App, code: KeyCode, _modifiers: KeyModifiers) {
    match &mut app.modal {
        // -----------------------------------------------------------------
        // No modal open – main torrent list controls
        // -----------------------------------------------------------------
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
                // Batch start all visible torrents in the current tag view
                let hashes: Vec<String> = app
                    .filtered_items()
                    .into_iter()
                    .map(|it| it.info_hash.clone())
                    .collect();
                let _ = app.action_tx.send(Action::BatchStart(hashes)).await;
            }
            KeyCode::Char('P') => {
                // Batch pause all visible torrents in the current tag view
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
            KeyCode::Char('D') => {
                if let Some(item) = app.selected_torrent() {
                    app.modal = Modal::DeleteConfirm {
                        info_hash: item.info_hash.clone(),
                        name: item.name.clone(),
                        delete_files_from_disk: true,
                    };
                }
            }
            _ => {}
        },

        // -----------------------------------------------------------------
        // Add torrent modal
        // -----------------------------------------------------------------
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
                let clean = if target.starts_with("magnet:") {
                    strip_quotes(&target)
                } else {
                    clean_path_str(&strip_quotes(&target))
                };
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

        // -----------------------------------------------------------------
        // File selection modal
        // -----------------------------------------------------------------
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
                let indices_str = files
                    .iter()
                    .filter(|f| f.selected)
                    .map(|f| f.index.clone())
                    .collect::<Vec<_>>()
                    .join(",");
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

        // -----------------------------------------------------------------
        // Edit tags modal
        // -----------------------------------------------------------------
        Modal::EditTags { info_hash, tags, .. } => match code {
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

        // -----------------------------------------------------------------
        // Per-torrent options modal
        // -----------------------------------------------------------------
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
                0 => { max_download_limit.pop(); }
                1 => { max_upload_limit.pop(); }
                2 => { seed_ratio.pop(); }
                3 => { seed_time.pop(); }
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

        // -----------------------------------------------------------------
        // Global options modal
        // -----------------------------------------------------------------
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
                0 => { max_overall_download_limit.pop(); }
                1 => { max_overall_upload_limit.pop(); }
                2 => { default_seed_ratio.pop(); }
                3 => { default_seed_time.pop(); }
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

        // -----------------------------------------------------------------
        // Delete confirmation modal
        // -----------------------------------------------------------------
        Modal::DeleteConfirm {
            info_hash,
            delete_files_from_disk,
            ..
        } => match code {
            KeyCode::Esc | KeyCode::Char('n') => {
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
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
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

        // -----------------------------------------------------------------
        // New torrents found modal
        // -----------------------------------------------------------------
        Modal::NewTorrentsFound {
            torrents,
            selected_index,
        } => match code {
            KeyCode::Esc => {
                app.modal = Modal::None;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if !torrents.is_empty() && *selected_index + 1 < torrents.len() {
                    *selected_index += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if *selected_index > 0 {
                    *selected_index -= 1;
                }
            }
            KeyCode::Char(' ') => {
                if let Some(t) = torrents.get_mut(*selected_index) {
                    t.selected = !t.selected;
                }
            }
            KeyCode::Char('a') => {
                for t in torrents.iter_mut() {
                    t.selected = true;
                }
            }
            KeyCode::Char('n') => {
                for t in torrents.iter_mut() {
                    t.selected = false;
                }
            }
            KeyCode::Enter | KeyCode::Char('y') => {
                let selected: Vec<_> = torrents.iter().filter(|t| t.selected).cloned().collect();
                let _ = app
                    .action_tx
                    .send(Action::ImportUntrackedTorrents {
                        torrents: selected,
                        start_download: true,
                    })
                    .await;
                app.modal = Modal::None;
            }
            KeyCode::Char('s') => {
                let selected: Vec<_> = torrents.iter().filter(|t| t.selected).cloned().collect();
                let _ = app
                    .action_tx
                    .send(Action::ImportUntrackedTorrents {
                        torrents: selected,
                        start_download: false,
                    })
                    .await;
                app.modal = Modal::None;
            }
            _ => {}
        },

        // -----------------------------------------------------------------
        // Message / info modal
        // -----------------------------------------------------------------
        Modal::Message { .. } => match code {
            KeyCode::Enter | KeyCode::Esc => {
                app.modal = Modal::None;
            }
            _ => {}
        },
    }
}
