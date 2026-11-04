use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

fn default_zero() -> String {
    "0".to_string()
}

fn default_seed_ratio() -> String {
    "1.0".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalLimitsConfig {
    #[serde(default = "default_zero")]
    pub max_overall_download_limit: String,
    #[serde(default = "default_zero")]
    pub max_overall_upload_limit: String,
    #[serde(default = "default_seed_ratio")]
    pub default_seed_ratio: String,
    #[serde(default = "default_zero")]
    pub default_seed_time: String,
}

impl Default for GlobalLimitsConfig {
    fn default() -> Self {
        Self {
            max_overall_download_limit: "0".to_string(),
            max_overall_upload_limit: "0".to_string(),
            default_seed_ratio: "1.0".to_string(),
            default_seed_time: "0".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TorrentMetadata {
    pub info_hash: String,
    pub name: String,
    #[serde(default)]
    pub torrent_filename: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub selected_files: String,
    #[serde(default = "default_zero")]
    pub max_download_limit: String,
    #[serde(default = "default_zero")]
    pub max_upload_limit: String,
    #[serde(default = "default_seed_ratio")]
    pub seed_ratio: String,
    #[serde(default = "default_zero")]
    pub seed_time: String,
    #[serde(default)]
    pub is_complete: bool,
    #[serde(default)]
    pub awaiting_file_selection: bool,
}

impl TorrentMetadata {
    /// Creates a new entry with sane defaults derived from the global configuration.
    pub fn new(
        info_hash: impl Into<String>,
        name: impl Into<String>,
        tags: Vec<String>,
        global: &GlobalLimitsConfig,
    ) -> Self {
        let hash = info_hash.into();
        Self {
            torrent_filename: format!("{}.torrent", hash),
            info_hash: hash,
            name: name.into(),
            tags,
            selected_files: String::new(),
            max_download_limit: "0".into(),
            max_upload_limit: "0".into(),
            seed_ratio: global.default_seed_ratio.clone(),
            seed_time: global.default_seed_time.clone(),
            is_complete: false,
            awaiting_file_selection: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppStateStorage {
    #[serde(default)]
    pub global_limits: GlobalLimitsConfig,
    #[serde(default)]
    pub torrents: HashMap<String, TorrentMetadata>,
}

#[derive(Debug, Clone)]
pub struct NewTorrentCandidate {
    pub info_hash: String,
    pub name: String,
    #[allow(dead_code)]
    pub file_path: PathBuf,
    pub selected: bool,
}

#[derive(Debug, Default)]
pub struct StartupScanResult {
    #[allow(dead_code)]
    pub pruned_torrents: Vec<(String, String)>,
    pub new_untracked: Vec<NewTorrentCandidate>,
}

/// Checks if payload files or directory for a torrent exist in base_dir
pub fn check_torrent_payload_exists(base_dir: &Path, name: &str, info_hash: &str) -> bool {
    // 1. Check using entry name
    if !name.is_empty() && !name.starts_with("[Magnet]") {
        let p = base_dir.join(name);
        if p.exists() {
            return true;
        }
    }

    // 2. Check using bencoded name from .torrent file if available
    let meta_dir = AppStateStorage::torrent_meta_dir(base_dir);
    let torrent_file = meta_dir.join(format!("{}.torrent", info_hash.to_lowercase()));
    if torrent_file.is_file() {
        if let Ok(bytes) = fs::read(&torrent_file) {
            if let Some((_, bencode_name)) = parse_torrent_info(&bytes) {
                if !bencode_name.is_empty() {
                    let p = base_dir.join(&bencode_name);
                    if p.exists() {
                        return true;
                    }
                }
            }
        }
    }

    false
}

impl AppStateStorage {
    pub fn state_file_path(base_dir: &Path) -> PathBuf {
        base_dir.join(".torrent").join("state.json")
    }

    pub fn torrent_meta_dir(base_dir: &Path) -> PathBuf {
        base_dir.join(".torrent")
    }

    pub fn load(base_dir: &Path) -> Result<Self> {
        let meta_dir = Self::torrent_meta_dir(base_dir);
        if !meta_dir.exists() {
            fs::create_dir_all(&meta_dir)
                .with_context(|| format!("Failed to create .torrent metadata dir {:?}", meta_dir))?;
        }

        let path = Self::state_file_path(base_dir);
        if !path.exists() {
            let state = AppStateStorage::default();
            let _ = state.save(base_dir);
            return Ok(state);
        }

        let data = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read state file {:?}", path))?;
        let state: AppStateStorage = serde_json::from_str(&data)
            .with_context(|| format!("Failed to parse state file {:?}", path))?;

        Ok(state)
    }

    pub fn save(&self, base_dir: &Path) -> Result<()> {
        let meta_dir = Self::torrent_meta_dir(base_dir);
        if !meta_dir.exists() {
            fs::create_dir_all(&meta_dir)
                .with_context(|| format!("Failed to create .torrent metadata dir {:?}", meta_dir))?;
        }

        let path = Self::state_file_path(base_dir);
        let tmp_path = base_dir.join(".torrent").join("state.json.tmp");

        let json = serde_json::to_string_pretty(self)
            .context("Failed to serialize app state to JSON")?;

        fs::write(&tmp_path, json)
            .with_context(|| format!("Failed to write temporary state file {:?}", tmp_path))?;

        if let Err(e) = fs::rename(&tmp_path, &path) {
            // On Windows rename fails if target exists in some cases, so fallback to copy/remove
            let _ = fs::copy(&tmp_path, &path);
            let _ = fs::remove_file(&tmp_path);
            log::debug!("Used fallback save for state.json: {:?}", e);
        }

        Ok(())
    }

    /// Synchronizes on startup:
    /// 1. If torrent exists in state & .torrent exists, but no payload files detected: delete .torrent file and remove from state.
    /// 2. If new .torrent files exist in .torrent/ that are not in state: returns them as untracked candidates to prompt the user.
    pub fn sync_on_startup(&mut self, base_dir: &Path) -> Result<StartupScanResult> {
        let mut pruned = Vec::new();
        let mut to_remove = Vec::new();
        let meta_dir = Self::torrent_meta_dir(base_dir);

        // 1. Scan existing torrents in state
        for (hash, entry) in &self.torrents {
            let torrent_file = meta_dir.join(format!("{}.torrent", hash.to_lowercase()));
            let torrent_exists = torrent_file.is_file();
            let payload_exists = check_torrent_payload_exists(base_dir, &entry.name, hash);

            if torrent_exists && !payload_exists {
                // Payload deleted by user -> remove .torrent file and state
                let _ = fs::remove_file(&torrent_file);
                let aria2_file = base_dir.join(format!("{}.aria2", entry.name));
                if aria2_file.is_file() {
                    let _ = fs::remove_file(&aria2_file);
                }
                to_remove.push(hash.clone());
                pruned.push((hash.clone(), entry.name.clone()));
            } else if !torrent_exists && !payload_exists {
                // Both missing -> remove from state
                to_remove.push(hash.clone());
                pruned.push((hash.clone(), entry.name.clone()));
            }
        }

        for hash in to_remove {
            self.torrents.remove(&hash);
        }

        if !pruned.is_empty() {
            let _ = self.save(base_dir);
        }

        // 2. Scan .torrent folder for untracked torrent files
        let mut new_untracked = Vec::new();
        if let Ok(entries) = fs::read_dir(&meta_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    if let Some(ext) = path.extension() {
                        if ext.to_string_lossy().eq_ignore_ascii_case("torrent") {
                            if let Ok(bytes) = fs::read(&path) {
                                if let Some((info_hash, name)) = parse_torrent_info(&bytes) {
                                    let key = info_hash.to_lowercase();
                                    if !self.torrents.contains_key(&key) {
                                        let display_name = if name.is_empty() {
                                            path.file_stem()
                                                .map(|s| s.to_string_lossy().to_string())
                                                .unwrap_or_else(|| key.clone())
                                        } else {
                                            name
                                        };
                                        new_untracked.push(NewTorrentCandidate {
                                            info_hash: key,
                                            name: display_name,
                                            file_path: path.clone(),
                                            selected: true,
                                        });
                                    }
                                } else if let Some(stem) = path.file_stem() {
                                    let stem_str = stem.to_string_lossy().to_lowercase();
                                    if stem_str.len() == 40 && stem_str.chars().all(|c| c.is_ascii_hexdigit()) {
                                        if !self.torrents.contains_key(&stem_str) {
                                            new_untracked.push(NewTorrentCandidate {
                                                info_hash: stem_str.clone(),
                                                name: stem_str.clone(),
                                                file_path: path.clone(),
                                                selected: true,
                                            });
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(StartupScanResult {
            pruned_torrents: pruned,
            new_untracked,
        })
    }


}

/// Copies a .torrent file into <DIR>/.torrent/<info_hash>.torrent
pub fn archive_torrent_file(source: &Path, base_dir: &Path, info_hash: &str) -> Result<PathBuf> {
    let dest_dir = AppStateStorage::torrent_meta_dir(base_dir);
    if !dest_dir.exists() {
        fs::create_dir_all(&dest_dir)?;
    }
    let dest_filename = format!("{}.torrent", info_hash.to_lowercase());
    let dest_path = dest_dir.join(dest_filename);

    if source != dest_path {
        fs::copy(source, &dest_path)
            .with_context(|| format!("Failed to copy torrent file {:?} to {:?}", source, dest_path))?;
    }
    Ok(dest_path)
}

/// Detects if aria2 saved <info_hash>.torrent directly in <DIR> and moves it to <DIR>/.torrent/<info_hash>.torrent
pub fn detect_and_move_saved_metadata(base_dir: &Path, info_hash: &str) -> Option<PathBuf> {
    let hash_lower = info_hash.to_lowercase();
    let src_filename = format!("{}.torrent", hash_lower);
    let src_path = base_dir.join(&src_filename);

    if src_path.is_file() {
        let dest_dir = AppStateStorage::torrent_meta_dir(base_dir);
        let _ = fs::create_dir_all(&dest_dir);
        let dest_path = dest_dir.join(&src_filename);

        if src_path != dest_path {
            if fs::rename(&src_path, &dest_path).is_ok()
                || (fs::copy(&src_path, &dest_path).is_ok() && fs::remove_file(&src_path).is_ok())
            {
                return Some(dest_path);
            }
        } else {
            return Some(dest_path);
        }
    }
    None
}

// Minimal, safe bencode parser to extract the info_hash and name from .torrent files
pub fn parse_torrent_info(bytes: &[u8]) -> Option<(String, String)> {
    let mut parser = BencodeParser::new(bytes);
    parser.parse_torrent()
}

struct BencodeParser<'a> {
    bytes: &'a [u8],
    idx: usize,
}

impl<'a> BencodeParser<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, idx: 0 }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.idx).copied()
    }

    fn advance(&mut self) -> Option<u8> {
        let b = self.bytes.get(self.idx).copied()?;
        self.idx += 1;
        Some(b)
    }

    // Skips a complete bencoded value and returns its slice [start..end]
    fn skip_value(&mut self) -> Option<&'a [u8]> {
        let start = self.idx;
        match self.peek()? {
            b'i' => {
                self.advance();
                while let Some(b) = self.advance() {
                    if b == b'e' {
                        return Some(&self.bytes[start..self.idx]);
                    }
                }
                None
            }
            b'l' => {
                self.advance();
                while self.peek()? != b'e' {
                    self.skip_value()?;
                }
                self.advance(); // consume 'e'
                Some(&self.bytes[start..self.idx])
            }
            b'd' => {
                self.advance();
                while self.peek()? != b'e' {
                    self.skip_value()?; // key
                    self.skip_value()?; // value
                }
                self.advance(); // consume 'e'
                Some(&self.bytes[start..self.idx])
            }
            b'0'..=b'9' => {
                let len = self.read_length()?;
                let end = self.idx.checked_add(len)?;
                if end <= self.bytes.len() {
                    self.idx = end;
                    Some(&self.bytes[start..self.idx])
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    fn read_length(&mut self) -> Option<usize> {
        let mut len_str = String::new();
        while let Some(b) = self.peek() {
            if b.is_ascii_digit() {
                len_str.push(b as char);
                self.advance();
            } else if b == b':' {
                self.advance();
                break;
            } else {
                return None;
            }
        }
        len_str.parse::<usize>().ok()
    }

    fn read_byte_string(&mut self) -> Option<&'a [u8]> {
        let len = self.read_length()?;
        let start = self.idx;
        let end = self.idx.checked_add(len)?;
        if end <= self.bytes.len() {
            self.idx = end;
            Some(&self.bytes[start..end])
        } else {
            None
        }
    }

    pub fn parse_torrent(&mut self) -> Option<(String, String)> {
        // Must start with 'd'
        if self.advance()? != b'd' {
            return None;
        }

        let mut info_bytes: Option<&'a [u8]> = None;
        let mut name: Option<String> = None;

        while let Some(ch) = self.peek() {
            if ch == b'e' {
                break;
            }
            let key = self.read_byte_string()?;
            if key == b"info" {
                let start = self.idx;
                let slice = self.skip_value()?;
                info_bytes = Some(slice);

                // Parse inside info dictionary to find name
                let mut info_parser = BencodeParser::new(&self.bytes[start..start + slice.len()]);
                if info_parser.advance() == Some(b'd') {
                    while let Some(b) = info_parser.peek() {
                        if b == b'e' {
                            break;
                        }
                        if let Some(k) = info_parser.read_byte_string() {
                            if k == b"name.utf-8" {
                                if let Some(v) = info_parser.read_byte_string() {
                                    name = Some(String::from_utf8_lossy(v).to_string());
                                }
                            } else if k == b"name" && name.is_none() {
                                if let Some(v) = info_parser.read_byte_string() {
                                    name = Some(String::from_utf8_lossy(v).to_string());
                                }
                            } else {
                                info_parser.skip_value();
                            }
                        } else {
                            break;
                        }
                    }
                }
            } else {
                self.skip_value()?;
            }
        }

        let info = info_bytes?;
        let mut hasher = Sha1::new();
        hasher.update(info);
        let hash = hasher.finalize();
        let info_hash = hex::encode(hash);

        Some((info_hash, name.unwrap_or_default()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bencode_parser() {
        // Simple torrent bencode structure: d4:infod4:name4:test12:piece lengthi16384ee4:nodee
        let raw = b"d4:infod4:name4:test12:piece lengthi16384eee";
        let res = parse_torrent_info(raw);
        assert!(res.is_some());
        let (hash, name) = res.unwrap();
        assert_eq!(name, "test");
        assert_eq!(hash.len(), 40);
    }

    #[test]
    fn test_state_load_save_and_scan() {
        let temp_dir = std::env::temp_dir().join(format!("grav_aria2_test_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let _ = fs::create_dir_all(&temp_dir);

        // Load initially
        let mut state = AppStateStorage::load(&temp_dir).unwrap();
        assert_eq!(state.global_limits.default_seed_ratio, "1.0");
        assert_eq!(state.global_limits.default_seed_time, "0");
        assert!(state.torrents.is_empty());

        // Modify limits and add a torrent
        state.global_limits.max_overall_download_limit = "5M".to_string();
        let mut meta = TorrentMetadata::new(
            "aabbccddeeff00112233445566778899aabbccdd",
            "ArchLinux",
            vec!["linux".to_string(), "iso".to_string()],
            &state.global_limits,
        );
        meta.selected_files = "1,2".to_string();
        meta.seed_ratio = "2.0".to_string();
        meta.seed_time = "60".to_string();
        meta.is_complete = true;
        state.torrents.insert(meta.info_hash.clone(), meta);

        state.save(&temp_dir).unwrap();

        // Reload state
        let reloaded = AppStateStorage::load(&temp_dir).unwrap();
        assert_eq!(reloaded.global_limits.max_overall_download_limit, "5M");
        let item = reloaded.torrents.get("aabbccddeeff00112233445566778899aabbccdd").unwrap();
        assert_eq!(item.name, "ArchLinux");
        assert_eq!(item.tags, vec!["linux", "iso"]);
        assert_eq!(item.seed_ratio, "2.0");
        assert!(item.is_complete);

        // Clean up
        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_check_torrent_payload_exists() {
        let temp_dir = std::env::temp_dir().join(format!("grav_payload_test_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let _ = fs::create_dir_all(&temp_dir);

        let hash = "1122334455667788990011223344556677889900";
        assert!(!check_torrent_payload_exists(&temp_dir, "ubuntu.iso", hash));

        // Create payload file
        let file_path = temp_dir.join("ubuntu.iso");
        let _ = fs::write(&file_path, b"dummy iso payload");
        assert!(check_torrent_payload_exists(&temp_dir, "ubuntu.iso", hash));

        // Delete payload file
        let _ = fs::remove_file(&file_path);
        assert!(!check_torrent_payload_exists(&temp_dir, "ubuntu.iso", hash));

        // Create multi-file directory payload
        let dir_path = temp_dir.join("MyFolder");
        let _ = fs::create_dir_all(&dir_path);
        assert!(check_torrent_payload_exists(&temp_dir, "MyFolder", hash));

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_startup_sync_pruning_and_untracked() {
        let temp_dir = std::env::temp_dir().join(format!("grav_sync_test_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let _ = fs::create_dir_all(&temp_dir);
        let meta_dir = AppStateStorage::torrent_meta_dir(&temp_dir);
        let _ = fs::create_dir_all(&meta_dir);

        // Raw bencoded torrent for "test" torrent
        let raw_torrent = b"d4:infod4:name4:test12:piece lengthi16384eee";
        let (info_hash, name) = parse_torrent_info(raw_torrent).unwrap();

        // 1. Create a torrent file in .torrent/
        let torrent_file = meta_dir.join(format!("{}.torrent", info_hash));
        let _ = fs::write(&torrent_file, raw_torrent);

        // Load empty state
        let mut state = AppStateStorage::load(&temp_dir).unwrap();

        // Initially, .torrent file exists in folder but not in state -> should be detected as untracked
        let sync1 = state.sync_on_startup(&temp_dir).unwrap();
        assert_eq!(sync1.new_untracked.len(), 1);
        assert_eq!(sync1.new_untracked[0].info_hash, info_hash);
        assert_eq!(sync1.new_untracked[0].name, name);
        assert!(sync1.pruned_torrents.is_empty());

        // Now add it to state (simulating it was downloaded), but do NOT create payload file on disk
        let mut meta = TorrentMetadata::new(info_hash.clone(), name.clone(), vec![], &state.global_limits);
        meta.is_complete = true;
        state.torrents.insert(info_hash.clone(), meta);
        state.save(&temp_dir).unwrap();

        // 2. Next sync_on_startup: torrent is in state, .torrent file exists, but NO payload files exist!
        // Should prune .torrent file and remove from state!
        let sync2 = state.sync_on_startup(&temp_dir).unwrap();
        assert_eq!(sync2.pruned_torrents.len(), 1);
        assert_eq!(sync2.pruned_torrents[0].0, info_hash);
        assert!(!torrent_file.exists()); // .torrent file was deleted!
        assert!(!state.torrents.contains_key(&info_hash)); // Removed from state!

        let _ = fs::remove_dir_all(&temp_dir);
    }
}

