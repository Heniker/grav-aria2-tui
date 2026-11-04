use std::path::{Path, PathBuf};

/// Strips Windows extended-length path prefixes (\\?\, \??\, UNC variants).
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

/// Cleans a path by stripping Windows extended-length path prefixes.
pub fn clean_path<P: AsRef<Path>>(p: P) -> PathBuf {
    let s = p.as_ref().to_string_lossy();
    PathBuf::from(clean_path_str(&s))
}

/// Removes surrounding single or double quotes from a string.
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

/// Extracts a 40-hex-char BitTorrent info-hash from a magnet URI.
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

/// Formats a byte count as a human-readable string (B / KB / MB / GB / TB).
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

/// Formats a bytes-per-second speed as a human-readable string.
pub fn format_speed(bytes_per_sec: u64) -> String {
    if bytes_per_sec == 0 {
        "0 B/s".to_string()
    } else {
        format!("{}/s", format_bytes(bytes_per_sec))
    }
}

/// Formats an optional ETA in seconds as a human-readable string.
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
        assert_eq!(
            strip_quotes("  \"C:\\Path With Spaces\\file.torrent\"  "),
            "C:\\Path With Spaces\\file.torrent"
        );
        assert_eq!(strip_quotes("no_quotes.torrent"), "no_quotes.torrent");
        assert_eq!(strip_quotes("\"\""), "");
        assert_eq!(strip_quotes("''"), "");
    }

    #[test]
    fn test_clean_path_str() {
        assert_eq!(
            clean_path_str(r"\\?\C:\Users\mark\Downloads"),
            r"C:\Users\mark\Downloads"
        );
        assert_eq!(
            clean_path_str(r"\\?\UNC\server\share\file.torrent"),
            r"\\server\share\file.torrent"
        );
        assert_eq!(clean_path_str(r"\??\D:\torrents"), r"D:\torrents");
        assert_eq!(clean_path_str(r"\??\UNC\server\share"), r"\\server\share");
        assert_eq!(
            clean_path_str(r"K:\_my\code\grav-aria2-tui"),
            r"K:\_my\code\grav-aria2-tui"
        );
        assert_eq!(clean_path_str("/tmp/torrents"), "/tmp/torrents");
    }

    #[test]
    fn test_extract_hash_from_magnet() {
        let uri = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&dn=Ubuntu";
        assert_eq!(
            extract_hash_from_magnet(uri),
            Some("0123456789abcdef0123456789abcdef01234567".to_string())
        );

        let uri2 =
            "magnet:?dn=Test&xt=urn:btih:0123456789ABCDEF0123456789ABCDEF01234567&tr=http://tracker";
        assert_eq!(
            extract_hash_from_magnet(uri2),
            Some("0123456789ABCDEF0123456789ABCDEF01234567".to_string())
        );

        assert_eq!(extract_hash_from_magnet("https://example.com/file.torrent"), None);
    }

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1024), "1.00 KB");
        assert_eq!(format_bytes(1_048_576), "1.00 MB");
        assert_eq!(format_bytes(1_073_741_824), "1.00 GB");
        assert_eq!(format_bytes(1_099_511_627_776), "1.00 TB");
    }

    #[test]
    fn test_format_speed() {
        assert_eq!(format_speed(0), "0 B/s");
        assert_eq!(format_speed(1_048_576), "1.00 MB/s");
    }

    #[test]
    fn test_format_eta() {
        assert_eq!(format_eta(None), "∞");
        assert_eq!(format_eta(Some(0)), "0s");
        assert_eq!(format_eta(Some(45)), "45s");
        assert_eq!(format_eta(Some(125)), "2m 05s");
        assert_eq!(format_eta(Some(3665)), "1h 01m");
    }
}
