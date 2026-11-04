use base64::Engine;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::mpsc::{Receiver, Sender};

use crate::app::{Action, FileSelectionItem, TorrentItem, TorrentStatus, WorkerMessage};
use crate::process::ProcessManager;
use crate::rpc::DownloadStatus;
use crate::state::{
    AppStateStorage, TorrentMetadata, archive_torrent_file, detect_and_move_saved_metadata,
    parse_torrent_info,
};
use crate::utils::{clean_path, clean_path_str, extract_hash_from_magnet, strip_quotes};

// ---------------------------------------------------------------------------
// AppWorker – runs in a background tokio task
// ---------------------------------------------------------------------------

pub struct AppWorker {
    pub base_dir: PathBuf,
    pub process_mgr: ProcessManager,
    pub state: AppStateStorage,
    /// Pending tags keyed by GID or info-hash, waiting for metadata resolution.
    pub pending_magnet_tags: HashMap<String, Vec<String>>,
    pub action_rx: Receiver<Action>,
    pub worker_tx: Sender<WorkerMessage>,
}

impl AppWorker {
    pub async fn run(mut self) {
        let mut interval = tokio::time::interval(Duration::from_millis(1000));

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    self.sync_aria2_and_state(false).await;
                }
                action = self.action_rx.recv() => {
                    match action {
                        Some(Action::Quit) | None => {
                            let _ = self.state.save(&self.base_dir);
                            self.process_mgr.shutdown().await;
                            break;
                        }
                        Some(act) => {
                            self.handle_action(act).await;
                        }
                    }
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Action dispatch
    // -----------------------------------------------------------------------

    async fn handle_action(&mut self, action: Action) {
        match action {
            Action::Refresh => {
                self.sync_aria2_and_state(true).await;
            }
            Action::ToggleStartPause(info_hash) => {
                self.toggle_start_pause(&info_hash).await;
                self.sync_aria2_and_state(true).await;
            }
            Action::BatchStart(hashes) => {
                for h in hashes {
                    self.start_or_resume(&h, false).await;
                }
                self.sync_aria2_and_state(true).await;
            }
            Action::BatchPause(hashes) => {
                for h in hashes {
                    self.pause_torrent(&h).await;
                }
                self.sync_aria2_and_state(true).await;
            }
            Action::VerifyRecheck(info_hash) => {
                self.verify_recheck(&info_hash).await;
                self.sync_aria2_and_state(true).await;
            }
            Action::AddTorrentPath { path, tags } => {
                self.add_torrent_path(&path, tags).await;
                self.sync_aria2_and_state(true).await;
            }
            Action::AddTorrentMagnet { magnet, tags } => {
                self.add_torrent_magnet(&magnet, tags).await;
                self.sync_aria2_and_state(true).await;
            }
            Action::FetchFilesForSelection(info_hash) => {
                self.fetch_files_for_selection(&info_hash).await;
            }
            Action::ApplyFileSelection { info_hash, selected_indices } => {
                self.apply_file_selection(&info_hash, &selected_indices).await;
                self.sync_aria2_and_state(true).await;
            }
            Action::UpdateTags { info_hash, tags } => {
                if let Some(entry) = self.state.torrents.get_mut(&info_hash.to_lowercase()) {
                    entry.tags = tags;
                    let _ = self.state.save(&self.base_dir);
                }
                self.sync_aria2_and_state(true).await;
            }
            Action::UpdateTorrentOptions { info_hash, max_dl, max_ul, ratio, time } => {
                self.update_torrent_options(&info_hash, max_dl, max_ul, ratio, time).await;
                self.sync_aria2_and_state(true).await;
            }
            Action::UpdateGlobalOptions { max_dl, max_ul, ratio, time } => {
                self.update_global_options(max_dl, max_ul, ratio, time).await;
                self.sync_aria2_and_state(true).await;
            }
            Action::DeleteTorrent { info_hash, delete_files } => {
                self.delete_torrent(&info_hash, delete_files).await;
                self.sync_aria2_and_state(true).await;
            }
            Action::ImportUntrackedTorrents { torrents, start_download } => {
                // Clone global limits to avoid simultaneous borrow of self.state
                let global = self.state.global_limits.clone();
                for candidate in torrents {
                    let key = candidate.info_hash.to_lowercase();
                    self.state.torrents.entry(key.clone()).or_insert_with(|| {
                        TorrentMetadata::new(key.clone(), candidate.name.clone(), Vec::new(), &global)
                    });
                    if start_download {
                        self.start_or_resume(&key, false).await;
                    }
                }
                let _ = self.state.save(&self.base_dir);
                self.sync_aria2_and_state(true).await;
            }
            Action::Quit => {}
        }
    }

    // -----------------------------------------------------------------------
    // Core sync: pull all downloads from aria2, merge with state, push update
    // -----------------------------------------------------------------------

    async fn sync_aria2_and_state(&mut self, _force: bool) {
        let active = self.process_mgr.client.tell_active().await.unwrap_or_default();
        let waiting = self
            .process_mgr
            .client
            .tell_waiting(0, 1000)
            .await
            .unwrap_or_default();
        let stopped = self
            .process_mgr
            .client
            .tell_stopped(0, 1000)
            .await
            .unwrap_or_default();
        let global_stat = self
            .process_mgr
            .client
            .get_global_stat()
            .await
            .unwrap_or_default();

        let global_opts = self
            .process_mgr
            .client
            .get_global_option()
            .await
            .unwrap_or_default();
        let global_dl_limit = global_opts
            .get("max-overall-download-limit")
            .and_then(|v| v.as_str())
            .unwrap_or("0")
            .to_string();
        let global_ul_limit = global_opts
            .get("max-overall-upload-limit")
            .and_then(|v| v.as_str())
            .unwrap_or("0")
            .to_string();

        let mut live_by_hash: HashMap<String, DownloadStatus> = HashMap::new();
        let mut magnet_fetching: Vec<DownloadStatus> = Vec::new();

        // Priority order: stopped (lowest) → waiting → active (highest).
        // Evaluating active last ensures live speeds overwrite any stale stopped entries.
        let all_downloads = stopped
            .into_iter()
            .chain(waiting.into_iter())
            .chain(active.into_iter());

        for d in all_downloads {
            let is_metadata = d.info_hash.is_none()
                || d.files
                    .as_ref()
                    .map_or(false, |fl| fl.iter().any(|f| f.path.starts_with("[METADATA]")))
                || d.followed_by.as_ref().map_or(false, |f| !f.is_empty());

            if is_metadata {
                magnet_fetching.push(d);
            } else if let Some(ref hash) = d.info_hash {
                let h = hash.to_lowercase();
                // Move any aria2-saved <hash>.torrent into our .torrent/ folder
                if let Some(dest) = detect_and_move_saved_metadata(&self.base_dir, &h) {
                    if let Ok(bytes) = std::fs::read(&dest) {
                        if let Some((_, name)) = parse_torrent_info(&bytes) {
                            if let Some(entry) = self.state.torrents.get_mut(&h) {
                                if entry.name.is_empty()
                                    || entry.name == h
                                    || entry.name.starts_with("[Magnet]")
                                {
                                    entry.name = name;
                                }
                            }
                        }
                    }
                }
                live_by_hash.insert(h, d);
            }
        }

        // Handle followedBy metadata downloads (magnet → real torrent transition)
        let global = self.state.global_limits.clone();
        for m in &magnet_fetching {
            if let Some(ref followed) = m.followed_by {
                for next_gid in followed {
                    if let Ok(next_stat) = self.process_mgr.client.tell_status(next_gid).await {
                        if let Some(ref hash) = next_stat.info_hash {
                            let h = hash.to_lowercase();
                            let _ = detect_and_move_saved_metadata(&self.base_dir, &h);

                            // Transfer pending tags from the metadata GID or hash
                            let tags = self
                                .pending_magnet_tags
                                .remove(&m.gid)
                                .or_else(|| self.pending_magnet_tags.remove(&h))
                                .unwrap_or_default();

                            let real_name = next_stat
                                .bittorrent
                                .as_ref()
                                .and_then(|bt| bt.info.as_ref())
                                .and_then(|i| i.name.clone());

                            let entry =
                                self.state.torrents.entry(h.clone()).or_insert_with(|| {
                                    let mut meta = TorrentMetadata::new(
                                        h.clone(),
                                        real_name.clone().unwrap_or_else(|| h.clone()),
                                        tags,
                                        &global,
                                    );
                                    meta.awaiting_file_selection = true;
                                    meta
                                });

                            if let Some(rn) = real_name {
                                if entry.name.is_empty()
                                    || entry.name == h
                                    || entry.name.starts_with("[Magnet]")
                                {
                                    entry.name = rn;
                                }
                            }
                            entry.awaiting_file_selection = true;
                            let _ = self.state.save(&self.base_dir);

                            // Prompt user to select files
                            if let Ok(files) = self.process_mgr.client.get_files(next_gid).await {
                                let selection_items: Vec<FileSelectionItem> = files
                                    .into_iter()
                                    .map(|f| FileSelectionItem {
                                        index: f.index,
                                        path: f.path,
                                        length: f.length.parse().unwrap_or(0),
                                        selected: f.selected == "true",
                                    })
                                    .collect();
                                let _ = self
                                    .worker_tx
                                    .send(WorkerMessage::AutoOpenFileSelection {
                                        info_hash: h.clone(),
                                        gid: Some(next_gid.clone()),
                                        files: selection_items,
                                    })
                                    .await;
                            }
                            // Clean up the now-resolved metadata download result
                            let _ = self
                                .process_mgr
                                .client
                                .remove_download_result(&m.gid)
                                .await;
                        }
                    }
                }
            }
        }

        // Build the unified torrent list
        let mut items = Vec::new();

        // 1. Torrents already tracked in state.json
        for (h, meta) in &mut self.state.torrents {
            if let Some(live) = live_by_hash.remove(h) {
                let total: u64 = live.total_length.parse().unwrap_or(0);
                let completed: u64 = live.completed_length.parse().unwrap_or(0);
                let uploaded: u64 = live.upload_length.parse().unwrap_or(0);
                let dl_speed: u64 = live.download_speed.parse().unwrap_or(0);
                let ul_speed: u64 = live.upload_speed.parse().unwrap_or(0);

                let is_seeder = live.seeder.as_deref() == Some("true");
                let is_complete = meta.is_complete
                    || (total > 0 && completed >= total)
                    || is_seeder
                    || live.status == "complete";
                meta.is_complete = is_complete;

                let status = TorrentStatus::from_aria2(
                    &live.status,
                    is_complete,
                    is_seeder,
                    meta.awaiting_file_selection,
                );

                // Update cached name from live bittorrent info if available
                let name = live
                    .bittorrent
                    .as_ref()
                    .and_then(|bt| bt.info.as_ref())
                    .and_then(|i| i.name.as_deref())
                    .filter(|n| !n.is_empty())
                    .map(|n| {
                        meta.name = n.to_string();
                        n.to_string()
                    })
                    .unwrap_or_else(|| meta.name.clone());

                items.push(TorrentItem {
                    info_hash: h.clone(),
                    gid: Some(live.gid),
                    name,
                    tags: meta.tags.clone(),
                    size_bytes: total,
                    completed_bytes: completed,
                    uploaded_bytes: uploaded,
                    download_speed: dl_speed,
                    upload_speed: ul_speed,
                    status,
                    selected_files: meta.selected_files.clone(),
                    max_download_limit: meta.max_download_limit.clone(),
                    max_upload_limit: meta.max_upload_limit.clone(),
                    seed_ratio: meta.seed_ratio.clone(),
                    seed_time: meta.seed_time.clone(),
                    is_complete,
                    awaiting_file_selection: meta.awaiting_file_selection,
                    error_message: live.error_message,
                    num_seeders: live.num_seeders,
                    connections: live.connections,
                });
            } else {
                // Saved torrent not currently loaded in aria2
                items.push(TorrentItem {
                    info_hash: h.clone(),
                    gid: None,
                    name: if meta.name.is_empty() { h.clone() } else { meta.name.clone() },
                    tags: meta.tags.clone(),
                    size_bytes: 0,
                    completed_bytes: 0,
                    uploaded_bytes: 0,
                    download_speed: 0,
                    upload_speed: 0,
                    status: TorrentStatus::StoppedSaved,
                    selected_files: meta.selected_files.clone(),
                    max_download_limit: meta.max_download_limit.clone(),
                    max_upload_limit: meta.max_upload_limit.clone(),
                    seed_ratio: meta.seed_ratio.clone(),
                    seed_time: meta.seed_time.clone(),
                    is_complete: meta.is_complete,
                    awaiting_file_selection: meta.awaiting_file_selection,
                    error_message: None,
                    num_seeders: None,
                    connections: None,
                });
            }
        }

        // 2. Active aria2 downloads not yet tracked in state.json
        for (h, live) in live_by_hash {
            let total: u64 = live.total_length.parse().unwrap_or(0);
            let completed: u64 = live.completed_length.parse().unwrap_or(0);
            let uploaded: u64 = live.upload_length.parse().unwrap_or(0);
            let dl_speed: u64 = live.download_speed.parse().unwrap_or(0);
            let ul_speed: u64 = live.upload_speed.parse().unwrap_or(0);
            let is_seeder = live.seeder.as_deref() == Some("true");
            let is_complete =
                (total > 0 && completed >= total) || is_seeder || live.status == "complete";

            let name = live
                .bittorrent
                .as_ref()
                .and_then(|bt| bt.info.as_ref())
                .and_then(|i| i.name.clone())
                .unwrap_or_else(|| h.clone());

            let tags = self
                .pending_magnet_tags
                .remove(&live.gid)
                .or_else(|| self.pending_magnet_tags.remove(&h))
                .unwrap_or_default();

            let mut new_meta = TorrentMetadata::new(h.clone(), name.clone(), tags.clone(), &global);
            new_meta.is_complete = is_complete;
            self.state.torrents.insert(h.clone(), new_meta);

            let status = TorrentStatus::from_aria2(&live.status, is_complete, is_seeder, false);

            items.push(TorrentItem {
                info_hash: h,
                gid: Some(live.gid),
                name,
                tags,
                size_bytes: total,
                completed_bytes: completed,
                uploaded_bytes: uploaded,
                download_speed: dl_speed,
                upload_speed: ul_speed,
                status,
                selected_files: String::new(),
                max_download_limit: "0".to_string(),
                max_upload_limit: "0".to_string(),
                seed_ratio: global.default_seed_ratio.clone(),
                seed_time: global.default_seed_time.clone(),
                is_complete,
                awaiting_file_selection: false,
                error_message: live.error_message,
                num_seeders: live.num_seeders,
                connections: live.connections,
            });
        }

        // 3. Magnets still fetching metadata (no info_hash yet)
        for m in magnet_fetching {
            if m.followed_by.is_some() || (m.status != "active" && m.status != "waiting") {
                continue;
            }
            let key = m.info_hash.as_deref().unwrap_or(&m.gid).to_lowercase();
            let tags = self
                .pending_magnet_tags
                .get(&m.gid)
                .cloned()
                .or_else(|| self.pending_magnet_tags.get(&key).cloned())
                .unwrap_or_default();
            let name = m
                .bittorrent
                .as_ref()
                .and_then(|bt| bt.info.as_ref())
                .and_then(|i| i.name.clone())
                .unwrap_or_else(|| format!("Magnet ({})", &m.gid[..m.gid.len().min(8)]));
            items.push(TorrentItem {
                info_hash: key,
                gid: Some(m.gid),
                name,
                tags,
                size_bytes: 0,
                completed_bytes: 0,
                uploaded_bytes: 0,
                download_speed: m.download_speed.parse().unwrap_or(0),
                upload_speed: m.upload_speed.parse().unwrap_or(0),
                status: TorrentStatus::FetchingMetadata,
                selected_files: String::new(),
                max_download_limit: "0".to_string(),
                max_upload_limit: "0".to_string(),
                seed_ratio: global.default_seed_ratio.clone(),
                seed_time: global.default_seed_time.clone(),
                is_complete: false,
                awaiting_file_selection: false,
                error_message: m.error_message,
                num_seeders: m.num_seeders,
                connections: m.connections,
            });
        }

        // Sort alphabetically
        items.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

        let _ = self.state.save(&self.base_dir);

        let _ = self
            .worker_tx
            .send(WorkerMessage::SyncUpdate {
                items,
                global_stat,
                global_dl_limit,
                global_ul_limit,
            })
            .await;
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// Returns the first `DownloadStatus` found in active or waiting downloads for the given info-hash.
    async fn find_live_download(&self, info_hash: &str) -> Option<DownloadStatus> {
        let h = info_hash.to_lowercase();
        let matches_hash = |d: &DownloadStatus| {
            d.info_hash.as_deref().map(|s| s.to_lowercase()) == Some(h.clone())
        };
        if let Ok(active) = self.process_mgr.client.tell_active().await {
            if let Some(item) = active.into_iter().find(matches_hash) {
                return Some(item);
            }
        }
        if let Ok(waiting) = self.process_mgr.client.tell_waiting(0, 1000).await {
            if let Some(item) = waiting.into_iter().find(matches_hash) {
                return Some(item);
            }
        }
        None
    }

    /// Returns the first GID found in active or waiting downloads for the given info-hash.
    async fn find_live_gid(&self, info_hash: &str) -> Option<String> {
        self.find_live_download(info_hash).await.map(|d| d.gid)
    }

    // -----------------------------------------------------------------------
    // Torrent control methods
    // -----------------------------------------------------------------------

    async fn toggle_start_pause(&mut self, info_hash: &str) {
        let h = info_hash.to_lowercase();
        if let Some(item) = self.find_live_download(&h).await {
            if item.status == "active" {
                let _ = self.process_mgr.client.pause(&item.gid).await;
                return;
            } else if item.status == "paused" {
                let _ = self.process_mgr.client.unpause(&item.gid).await;
                if let Some(entry) = self.state.torrents.get_mut(&h) {
                    entry.awaiting_file_selection = false;
                    let _ = self.state.save(&self.base_dir);
                }
                return;
            }
        }
        // Otherwise load from .torrent/ and start
        self.start_or_resume(&h, false).await;
    }

    async fn start_or_resume(&mut self, info_hash: &str, verify: bool) {
        let h = info_hash.to_lowercase();

        // Remove any stopped/completed result for this hash first
        if let Ok(stopped) = self.process_mgr.client.tell_stopped(0, 1000).await {
            for item in stopped {
                if item.info_hash.as_deref().map(|s| s.to_lowercase()) == Some(h.clone()) {
                    let _ = self
                        .process_mgr
                        .client
                        .remove_download_result(&item.gid)
                        .await;
                }
            }
        }

        // Read the archived .torrent file
        let meta_dir = AppStateStorage::torrent_meta_dir(&self.base_dir);
        let torrent_file = meta_dir.join(format!("{}.torrent", h));
        if !torrent_file.is_file() {
            let _ = self
                .worker_tx
                .send(WorkerMessage::ShowMessage {
                    title: "File Not Found".to_string(),
                    message: format!("Metadata file {:?} does not exist.", torrent_file),
                    is_error: true,
                })
                .await;
            return;
        }

        let bytes = match std::fs::read(&torrent_file) {
            Ok(b) => b,
            Err(e) => {
                let _ = self
                    .worker_tx
                    .send(WorkerMessage::ShowMessage {
                        title: "Read Error".to_string(),
                        message: format!("Failed to read {:?}: {}", torrent_file, e),
                        is_error: true,
                    })
                    .await;
                return;
            }
        };

        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        let global = self.state.global_limits.clone();
        let meta = self.state.torrents.get(&h).cloned().unwrap_or_else(|| {
            TorrentMetadata::new(h.clone(), h.clone(), Vec::new(), &global)
        });

        let mut opts = serde_json::Map::new();
        let dir_clean = clean_path(&self.base_dir).to_string_lossy().to_string();
        opts.insert("dir".to_string(), serde_json::json!(dir_clean));
        opts.insert("file-allocation".to_string(), serde_json::json!("none"));
        if !meta.selected_files.is_empty() {
            opts.insert("select-file".to_string(), serde_json::json!(meta.selected_files));
        }
        if meta.max_download_limit != "0" {
            opts.insert(
                "max-download-limit".to_string(),
                serde_json::json!(meta.max_download_limit),
            );
        }
        if meta.max_upload_limit != "0" {
            opts.insert(
                "max-upload-limit".to_string(),
                serde_json::json!(meta.max_upload_limit),
            );
        }
        if !meta.seed_ratio.is_empty() {
            opts.insert("seed-ratio".to_string(), serde_json::json!(meta.seed_ratio));
        }
        if !meta.seed_time.is_empty() && meta.seed_time != "0" {
            opts.insert("seed-time".to_string(), serde_json::json!(meta.seed_time));
        }
        if verify {
            opts.insert("check-integrity".to_string(), serde_json::json!("true"));
        } else if meta.is_complete {
            opts.insert("bt-seed-unverified".to_string(), serde_json::json!("true"));
        }

        if let Err(e) = self
            .process_mgr
            .client
            .add_torrent(&b64, vec![], serde_json::Value::Object(opts))
            .await
        {
            let _ = self
                .worker_tx
                .send(WorkerMessage::ShowMessage {
                    title: "Add Torrent Failed".to_string(),
                    message: format!("RPC addTorrent error: {}", e),
                    is_error: true,
                })
                .await;
        }
    }

    async fn pause_torrent(&mut self, info_hash: &str) {
        let h = info_hash.to_lowercase();
        if let Some(gid) = self.find_live_gid(&h).await {
            let _ = self.process_mgr.client.pause(&gid).await;
        }
    }

    async fn verify_recheck(&mut self, info_hash: &str) {
        let h = info_hash.to_lowercase();
        // Remove from active or waiting before re-adding with integrity check
        if let Some(gid) = self.find_live_gid(&h).await {
            let _ = self.process_mgr.client.remove(&gid).await;
        }
        self.start_or_resume(&h, true).await;
    }

    async fn add_torrent_path(&mut self, file_path_str: &str, tags: Vec<String>) {
        let clean_input = clean_path_str(&strip_quotes(file_path_str));
        let path = std::path::PathBuf::from(&clean_input);

        if !path.is_file() {
            let _ = self
                .worker_tx
                .send(WorkerMessage::ShowMessage {
                    title: "File Not Found".to_string(),
                    message: format!("Torrent file does not exist: {}", clean_input),
                    is_error: true,
                })
                .await;
            return;
        }

        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                let _ = self
                    .worker_tx
                    .send(WorkerMessage::ShowMessage {
                        title: "Read Error".to_string(),
                        message: format!("Failed to read torrent file: {}", e),
                        is_error: true,
                    })
                    .await;
                return;
            }
        };

        let parsed_info = parse_torrent_info(&bytes);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);

        let mut opts = serde_json::Map::new();
        let dir_clean = clean_path(&self.base_dir).to_string_lossy().to_string();
        opts.insert("dir".to_string(), serde_json::json!(dir_clean));
        opts.insert("pause".to_string(), serde_json::json!("true"));
        opts.insert("file-allocation".to_string(), serde_json::json!("none"));

        let gid = match self
            .process_mgr
            .client
            .add_torrent(&b64, vec![], serde_json::Value::Object(opts))
            .await
        {
            Ok(id) => id,
            Err(e) => {
                let _ = self
                    .worker_tx
                    .send(WorkerMessage::ShowMessage {
                        title: "Add Torrent Failed".to_string(),
                        message: format!("Failed to add torrent via RPC: {}", e),
                        is_error: true,
                    })
                    .await;
                return;
            }
        };

        // Resolve the info-hash
        let status = self.process_mgr.client.tell_status(&gid).await.ok();
        let info_hash = status
            .as_ref()
            .and_then(|s| s.info_hash.clone())
            .or_else(|| parsed_info.as_ref().map(|p| p.0.clone()))
            .unwrap_or_else(|| gid.clone())
            .to_lowercase();

        let name = parsed_info
            .as_ref()
            .map(|p| p.1.clone())
            .unwrap_or_else(|| {
                path.file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| info_hash.clone())
            });

        // Archive the .torrent file and persist metadata
        let _ = archive_torrent_file(&path, &self.base_dir, &info_hash);

        let global = self.state.global_limits.clone();
        let mut new_meta =
            TorrentMetadata::new(info_hash.clone(), name.clone(), tags, &global);
        new_meta.awaiting_file_selection = true;
        self.state.torrents.insert(info_hash.clone(), new_meta);
        let _ = self.state.save(&self.base_dir);

        // Fetch file list for the selection modal
        if let Ok(files) = self.process_mgr.client.get_files(&gid).await {
            let selection_items: Vec<FileSelectionItem> = files
                .into_iter()
                .map(|f| FileSelectionItem {
                    index: f.index,
                    path: f.path,
                    length: f.length.parse().unwrap_or(0),
                    selected: f.selected == "true",
                })
                .collect();
            let _ = self
                .worker_tx
                .send(WorkerMessage::FilesLoaded {
                    info_hash,
                    gid: Some(gid),
                    files: selection_items,
                })
                .await;
        }
    }

    async fn add_torrent_magnet(&mut self, magnet_str: &str, tags: Vec<String>) {
        let clean_magnet = strip_quotes(magnet_str);
        if !clean_magnet.starts_with("magnet:") {
            let _ = self
                .worker_tx
                .send(WorkerMessage::ShowMessage {
                    title: "Invalid Magnet URI".to_string(),
                    message: "URI must begin with 'magnet:'".to_string(),
                    is_error: true,
                })
                .await;
            return;
        }

        let mut opts = serde_json::Map::new();
        let dir_clean = clean_path(&self.base_dir).to_string_lossy().to_string();
        opts.insert("dir".to_string(), serde_json::json!(dir_clean));
        opts.insert("bt-save-metadata".to_string(), serde_json::json!("true"));
        opts.insert("pause-metadata".to_string(), serde_json::json!("true"));
        opts.insert("file-allocation".to_string(), serde_json::json!("none"));

        let gid = match self
            .process_mgr
            .client
            .add_uri(vec![clean_magnet.clone()], serde_json::Value::Object(opts))
            .await
        {
            Ok(g) => g,
            Err(e) => {
                let _ = self
                    .worker_tx
                    .send(WorkerMessage::ShowMessage {
                        title: "Add Magnet Failed".to_string(),
                        message: format!("Failed to add magnet via RPC: {}", e),
                        is_error: true,
                    })
                    .await;
                return;
            }
        };

        // Track pending tags keyed by GID
        self.pending_magnet_tags.insert(gid.clone(), tags.clone());

        // Also key by info-hash if extractable from the URI
        if let Some(hash) = extract_hash_from_magnet(&clean_magnet) {
            let h = hash.to_lowercase();
            self.pending_magnet_tags.insert(h, tags);
        }
    }

    async fn fetch_files_for_selection(&mut self, info_hash: &str) {
        let h = info_hash.to_lowercase();

        if let Some(gid) = self.find_live_gid(&h).await {
            if let Ok(files) = self.process_mgr.client.get_files(&gid).await {
                let saved_selected: Option<std::collections::HashSet<String>> = self
                    .state
                    .torrents
                    .get(&h)
                    .map(|m| &m.selected_files)
                    .filter(|s| !s.is_empty())
                    .map(|s| s.split(',').map(|x| x.trim().to_string()).collect());

                let selection_items: Vec<FileSelectionItem> = files
                    .into_iter()
                    .map(|f| {
                        let is_sel = if let Some(ref set) = saved_selected {
                            set.contains(&f.index)
                        } else {
                            f.selected == "true"
                        };
                        FileSelectionItem {
                            index: f.index,
                            path: f.path,
                            length: f.length.parse().unwrap_or(0),
                            selected: is_sel,
                        }
                    })
                    .collect();

                let _ = self
                    .worker_tx
                    .send(WorkerMessage::FilesLoaded {
                        info_hash: h,
                        gid: Some(gid),
                        files: selection_items,
                    })
                    .await;
                return;
            }
        }

        let _ = self
            .worker_tx
            .send(WorkerMessage::ShowMessage {
                title: "File Selection Unavailable".to_string(),
                message:
                    "Torrent must be loaded in aria2c to inspect or modify file list. Press 's' to start/load it first."
                        .to_string(),
                is_error: false,
            })
            .await;
    }

    async fn apply_file_selection(&mut self, info_hash: &str, selected_indices: &str) {
        let h = info_hash.to_lowercase();
        if let Some(entry) = self.state.torrents.get_mut(&h) {
            entry.selected_files = selected_indices.to_string();
            entry.awaiting_file_selection = false;
            let _ = self.state.save(&self.base_dir);
        }

        if let Some(gid) = self.find_live_gid(&h).await {
            let mut opts = serde_json::Map::new();
            if !selected_indices.is_empty() {
                opts.insert(
                    "select-file".to_string(),
                    serde_json::json!(selected_indices),
                );
            }
            opts.insert("file-allocation".to_string(), serde_json::json!("none"));
            let _ = self
                .process_mgr
                .client
                .change_option(&gid, serde_json::Value::Object(opts))
                .await;
            let _ = self.process_mgr.client.unpause(&gid).await;
        }
    }

    async fn update_torrent_options(
        &mut self,
        info_hash: &str,
        max_dl: String,
        max_ul: String,
        ratio: String,
        time: String,
    ) {
        let h = info_hash.to_lowercase();
        if let Some(entry) = self.state.torrents.get_mut(&h) {
            entry.max_download_limit = max_dl.clone();
            entry.max_upload_limit = max_ul.clone();
            entry.seed_ratio = ratio.clone();
            entry.seed_time = time.clone();
            let _ = self.state.save(&self.base_dir);
        }

        if let Some(gid) = self.find_live_gid(&h).await {
            let mut opts = serde_json::Map::new();
            opts.insert("max-download-limit".to_string(), serde_json::json!(max_dl));
            opts.insert("max-upload-limit".to_string(), serde_json::json!(max_ul));
            opts.insert("seed-ratio".to_string(), serde_json::json!(ratio));
            if time != "0" {
                opts.insert("seed-time".to_string(), serde_json::json!(time));
            }
            let _ = self
                .process_mgr
                .client
                .change_option(&gid, serde_json::Value::Object(opts))
                .await;
        }
    }

    async fn update_global_options(
        &mut self,
        max_dl: String,
        max_ul: String,
        ratio: String,
        time: String,
    ) {
        self.state.global_limits.max_overall_download_limit = max_dl.clone();
        self.state.global_limits.max_overall_upload_limit = max_ul.clone();
        self.state.global_limits.default_seed_ratio = ratio.clone();
        self.state.global_limits.default_seed_time = time.clone();
        let _ = self.state.save(&self.base_dir);

        let mut opts = serde_json::Map::new();
        opts.insert(
            "max-overall-download-limit".to_string(),
            serde_json::json!(max_dl),
        );
        opts.insert(
            "max-overall-upload-limit".to_string(),
            serde_json::json!(max_ul),
        );
        opts.insert("seed-ratio".to_string(), serde_json::json!(ratio));
        if time != "0" {
            opts.insert("seed-time".to_string(), serde_json::json!(time));
        }
        let _ = self
            .process_mgr
            .client
            .change_global_option(serde_json::Value::Object(opts))
            .await;
    }

    async fn delete_torrent(&mut self, info_hash: &str, delete_files: bool) {
        let h = info_hash.to_lowercase();

        // Collect all GIDs for this torrent across active / waiting / stopped
        let mut gids_to_remove: Vec<(String, bool)> = Vec::new();
        if let Ok(active) = self.process_mgr.client.tell_active().await {
            for item in active {
                if item.info_hash.as_deref().map(|s| s.to_lowercase()) == Some(h.clone())
                    || item.gid == h
                {
                    gids_to_remove.push((item.gid, true));
                }
            }
        }
        if let Ok(waiting) = self.process_mgr.client.tell_waiting(0, 1000).await {
            for item in waiting {
                if item.info_hash.as_deref().map(|s| s.to_lowercase()) == Some(h.clone())
                    || item.gid == h
                {
                    gids_to_remove.push((item.gid, true));
                }
            }
        }
        if let Ok(stopped) = self.process_mgr.client.tell_stopped(0, 1000).await {
            for item in stopped {
                if item.info_hash.as_deref().map(|s| s.to_lowercase()) == Some(h.clone())
                    || item.gid == h
                {
                    gids_to_remove.push((item.gid, false));
                }
            }
        }

        for (gid, is_active_or_waiting) in gids_to_remove {
            if is_active_or_waiting {
                let _ = self.process_mgr.client.force_remove(&gid).await;
            }
            let _ = self.process_mgr.client.remove_download_result(&gid).await;
        }

        // Brief delay to let aria2 release file handles (especially on Windows)
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Delete payload files if requested
        if delete_files {
            let mut candidate_names: Vec<String> = Vec::new();
            if let Some(entry) = self.state.torrents.get(&h) {
                if !entry.name.is_empty() && !entry.name.starts_with("[Magnet]") {
                    candidate_names.push(entry.name.clone());
                }
            }
            let meta_dir = AppStateStorage::torrent_meta_dir(&self.base_dir);
            let torrent_file = meta_dir.join(format!("{}.torrent", h));
            if torrent_file.is_file() {
                if let Ok(bytes) = std::fs::read(&torrent_file) {
                    if let Some((_, bencode_name)) = parse_torrent_info(&bytes) {
                        if !bencode_name.is_empty() && !candidate_names.contains(&bencode_name) {
                            candidate_names.push(bencode_name);
                        }
                    }
                }
            }
            for name in candidate_names {
                remove_path_with_retry(&self.base_dir.join(&name)).await;
                let aria2_control = self.base_dir.join(format!("{}.aria2", name));
                if aria2_control.is_file() {
                    let _ = std::fs::remove_file(&aria2_control);
                }
            }
        }

        // Remove the archived .torrent file and state entry
        let meta_dir = AppStateStorage::torrent_meta_dir(&self.base_dir);
        let torrent_file = meta_dir.join(format!("{}.torrent", h));
        if torrent_file.is_file() {
            let _ = std::fs::remove_file(&torrent_file);
        }
        self.state.torrents.remove(&h);
        let _ = self.state.save(&self.base_dir);
    }
}

/// Attempts to remove a file or directory, retrying up to 4 times on failure.
/// Uses async sleep so the executor is not blocked.
async fn remove_path_with_retry(path: &Path) {
    if !path.exists() {
        return;
    }
    for _ in 0..4 {
        let res = if path.is_dir() {
            tokio::fs::remove_dir_all(path).await
        } else {
            tokio::fs::remove_file(path).await
        };
        if res.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
