use std::collections::BTreeSet;
use std::path::PathBuf;
use tokio::sync::mpsc::{Receiver, Sender};

use crate::rpc::GlobalStat;

// ---------------------------------------------------------------------------
// TorrentStatus
// ---------------------------------------------------------------------------

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
    /// Maps an aria2 status string + context flags to a `TorrentStatus`.
    pub fn from_aria2(
        status: &str,
        is_complete: bool,
        is_seeder: bool,
        awaiting_files: bool,
    ) -> Self {
        if awaiting_files {
            Self::SelectFiles
        } else if status == "error" {
            Self::Error
        } else if status == "paused" {
            Self::Paused
        } else if status == "active" {
            if is_complete || is_seeder {
                Self::Seeding
            } else {
                Self::Downloading
            }
        } else {
            // "complete", "waiting", or anything else
            Self::StoppedSaved
        }
    }
}

// ---------------------------------------------------------------------------
// TorrentItem
// ---------------------------------------------------------------------------

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
            if self.is_complete { 100.0 } else { 0.0 }
        } else {
            ((self.completed_bytes as f64 / self.size_bytes as f64) * 100.0).clamp(0.0, 100.0)
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
        if denom > 0.0 { self.uploaded_bytes as f64 / denom } else { 0.0 }
    }

    pub fn eta_seconds(&self) -> Option<u64> {
        if self.download_speed > 0 && self.size_bytes > self.completed_bytes {
            Some((self.size_bytes - self.completed_bytes) / self.download_speed)
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// FileSelectionItem
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct FileSelectionItem {
    pub index: String,
    pub path: String,
    pub length: u64,
    pub selected: bool,
}

// ---------------------------------------------------------------------------
// Modal
// ---------------------------------------------------------------------------

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
        delete_files_from_disk: bool,
    },
    NewTorrentsFound {
        torrents: Vec<crate::state::NewTorrentCandidate>,
        selected_index: usize,
    },
    Message {
        title: String,
        message: String,
        is_error: bool,
    },
}

// ---------------------------------------------------------------------------
// Action (UI → Worker)
// ---------------------------------------------------------------------------

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
    ImportUntrackedTorrents {
        torrents: Vec<crate::state::NewTorrentCandidate>,
        start_download: bool,
    },
    Quit,
}

// ---------------------------------------------------------------------------
// WorkerMessage (Worker → UI)
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// App – UI state
// ---------------------------------------------------------------------------

pub struct App {
    pub dir: PathBuf,
    pub items: Vec<TorrentItem>,
    pub selected_index: usize,
    pub tag_filter_index: usize,
    /// Sorted, deduplicated tag list; always starts with "All". Updated on every SyncUpdate.
    tag_cache: Vec<String>,
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
        action_tx: Sender<Action>,
        worker_rx: Receiver<WorkerMessage>,
    ) -> Self {
        Self {
            dir,
            items: Vec::new(),
            selected_index: 0,
            tag_filter_index: 0,
            tag_cache: vec!["All".to_string()],
            global_stat: GlobalStat::default(),
            global_dl_limit: "0".to_string(),
            global_ul_limit: "0".to_string(),
            modal: Modal::None,
            should_quit: false,
            action_tx,
            worker_rx,
        }
    }

    /// Rebuilds the sorted tag cache from the current item list.
    fn rebuild_tag_cache(&mut self) {
        let mut set = BTreeSet::new();
        for item in &self.items {
            for tag in &item.tags {
                if !tag.is_empty() {
                    set.insert(tag.clone());
                }
            }
        }
        let mut tags = vec!["All".to_string()];
        tags.extend(set);
        self.tag_cache = tags;
    }

    /// Returns all available tag filters, starting with "All".
    pub fn available_tags(&self) -> &[String] {
        &self.tag_cache
    }

    /// Returns the currently-active tag filter, or `None` if showing all.
    pub fn active_tag_filter(&self) -> Option<String> {
        if self.tag_filter_index == 0 || self.tag_filter_index >= self.tag_cache.len() {
            None
        } else {
            Some(self.tag_cache[self.tag_filter_index].clone())
        }
    }

    /// Returns the subset of torrents matching the active tag filter.
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
            Some(filtered[self.selected_index.min(filtered.len() - 1)])
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
        let len = self.tag_cache.len();
        if len > 0 {
            self.tag_filter_index = (self.tag_filter_index + 1) % len;
            self.selected_index = 0;
        }
    }

    pub fn prev_tag_filter(&mut self) {
        let len = self.tag_cache.len();
        if len > 0 {
            self.tag_filter_index = if self.tag_filter_index == 0 {
                len - 1
            } else {
                self.tag_filter_index - 1
            };
            self.selected_index = 0;
        }
    }

    /// Applies a single message received from the worker to the UI state.
    pub fn apply_worker_message(&mut self, msg: WorkerMessage) {
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
                self.rebuild_tag_cache();

                // Clamp selection if the filtered list shrank
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
                // Only auto-open if no other modal is currently shown
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

    /// Drains all pending messages from the worker channel and applies them to UI state.
    pub fn process_worker_messages(&mut self) {
        while let Ok(msg) = self.worker_rx.try_recv() {
            self.apply_worker_message(msg);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_torrent_status_from_aria2() {
        assert_eq!(
            TorrentStatus::from_aria2("active", false, false, false),
            TorrentStatus::Downloading
        );
        assert_eq!(
            TorrentStatus::from_aria2("active", true, false, false),
            TorrentStatus::Seeding
        );
        assert_eq!(
            TorrentStatus::from_aria2("active", false, true, false),
            TorrentStatus::Seeding
        );
        assert_eq!(
            TorrentStatus::from_aria2("paused", false, false, false),
            TorrentStatus::Paused
        );
        assert_eq!(
            TorrentStatus::from_aria2("error", false, false, false),
            TorrentStatus::Error
        );
        assert_eq!(
            TorrentStatus::from_aria2("complete", false, false, false),
            TorrentStatus::StoppedSaved
        );
        assert_eq!(
            TorrentStatus::from_aria2("active", false, false, true),
            TorrentStatus::SelectFiles
        );
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
        assert_eq!(item.eta_seconds(), Some(5));
    }

    #[test]
    fn test_tag_filtering() {
        let (action_tx, _action_rx) = tokio::sync::mpsc::channel(10);
        let (_worker_tx, worker_rx) = tokio::sync::mpsc::channel(10);
        let mut app = App::new(PathBuf::from("."), action_tx, worker_rx);

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
        app.rebuild_tag_cache();

        let tags = app.available_tags();
        assert_eq!(tags, vec!["All", "distro", "games", "linux"]);

        // "All" view: both items visible
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
