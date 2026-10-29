use base64::Engine;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::sync::mpsc::{Receiver, Sender};

use crate::process::ProcessManager;
use crate::rpc::{DownloadStatus, GlobalStat};
use crate::state::{
    AppStateStorage, TorrentMetadata, archive_torrent_file, detect_and_move_saved_metadata,
    parse_torrent_info,
};


#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub enum TorrentStatus {
    Downloading,
    Seeding,
    Paused,
    FetchingMetadata,
    SelectFiles,
    StoppedSaved,
    Error,
}

impl TorrentStatus {
    #[allow(dead_code)]
    pub fn display_str(&self) -> &'static str {
        match self {
            TorrentStatus::Downloading => "Downloading",
            TorrentStatus::Seeding => "Seeding",
            TorrentStatus::Paused => "Paused",
            TorrentStatus::FetchingMetadata => "Fetching Metadata...",
            TorrentStatus::SelectFiles => "Select Files (f)",
            TorrentStatus::StoppedSaved => "Stopped (Saved)",
            TorrentStatus::Error => "Error",
        }
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct TorrentItem {
    pub info_hash: String,
    pub gid: Option<String>,
    pub name: String,
    pub tags: Vec<String>,
    pub size_bytes: u64,
    pub completed_bytes: u64,
    pub uploaded_bytes: u64,
    pub download_speed: u64,
    pub upload_speed: u64,
    pub status: TorrentStatus,
    pub selected_files: String,
    pub max_download_limit: String,
    pub max_upload_limit: String,
    pub seed_ratio: String,
    pub seed_time: String,
    pub is_complete: bool,
    pub awaiting_file_selection: bool,
    pub error_message: Option<String>,
    pub num_seeders: Option<String>,
    pub connections: Option<String>,
}

impl TorrentItem {
    pub fn progress_pct(&self) -> f64 {
        if self.size_bytes == 0 {
            if self.is_complete {
                100.0
            } else {
                0.0
            }
        } else {
            let pct = (self.completed_bytes as f64 / self.size_bytes as f64) * 100.0;
            pct.clamp(0.0, 100.0)
        }
    }

    pub fn ratio(&self) -> f64 {
        let denom = if self.completed_bytes > 0 {
            self.completed_bytes as f64
        } else if self.size_bytes > 0 {
            self.size_bytes as f64
        } else {
            0.0
        };

        if denom > 0.0 {
            self.uploaded_bytes as f64 / denom
        } else {
            0.0
        }
    }

    pub fn eta_seconds(&self) -> Option<u64> {
        if self.download_speed > 0 && self.size_bytes > self.completed_bytes {
            Some((self.size_bytes - self.completed_bytes) / self.download_speed)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone)]
pub struct FileSelectionItem {
    pub index: String,
    pub path: String,
    pub length: u64,
    pub selected: bool,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum Modal {
    None,
    AddTorrent {
        path_or_magnet: String,
        tags: String,
        active_field: usize, // 0: path/magnet, 1: tags
        cursor_pos: usize,
    },
    FileSelection {
        info_hash: String,
        gid: Option<String>,
        files: Vec<FileSelectionItem>,
        selected_index: usize,
    },
    EditTags {
        info_hash: String,
        tags: String,
        cursor_pos: usize,
    },
    TorrentOptions {
        info_hash: String,
        max_download_limit: String,
        max_upload_limit: String,
        seed_ratio: String,
        seed_time: String,
        active_field: usize, // 0..3
        cursor_pos: usize,
    },
    GlobalOptions {
        max_overall_download_limit: String,
        max_overall_upload_limit: String,
        default_seed_ratio: String,
        default_seed_time: String,
        active_field: usize, // 0..3
        cursor_pos: usize,
    },
    DeleteConfirm {
        info_hash: String,
        name: String,
        delete_files_from_disk: bool, // false: Choice 1 (keep), true: Choice 2 (delete)
    },
    Message {
        title: String,
        message: String,
        is_error: bool,
    },
}

#[derive(Debug)]
pub enum Action {
    Refresh,
    ToggleStartPause(String),
    BatchStart(Vec<String>),
    BatchPause(Vec<String>),
    VerifyRecheck(String),
    AddTorrentPath {
        path: String,
        tags: Vec<String>,
    },
    AddTorrentMagnet {
        magnet: String,
        tags: Vec<String>,
    },
    FetchFilesForSelection(String),
    ApplyFileSelection {
        info_hash: String,
        selected_indices: String,
    },
    UpdateTags {
        info_hash: String,
        tags: Vec<String>,
    },
    UpdateTorrentOptions {
        info_hash: String,
        max_dl: String,
        max_ul: String,
        ratio: String,
        time: String,
    },
    UpdateGlobalOptions {
        max_dl: String,
        max_ul: String,
        ratio: String,
        time: String,
    },
    DeleteTorrent {
        info_hash: String,
        delete_files: bool,
    },
    Quit,
}

#[derive(Debug)]
pub enum WorkerMessage {
    SyncUpdate {
        items: Vec<TorrentItem>,
        global_stat: GlobalStat,
        global_dl_limit: String,
        global_ul_limit: String,
    },
    FilesLoaded {
        info_hash: String,
        gid: Option<String>,
        files: Vec<FileSelectionItem>,
    },
    AutoOpenFileSelection {
        info_hash: String,
        gid: Option<String>,
        files: Vec<FileSelectionItem>,
    },
    ShowMessage {
        title: String,
        message: String,
        is_error: bool,
    },
}

pub struct App {
    pub dir: PathBuf,
    pub spawned_by_us: bool,
    pub items: Vec<TorrentItem>,
    pub selected_index: usize,
    pub tag_filter_index: usize,
    pub global_stat: GlobalStat,
    pub global_dl_limit: String,
    pub global_ul_limit: String,
    pub modal: Modal,
    pub should_quit: bool,
    pub action_tx: Sender<Action>,
    pub worker_rx: Receiver<WorkerMessage>,
}

impl App {
    pub fn new(
        dir: PathBuf,
        spawned_by_us: bool,
        action_tx: Sender<Action>,
        worker_rx: Receiver<WorkerMessage>,
    ) -> Self {
        Self {
            dir,
            spawned_by_us,
            items: Vec::new(),
            selected_index: 0,
            tag_filter_index: 0,
            global_stat: GlobalStat::default(),
            global_dl_limit: "0".to_string(),
            global_ul_limit: "0".to_string(),
            modal: Modal::None,
            should_quit: false,
            action_tx,
            worker_rx,
        }
    }

    pub fn available_tags(&self) -> Vec<String> {
        let mut set = std::collections::BTreeSet::new();
        for item in &self.items {
            for tag in &item.tags {
                if !tag.is_empty() {
                    set.insert(tag.clone());
                }
            }
        }
        let mut tags = vec!["All".to_string()];
        tags.extend(set);
        tags
    }

    pub fn active_tag_filter(&self) -> Option<String> {
        let tags = self.available_tags();
        if self.tag_filter_index == 0 || self.tag_filter_index >= tags.len() {
            None
        } else {
            Some(tags[self.tag_filter_index].clone())
        }
    }

    pub fn filtered_items(&self) -> Vec<&TorrentItem> {
        match self.active_tag_filter() {
            None => self.items.iter().collect(),
            Some(tag) => self
                .items
                .iter()
                .filter(|it| it.tags.iter().any(|t| t.eq_ignore_ascii_case(&tag)))
                .collect(),
        }
    }

    pub fn selected_torrent(&self) -> Option<&TorrentItem> {
        let filtered = self.filtered_items();
        if filtered.is_empty() {
            None
        } else {
            let idx = self.selected_index.min(filtered.len() - 1);
            Some(filtered[idx])
        }
    }

    pub fn next_torrent(&mut self) {
        let count = self.filtered_items().len();
        if count > 0 && self.selected_index + 1 < count {
            self.selected_index += 1;
        }
    }

    pub fn prev_torrent(&mut self) {
        if self.selected_index > 0 {
            self.selected_index -= 1;
        }
    }

    pub fn next_tag_filter(&mut self) {
        let tags = self.available_tags();
        if !tags.is_empty() {
            self.tag_filter_index = (self.tag_filter_index + 1) % tags.len();
            self.selected_index = 0;
        }
    }

    pub fn prev_tag_filter(&mut self) {
        let tags = self.available_tags();
        if !tags.is_empty() {
            if self.tag_filter_index == 0 {
                self.tag_filter_index = tags.len() - 1;
            } else {
                self.tag_filter_index -= 1;
            }
            self.selected_index = 0;
        }
    }

    pub fn process_worker_messages(&mut self) {
        while let Ok(msg) = self.worker_rx.try_recv() {
            match msg {
                WorkerMessage::SyncUpdate {
                    items,
                    global_stat,
                    global_dl_limit,
                    global_ul_limit,
                } => {
                    self.items = items;
                    self.global_stat = global_stat;
                    self.global_dl_limit = global_dl_limit;
                    self.global_ul_limit = global_ul_limit;

                    let filtered_count = self.filtered_items().len();
                    if filtered_count > 0 && self.selected_index >= filtered_count {
                        self.selected_index = filtered_count - 1;
                    }
                }
                WorkerMessage::FilesLoaded {
                    info_hash,
                    gid,
                    files,
                } => {
                    self.modal = Modal::FileSelection {
                        info_hash,
                        gid,
                        files,
                        selected_index: 0,
                    };
                }
                WorkerMessage::AutoOpenFileSelection {
                    info_hash,
                    gid,
                    files,
                } => {
                    // Only auto-open if no other modal is currently active
                    if matches!(self.modal, Modal::None) {
                        self.modal = Modal::FileSelection {
                            info_hash,
                            gid,
                            files,
                            selected_index: 0,
                        };
                    }
                }
                WorkerMessage::ShowMessage {
                    title,
                    message,
                    is_error,
                } => {
                    self.modal = Modal::Message {
                        title,
                        message,
                        is_error,
                    };
                }
            }
        }
    }
}

// Background sync worker
pub struct AppWorker {
    pub base_dir: PathBuf,
    pub process_mgr: ProcessManager,
    pub state: AppStateStorage,
    pub pending_magnet_tags: HashMap<String, Vec<String>>, // GID or infoHash -> tags
    pub action_rx: Receiver<Action>,
    pub worker_tx: Sender<WorkerMessage>,
}

impl AppWorker {
    pub async fn run(mut self) {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(1000));

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
            Action::ApplyFileSelection {
                info_hash,
                selected_indices,
            } => {
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
            Action::UpdateTorrentOptions {
                info_hash,
                max_dl,
                max_ul,
                ratio,
                time,
            } => {
                self.update_torrent_options(&info_hash, max_dl, max_ul, ratio, time).await;
                self.sync_aria2_and_state(true).await;
            }
            Action::UpdateGlobalOptions {
                max_dl,
                max_ul,
                ratio,
                time,
            } => {
                self.update_global_options(max_dl, max_ul, ratio, time).await;
                self.sync_aria2_and_state(true).await;
            }
            Action::DeleteTorrent {
                info_hash,
                delete_files,
            } => {
                self.delete_torrent(&info_hash, delete_files).await;
                self.sync_aria2_and_state(true).await;
            }
            Action::Quit => {}
        }
    }

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

        // Priority order: stopped (lowest) -> waiting -> active (highest).
        // Evaluating active last ensures that active tasks with live speeds overwrite any stopped entries.
        let all_downloads = stopped.into_iter().chain(waiting.into_iter()).chain(active.into_iter());
        for d in all_downloads {
            let is_metadata = d.info_hash.is_none()
                || d.files.as_ref().map_or(false, |fl| fl.iter().any(|f| f.path.starts_with("[METADATA]")))
                || d.followed_by.as_ref().map_or(false, |f| !f.is_empty());

            if is_metadata {
                magnet_fetching.push(d);
            } else if let Some(ref hash) = d.info_hash {
                let h = hash.to_lowercase();
                // Check if metadata completed and aria2 saved <hash>.torrent in <DIR>
                if let Some(dest) = detect_and_move_saved_metadata(&self.base_dir, &h) {
                    if let Ok(bytes) = std::fs::read(&dest) {
                        if let Some((_, name)) = parse_torrent_info(&bytes) {
                            if let Some(entry) = self.state.torrents.get_mut(&h) {
                                if entry.name.is_empty() || entry.name == h {
                                    entry.name = name;
                                }
                            }
                        }
                    }
                }
                live_by_hash.insert(h, d);
            }
        }

        // Handle followedBy metadata downloads
        for m in &magnet_fetching {
            if let Some(ref followed) = m.followed_by {
                for next_gid in followed {
                    if let Ok(next_stat) = self.process_mgr.client.tell_status(next_gid).await {
                        if let Some(ref hash) = next_stat.info_hash {
                            let h = hash.to_lowercase();
                            let _ = detect_and_move_saved_metadata(&self.base_dir, &h);
                            // Transfer pending tags
                            let tags = self
                                .pending_magnet_tags
                                .remove(&m.gid)
                                .or_else(|| self.pending_magnet_tags.remove(&h))
                                .unwrap_or_default();

                            let entry = self.state.torrents.entry(h.clone()).or_insert_with(|| {
                                TorrentMetadata {
                                    info_hash: h.clone(),
                                    name: next_stat
                                        .bittorrent
                                        .as_ref()
                                        .and_then(|bt| bt.info.as_ref())
                                        .and_then(|i| i.name.clone())
                                        .unwrap_or_else(|| h.clone()),
                                    torrent_filename: format!("{}.torrent", h),
                                    tags,
                                    selected_files: String::new(),
                                    max_download_limit: "0".to_string(),
                                    max_upload_limit: "0".to_string(),
                                    seed_ratio: self.state.global_limits.default_seed_ratio.clone(),
                                    seed_time: self.state.global_limits.default_seed_time.clone(),
                                    is_complete: false,
                                    awaiting_file_selection: true,
                                }
                            });
                            entry.awaiting_file_selection = true;
                            let _ = self.state.save(&self.base_dir);

                            // Load files and send AutoOpenFileSelection
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
                                let _ = self.worker_tx.send(WorkerMessage::AutoOpenFileSelection {
                                    info_hash: h.clone(),
                                    gid: Some(next_gid.clone()),
                                    files: selection_items,
                                }).await;
                            }
                            // Clean up obsolete metadata download result from aria2
                            let _ = self.process_mgr.client.remove_download_result(&m.gid).await;
                        }
                    }
                }
            }
        }

        // Build unified list of items
        let mut items = Vec::new();

        // 1. Torrents tracked in state.json
        for (h, meta) in &mut self.state.torrents {
            if let Some(live) = live_by_hash.remove(h) {
                // Torrent is live in aria2
                let total: u64 = live.total_length.parse().unwrap_or(0);
                let completed: u64 = live.completed_length.parse().unwrap_or(0);
                let uploaded: u64 = live.upload_length.parse().unwrap_or(0);
                let dl_speed: u64 = live.download_speed.parse().unwrap_or(0);
                let ul_speed: u64 = live.upload_speed.parse().unwrap_or(0);

                let is_complete = meta.is_complete
                    || (total > 0 && completed >= total)
                    || live.seeder.as_deref() == Some("true")
                    || live.status == "complete";
                meta.is_complete = is_complete;

                let status = if meta.awaiting_file_selection {
                    TorrentStatus::SelectFiles
                } else if live.status == "error" {
                    TorrentStatus::Error
                } else if live.status == "paused" {
                    TorrentStatus::Paused
                } else if live.status == "active" {
                    if is_complete || live.seeder.as_deref() == Some("true") {
                        TorrentStatus::Seeding
                    } else {
                        TorrentStatus::Downloading
                    }
                } else if live.status == "complete" {
                    TorrentStatus::StoppedSaved
                } else {
                    TorrentStatus::StoppedSaved
                };

                let name = if let Some(ref bt) = live.bittorrent {
                    if let Some(ref info) = bt.info {
                        if let Some(ref n) = info.name {
                            if !n.is_empty() {
                                meta.name = n.clone();
                                n.clone()
                            } else {
                                meta.name.clone()
                            }
                        } else {
                            meta.name.clone()
                        }
                    } else {
                        meta.name.clone()
                    }
                } else {
                    meta.name.clone()
                };

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
                // Saved torrent not currently active in aria2
                let total = 0;
                let completed = if meta.is_complete { total } else { 0 };
                items.push(TorrentItem {
                    info_hash: h.clone(),
                    gid: None,
                    name: if meta.name.is_empty() { h.clone() } else { meta.name.clone() },
                    tags: meta.tags.clone(),
                    size_bytes: total,
                    completed_bytes: completed,
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

        // 2. Any active aria2 downloads with info_hash not yet in state.json
        for (h, live) in live_by_hash {
            let total: u64 = live.total_length.parse().unwrap_or(0);
            let completed: u64 = live.completed_length.parse().unwrap_or(0);
            let uploaded: u64 = live.upload_length.parse().unwrap_or(0);
            let dl_speed: u64 = live.download_speed.parse().unwrap_or(0);
            let ul_speed: u64 = live.upload_speed.parse().unwrap_or(0);
            let is_comp = (total > 0 && completed >= total)
                || live.seeder.as_deref() == Some("true")
                || live.status == "complete";

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

            self.state.torrents.insert(
                h.clone(),
                TorrentMetadata {
                    info_hash: h.clone(),
                    name: name.clone(),
                    torrent_filename: format!("{}.torrent", h),
                    tags: tags.clone(),
                    selected_files: String::new(),
                    max_download_limit: "0".to_string(),
                    max_upload_limit: "0".to_string(),
                    seed_ratio: self.state.global_limits.default_seed_ratio.clone(),
                    seed_time: self.state.global_limits.default_seed_time.clone(),
                    is_complete: is_comp,
                    awaiting_file_selection: false,
                },
            );

            let status = if live.status == "error" {
                TorrentStatus::Error
            } else if live.status == "paused" {
                TorrentStatus::Paused
            } else if live.status == "active" {
                if is_comp || live.seeder.as_deref() == Some("true") {
                    TorrentStatus::Seeding
                } else {
                    TorrentStatus::Downloading
                }
            } else if live.status == "complete" {
                TorrentStatus::StoppedSaved
            } else {
                TorrentStatus::StoppedSaved
            };

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
                seed_ratio: self.state.global_limits.default_seed_ratio.clone(),
                seed_time: self.state.global_limits.default_seed_time.clone(),
                is_complete: is_comp,
                awaiting_file_selection: false,
                error_message: live.error_message,
                num_seeders: live.num_seeders,
                connections: live.connections,
            });
        }

        // 3. Magnets currently fetching metadata
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
                seed_ratio: self.state.global_limits.default_seed_ratio.clone(),
                seed_time: self.state.global_limits.default_seed_time.clone(),
                is_complete: false,
                awaiting_file_selection: false,
                error_message: m.error_message,
                num_seeders: m.num_seeders,
                connections: m.connections,
            });
        }

        // Sort items by name
        items.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

        let _ = self.state.save(&self.base_dir);

        let _ = self.worker_tx.send(WorkerMessage::SyncUpdate {
            items,
            global_stat,
            global_dl_limit,
            global_ul_limit,
        }).await;
    }

    async fn toggle_start_pause(&mut self, info_hash: &str) {
        let h = info_hash.to_lowercase();
        // Find if active or paused in aria2
        if let Ok(active) = self.process_mgr.client.tell_active().await {
            if let Some(item) = active.into_iter().find(|d| d.info_hash.as_deref().map(|s| s.to_lowercase()) == Some(h.clone())) {
                let _ = self.process_mgr.client.pause(&item.gid).await;
                return;
            }
        }
        if let Ok(waiting) = self.process_mgr.client.tell_waiting(0, 1000).await {
            if let Some(item) = waiting.into_iter().find(|d| d.info_hash.as_deref().map(|s| s.to_lowercase()) == Some(h.clone())) {
                if item.status == "paused" {
                    let _ = self.process_mgr.client.unpause(&item.gid).await;
                    if let Some(entry) = self.state.torrents.get_mut(&h) {
                        entry.awaiting_file_selection = false;
                        let _ = self.state.save(&self.base_dir);
                    }
                    return;
                }
            }
        }

        // If stopped or not loaded in aria2: load from .torrent/
        self.start_or_resume(&h, false).await;
    }

    async fn start_or_resume(&mut self, info_hash: &str, verify: bool) {
        let h = info_hash.to_lowercase();
        // Check if stopped in aria2
        if let Ok(stopped) = self.process_mgr.client.tell_stopped(0, 1000).await {
            for item in stopped {
                if item.info_hash.as_deref().map(|s| s.to_lowercase()) == Some(h.clone()) {
                    let _ = self.process_mgr.client.remove_download_result(&item.gid).await;
                }
            }
        }

        // Read .torrent file
        let meta_dir = AppStateStorage::torrent_meta_dir(&self.base_dir);
        let torrent_file = meta_dir.join(format!("{}.torrent", h));
        if !torrent_file.is_file() {
            let _ = self.worker_tx.send(WorkerMessage::ShowMessage {
                title: "File Not Found".to_string(),
                message: format!("Metadata file {:?} does not exist.", torrent_file),
                is_error: true,
            }).await;
            return;
        }

        let bytes = match std::fs::read(&torrent_file) {
            Ok(b) => b,
            Err(e) => {
                let _ = self.worker_tx.send(WorkerMessage::ShowMessage {
                    title: "Read Error".to_string(),
                    message: format!("Failed to read {:?}: {}", torrent_file, e),
                    is_error: true,
                }).await;
                return;
            }
        };

        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        let meta = self.state.torrents.get(&h).cloned().unwrap_or_else(|| {
            TorrentMetadata {
                info_hash: h.clone(),
                name: h.clone(),
                torrent_filename: format!("{}.torrent", h),
                tags: Vec::new(),
                selected_files: String::new(),
                max_download_limit: "0".to_string(),
                max_upload_limit: "0".to_string(),
                seed_ratio: self.state.global_limits.default_seed_ratio.clone(),
                seed_time: self.state.global_limits.default_seed_time.clone(),
                is_complete: false,
                awaiting_file_selection: false,
            }
        });

        let mut opts = serde_json::Map::new();
        let dir_clean = clean_path(&self.base_dir).to_string_lossy().to_string();
        opts.insert("dir".to_string(), serde_json::json!(dir_clean));
        opts.insert("file-allocation".to_string(), serde_json::json!("none"));
        if !meta.selected_files.is_empty() {
            opts.insert("select-file".to_string(), serde_json::json!(meta.selected_files));
        }
        if meta.max_download_limit != "0" {
            opts.insert("max-download-limit".to_string(), serde_json::json!(meta.max_download_limit));
        }
        if meta.max_upload_limit != "0" {
            opts.insert("max-upload-limit".to_string(), serde_json::json!(meta.max_upload_limit));
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

        if let Err(e) = self.process_mgr.client.add_torrent(&b64, vec![], serde_json::Value::Object(opts)).await {
            let _ = self.worker_tx.send(WorkerMessage::ShowMessage {
                title: "Add Torrent Failed".to_string(),
                message: format!("RPC addTorrent error: {}", e),
                is_error: true,
            }).await;
        }
    }

    async fn pause_torrent(&mut self, info_hash: &str) {
        let h = info_hash.to_lowercase();
        if let Ok(active) = self.process_mgr.client.tell_active().await {
            for item in active {
                if item.info_hash.as_deref().map(|s| s.to_lowercase()) == Some(h.clone()) {
                    let _ = self.process_mgr.client.pause(&item.gid).await;
                }
            }
        }
    }

    async fn verify_recheck(&mut self, info_hash: &str) {
        let h = info_hash.to_lowercase();
        // If active in aria2, remove it first
        if let Ok(active) = self.process_mgr.client.tell_active().await {
            for item in active {
                if item.info_hash.as_deref().map(|s| s.to_lowercase()) == Some(h.clone()) {
                    let _ = self.process_mgr.client.remove(&item.gid).await;
                }
            }
        }
        if let Ok(waiting) = self.process_mgr.client.tell_waiting(0, 1000).await {
            for item in waiting {
                if item.info_hash.as_deref().map(|s| s.to_lowercase()) == Some(h.clone()) {
                    let _ = self.process_mgr.client.remove(&item.gid).await;
                }
            }
        }

        self.start_or_resume(&h, true).await;
    }

    async fn add_torrent_path(&mut self, file_path_str: &str, tags: Vec<String>) {
        let clean_input_path = clean_path_str(&strip_quotes(file_path_str));
        let path = PathBuf::from(&clean_input_path);

        if !path.is_file() {
            let _ = self.worker_tx.send(WorkerMessage::ShowMessage {
                title: "File Not Found".to_string(),
                message: format!("Torrent file does not exist: {}", clean_input_path),
                is_error: true,
            }).await;
            return;
        }

        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                let _ = self.worker_tx.send(WorkerMessage::ShowMessage {
                    title: "Read Error".to_string(),
                    message: format!("Failed to read torrent file: {}", e),
                    is_error: true,
                }).await;
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

        let gid = match self.process_mgr.client.add_torrent(&b64, vec![], serde_json::Value::Object(opts)).await {
            Ok(id) => id,
            Err(e) => {
                let _ = self.worker_tx.send(WorkerMessage::ShowMessage {
                    title: "Add Torrent Failed".to_string(),
                    message: format!("Failed to add torrent via RPC: {}", e),
                    is_error: true,
                }).await;
                return;
            }
        };

        // Query status
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
            .unwrap_or_else(|| path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| info_hash.clone()));

        // Archive .torrent file
        let _ = archive_torrent_file(&path, &self.base_dir, &info_hash);

        // Record in state.json with awaiting_file_selection = true
        self.state.torrents.insert(
            info_hash.clone(),
            TorrentMetadata {
                info_hash: info_hash.clone(),
                name: name.clone(),
                torrent_filename: format!("{}.torrent", info_hash),
                tags,
                selected_files: String::new(),
                max_download_limit: "0".to_string(),
                max_upload_limit: "0".to_string(),
                seed_ratio: self.state.global_limits.default_seed_ratio.clone(),
                seed_time: self.state.global_limits.default_seed_time.clone(),
                is_complete: false,
                awaiting_file_selection: true,
            },
        );
        let _ = self.state.save(&self.base_dir);

        // Fetch files for selection modal
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

            let _ = self.worker_tx.send(WorkerMessage::FilesLoaded {
                info_hash,
                gid: Some(gid),
                files: selection_items,
            }).await;
        }
    }

    async fn add_torrent_magnet(&mut self, magnet_str: &str, tags: Vec<String>) {
        let clean_magnet = strip_quotes(magnet_str);
        if !clean_magnet.starts_with("magnet:") {
            let _ = self.worker_tx.send(WorkerMessage::ShowMessage {
                title: "Invalid Magnet URI".to_string(),
                message: "URI must begin with 'magnet:'".to_string(),
                is_error: true,
            }).await;
            return;
        }

        let mut opts = serde_json::Map::new();
        let dir_clean = clean_path(&self.base_dir).to_string_lossy().to_string();
        opts.insert("dir".to_string(), serde_json::json!(dir_clean));
        opts.insert("bt-save-metadata".to_string(), serde_json::json!("true"));
        opts.insert("pause-metadata".to_string(), serde_json::json!("true"));
        opts.insert("file-allocation".to_string(), serde_json::json!("none"));

        let gid = match self.process_mgr.client.add_uri(vec![clean_magnet.clone()], serde_json::Value::Object(opts)).await {
            Ok(g) => g,
            Err(e) => {
                let _ = self.worker_tx.send(WorkerMessage::ShowMessage {
                    title: "Add Magnet Failed".to_string(),
                    message: format!("Failed to add magnet via RPC: {}", e),
                    is_error: true,
                }).await;
                return;
            }
        };

        // Track pending tags by GID
        self.pending_magnet_tags.insert(gid.clone(), tags);

        // Try extracting infoHash from magnet string if present
        if let Some(hash) = extract_hash_from_magnet(&clean_magnet) {
            let h = hash.to_lowercase();
            if let Some(t) = self.pending_magnet_tags.get(&gid).cloned() {
                self.pending_magnet_tags.insert(h, t);
            }
        }
    }

    async fn fetch_files_for_selection(&mut self, info_hash: &str) {
        let h = info_hash.to_lowercase();
        // Find live GID in aria2
        let mut target_gid: Option<String> = None;
        if let Ok(active) = self.process_mgr.client.tell_active().await {
            for item in active {
                if item.info_hash.as_deref().map(|s| s.to_lowercase()) == Some(h.clone()) {
                    target_gid = Some(item.gid);
                    break;
                }
            }
        }
        if target_gid.is_none() {
            if let Ok(waiting) = self.process_mgr.client.tell_waiting(0, 1000).await {
                for item in waiting {
                    if item.info_hash.as_deref().map(|s| s.to_lowercase()) == Some(h.clone()) {
                        target_gid = Some(item.gid);
                        break;
                    }
                }
            }
        }

        if let Some(gid) = target_gid {
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

                let _ = self.worker_tx.send(WorkerMessage::FilesLoaded {
                    info_hash: h,
                    gid: Some(gid),
                    files: selection_items,
                }).await;
                return;
            }
        }

        let _ = self.worker_tx.send(WorkerMessage::ShowMessage {
            title: "File Selection Unavailable".to_string(),
            message: "Torrent must be loaded in aria2c to inspect or modify file list. Press 's' to start/load it first.".to_string(),
            is_error: false,
        }).await;
    }

    async fn apply_file_selection(&mut self, info_hash: &str, selected_indices: &str) {
        let h = info_hash.to_lowercase();
        if let Some(entry) = self.state.torrents.get_mut(&h) {
            entry.selected_files = selected_indices.to_string();
            entry.awaiting_file_selection = false;
            let _ = self.state.save(&self.base_dir);
        }

        // If active/waiting in aria2, apply changeOption and unpause
        let mut target_gid: Option<String> = None;
        if let Ok(waiting) = self.process_mgr.client.tell_waiting(0, 1000).await {
            for item in waiting {
                if item.info_hash.as_deref().map(|s| s.to_lowercase()) == Some(h.clone()) {
                    target_gid = Some(item.gid);
                    break;
                }
            }
        }
        if target_gid.is_none() {
            if let Ok(active) = self.process_mgr.client.tell_active().await {
                for item in active {
                    if item.info_hash.as_deref().map(|s| s.to_lowercase()) == Some(h.clone()) {
                        target_gid = Some(item.gid);
                        break;
                    }
                }
            }
        }

        if let Some(gid) = target_gid {
            let mut opts = serde_json::Map::new();
            if !selected_indices.is_empty() {
                opts.insert("select-file".to_string(), serde_json::json!(selected_indices));
            }
            opts.insert("file-allocation".to_string(), serde_json::json!("none"));
            let _ = self.process_mgr.client.change_option(&gid, serde_json::Value::Object(opts)).await;
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

        // Apply live to aria2 if active
        let mut target_gid = None;
        if let Ok(active) = self.process_mgr.client.tell_active().await {
            for item in active {
                if item.info_hash.as_deref().map(|s| s.to_lowercase()) == Some(h.clone()) {
                    target_gid = Some(item.gid);
                    break;
                }
            }
        }
        if target_gid.is_none() {
            if let Ok(waiting) = self.process_mgr.client.tell_waiting(0, 1000).await {
                for item in waiting {
                    if item.info_hash.as_deref().map(|s| s.to_lowercase()) == Some(h.clone()) {
                        target_gid = Some(item.gid);
                        break;
                    }
                }
            }
        }

        if let Some(gid) = target_gid {
            let mut opts = serde_json::Map::new();
            opts.insert("max-download-limit".to_string(), serde_json::json!(max_dl));
            opts.insert("max-upload-limit".to_string(), serde_json::json!(max_ul));
            opts.insert("seed-ratio".to_string(), serde_json::json!(ratio));
            if time != "0" {
                opts.insert("seed-time".to_string(), serde_json::json!(time));
            }
            let _ = self.process_mgr.client.change_option(&gid, serde_json::Value::Object(opts)).await;
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
        opts.insert("max-overall-download-limit".to_string(), serde_json::json!(max_dl));
        opts.insert("max-overall-upload-limit".to_string(), serde_json::json!(max_ul));
        opts.insert("seed-ratio".to_string(), serde_json::json!(ratio));
        if time != "0" {
            opts.insert("seed-time".to_string(), serde_json::json!(time));
        }
        let _ = self.process_mgr.client.change_global_option(serde_json::Value::Object(opts)).await;
    }

    async fn delete_torrent(&mut self, info_hash: &str, delete_files: bool) {
        let h = info_hash.to_lowercase();
        // Remove from aria2 active/waiting/stopped
        let mut gids_to_remove = Vec::new();
        if let Ok(active) = self.process_mgr.client.tell_active().await {
            for item in active {
                if item.info_hash.as_deref().map(|s| s.to_lowercase()) == Some(h.clone()) || item.gid == h {
                    gids_to_remove.push((item.gid, true));
                }
            }
        }
        if let Ok(waiting) = self.process_mgr.client.tell_waiting(0, 1000).await {
            for item in waiting {
                if item.info_hash.as_deref().map(|s| s.to_lowercase()) == Some(h.clone()) || item.gid == h {
                    gids_to_remove.push((item.gid, true));
                }
            }
        }
        if let Ok(stopped) = self.process_mgr.client.tell_stopped(0, 1000).await {
            for item in stopped {
                if item.info_hash.as_deref().map(|s| s.to_lowercase()) == Some(h.clone()) || item.gid == h {
                    gids_to_remove.push((item.gid, false));
                }
            }
        }

        for (gid, active_or_waiting) in gids_to_remove {
            if active_or_waiting {
                let _ = self.process_mgr.client.force_remove(&gid).await;
            }
            let _ = self.process_mgr.client.remove_download_result(&gid).await;
        }

        // Delete payload files if requested
        if delete_files {
            if let Some(entry) = self.state.torrents.get(&h) {
                let payload_path = self.base_dir.join(&entry.name);
                if payload_path.exists() {
                    if payload_path.is_dir() {
                        let _ = std::fs::remove_dir_all(&payload_path);
                    } else if payload_path.is_file() {
                        let _ = std::fs::remove_file(&payload_path);
                    }
                }
            }
        }

        // Delete .torrent file from <DIR>/.torrent/
        let meta_dir = AppStateStorage::torrent_meta_dir(&self.base_dir);
        let torrent_file = meta_dir.join(format!("{}.torrent", h));
        if torrent_file.is_file() {
            let _ = std::fs::remove_file(&torrent_file);
        }

        // Remove from state.json
        self.state.torrents.remove(&h);
        let _ = self.state.save(&self.base_dir);
    }
}

pub fn clean_path_str(s: &str) -> String {
    let trimmed = s.trim();
    if let Some(rest) = trimmed.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{}", rest)
    } else if let Some(rest) = trimmed.strip_prefix(r"\??\UNC\") {
        format!(r"\\{}", rest)
    } else if let Some(rest) = trimmed.strip_prefix(r"\\?\") {
        rest.to_string()
    } else if let Some(rest) = trimmed.strip_prefix(r"\??\") {
        rest.to_string()
    } else {
        trimmed.to_string()
    }
}

pub fn clean_path<P: AsRef<Path>>(p: P) -> PathBuf {
    let s = p.as_ref().to_string_lossy();
    PathBuf::from(clean_path_str(&s))
}

pub fn strip_quotes(s: &str) -> String {
    let trimmed = s.trim();
    if (trimmed.starts_with('"') && trimmed.ends_with('"'))
        || (trimmed.starts_with('\'') && trimmed.ends_with('\''))
    {
        if trimmed.len() >= 2 {
            return trimmed[1..trimmed.len() - 1].trim().to_string();
        }
    }
    trimmed.to_string()
}

pub fn extract_hash_from_magnet(magnet: &str) -> Option<String> {
    if let Some(pos) = magnet.find("urn:btih:") {
        let rest = &magnet[pos + 9..];
        let hash = rest.split('&').next().unwrap_or(rest);
        if hash.len() == 40 && hash.chars().all(|c| c.is_ascii_hexdigit()) {
            return Some(hash.to_string());
        }
    }
    None
}

pub fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    const TB: u64 = GB * 1024;

    if bytes >= TB {
        format!("{:.2} TB", bytes as f64 / TB as f64)
    } else if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

pub fn format_speed(bytes_per_sec: u64) -> String {
    if bytes_per_sec == 0 {
        "0 B/s".to_string()
    } else {
        format!("{}/s", format_bytes(bytes_per_sec))
    }
}

pub fn format_eta(seconds: Option<u64>) -> String {
    match seconds {
        None => "∞".to_string(),
        Some(0) => "0s".to_string(),
        Some(s) => {
            let hours = s / 3600;
            let minutes = (s % 3600) / 60;
            let secs = s % 60;

            if hours > 0 {
                format!("{}h {:02}m", hours, minutes)
            } else if minutes > 0 {
                format!("{}m {:02}s", minutes, secs)
            } else {
                format!("{}s", secs)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_quotes() {
        assert_eq!(strip_quotes("\"hello world\""), "hello world");
        assert_eq!(strip_quotes("'hello world'"), "hello world");
        assert_eq!(strip_quotes("  \"C:\\Path With Spaces\\file.torrent\"  "), "C:\\Path With Spaces\\file.torrent");
        assert_eq!(strip_quotes("no_quotes.torrent"), "no_quotes.torrent");
        assert_eq!(strip_quotes("\"\""), "");
        assert_eq!(strip_quotes("''"), "");
    }

    #[test]
    fn test_clean_path_str() {
        assert_eq!(clean_path_str(r"\\?\C:\Users\mark\Downloads"), r"C:\Users\mark\Downloads");
        assert_eq!(clean_path_str(r"\\?\UNC\server\share\file.torrent"), r"\\server\share\file.torrent");
        assert_eq!(clean_path_str(r"\??\D:\torrents"), r"D:\torrents");
        assert_eq!(clean_path_str(r"\??\UNC\server\share"), r"\\server\share");
        assert_eq!(clean_path_str(r"K:\_my\code\grav-aria2-tui"), r"K:\_my\code\grav-aria2-tui");
        assert_eq!(clean_path_str("/tmp/torrents"), "/tmp/torrents");
    }

    #[test]
    fn test_extract_hash_from_magnet() {
        let uri = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&dn=Ubuntu";
        assert_eq!(
            extract_hash_from_magnet(uri),
            Some("0123456789abcdef0123456789abcdef01234567".to_string())
        );

        let uri2 = "magnet:?dn=Test&xt=urn:btih:0123456789ABCDEF0123456789ABCDEF01234567&tr=http://tracker";
        assert_eq!(
            extract_hash_from_magnet(uri2),
            Some("0123456789ABCDEF0123456789ABCDEF01234567".to_string())
        );

        let invalid = "https://example.com/file.torrent";
        assert_eq!(extract_hash_from_magnet(invalid), None);
    }

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1024), "1.00 KB");
        assert_eq!(format_bytes(1048576), "1.00 MB");
        assert_eq!(format_bytes(1073741824), "1.00 GB");
        assert_eq!(format_bytes(1099511627776), "1.00 TB");
    }

    #[test]
    fn test_format_speed() {
        assert_eq!(format_speed(0), "0 B/s");
        assert_eq!(format_speed(1048576), "1.00 MB/s");
    }

    #[test]
    fn test_format_eta() {
        assert_eq!(format_eta(None), "∞");
        assert_eq!(format_eta(Some(0)), "0s");
        assert_eq!(format_eta(Some(45)), "45s");
        assert_eq!(format_eta(Some(125)), "2m 05s");
        assert_eq!(format_eta(Some(3665)), "1h 01m");
    }

    #[test]
    fn test_torrent_item_calculations() {
        let item = TorrentItem {
            info_hash: "hash1".to_string(),
            gid: Some("1".to_string()),
            name: "Test.iso".to_string(),
            tags: vec!["iso".to_string()],
            size_bytes: 1_000_000,
            completed_bytes: 500_000,
            uploaded_bytes: 1_000_000,
            download_speed: 100_000,
            upload_speed: 50_000,
            status: TorrentStatus::Downloading,
            selected_files: String::new(),
            max_download_limit: "0".to_string(),
            max_upload_limit: "0".to_string(),
            seed_ratio: "1.0".to_string(),
            seed_time: "0".to_string(),
            is_complete: false,
            awaiting_file_selection: false,
            error_message: None,
            num_seeders: Some("10".to_string()),
            connections: Some("5".to_string()),
        };

        assert_eq!(item.progress_pct(), 50.0);
        assert_eq!(item.ratio(), 2.0);
        assert_eq!(item.eta_seconds(), Some(5)); // (1000000 - 500000) / 100000 = 5s
    }

    #[test]
    fn test_tag_filtering() {
        let (action_tx, _action_rx) = tokio::sync::mpsc::channel(10);
        let (_worker_tx, worker_rx) = tokio::sync::mpsc::channel(10);
        let mut app = App::new(PathBuf::from("."), false, action_tx, worker_rx);

        app.items = vec![
            TorrentItem {
                info_hash: "1".to_string(),
                gid: None,
                name: "Linux A".to_string(),
                tags: vec!["linux".to_string(), "distro".to_string()],
                size_bytes: 100,
                completed_bytes: 100,
                uploaded_bytes: 0,
                download_speed: 0,
                upload_speed: 0,
                status: TorrentStatus::Seeding,
                selected_files: String::new(),
                max_download_limit: "0".to_string(),
                max_upload_limit: "0".to_string(),
                seed_ratio: "1.0".to_string(),
                seed_time: "0".to_string(),
                is_complete: true,
                awaiting_file_selection: false,
                error_message: None,
                num_seeders: None,
                connections: None,
            },
            TorrentItem {
                info_hash: "2".to_string(),
                gid: None,
                name: "Game B".to_string(),
                tags: vec!["games".to_string()],
                size_bytes: 200,
                completed_bytes: 50,
                uploaded_bytes: 0,
                download_speed: 10,
                upload_speed: 0,
                status: TorrentStatus::Downloading,
                selected_files: String::new(),
                max_download_limit: "0".to_string(),
                max_upload_limit: "0".to_string(),
                seed_ratio: "1.0".to_string(),
                seed_time: "0".to_string(),
                is_complete: false,
                awaiting_file_selection: false,
                error_message: None,
                num_seeders: None,
                connections: None,
            },
        ];

        let tags = app.available_tags();
        assert_eq!(tags, vec!["All", "distro", "games", "linux"]);

        // When tag_filter_index is 0 ("All")
        assert_eq!(app.filtered_items().len(), 2);

        // Switch to "distro" (index 1)
        app.next_tag_filter();
        assert_eq!(app.filtered_items().len(), 1);
        assert_eq!(app.filtered_items()[0].name, "Linux A");

        // Switch to "games" (index 2)
        app.next_tag_filter();
        assert_eq!(app.filtered_items().len(), 1);
        assert_eq!(app.filtered_items()[0].name, "Game B");
    }
}

