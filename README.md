Written by agent in a single prompt.
tldr: TUI torrent manager for my needs.

# grav-aria2-tui

A fast, production-ready, minimalist Terminal User Interface (TUI) for managing BitTorrent downloads and seeding via `aria2c`. Written in Rust using `ratatui`, `tokio`, and `crossterm`.

Cross-platform support for both **Windows** and **Unix** (Linux, macOS).

---

## Features

- **Torrents Only:** Exclusively manages BitTorrent downloads (`.torrent` files and `magnet:` URIs).
- **Minimalist, High-Performance UI:** Keyboard-driven 3-zone layout with zero lag. All RPC queries and heavy operations run in non-blocking Tokio async background workers.
- **Self-Contained Persistence:** Metadata, tags, file selection, and per-torrent options are automatically stored in `<DIR>/.torrent/state.json`.
- **Automatic Lifecycle Management:** Connects to an existing `aria2c` instance if already running, or automatically spawns a headless background `aria2c` daemon and terminates it cleanly on exit.
- **Interactive File Selection:** Select exactly which files to download before data transfer begins (`Space` to toggle, `a` for all, `n` for none).
- **Tagging & Batch Filtering:** Categorize torrents with custom tags (`linux`, `games`, `movies`). Cycle through tags with `Tab` / `Shift+Tab` and perform batch operations (`S` to batch start, `P` to batch pause).
- **Speed, Ratio & Seeding Controls:** Configure per-torrent and global download/upload limits, seed ratio targets, and seed time.
- **Integrity Recheck & Fast Seeding:** Quick-verify integrity on demand (`v`), or fast-resume verified completed torrents without redundant hashing (`bt-seed-unverified`).

---

## Installation & Build

### Prerequisites
- [Rust](https://rustup.rs/) (1.75+ recommended)
- `aria2c` installed and accessible on your `PATH` (or specified via `--aria2-bin`)

### Building from Source
```bash
git clone https://github.com/your-username/grav-aria2-tui.git
cd grav-aria2-tui
cargo build --release
```

The compiled binary will be located at `target/release/grav-aria2-tui` (or `grav-aria2-tui.exe` on Windows).

---

## CLI Usage

```text
grav-aria2-tui [OPTIONS] --dir <PATH>
```

### Options
- `-d, --dir <PATH>`: **(Required)** Root directory for payload downloads and `.torrent/` metadata storage.
- `--rpc-url <URL>`: JSON-RPC endpoint URL *(default: `http://127.0.0.1:6800/jsonrpc`)*.
- `--rpc-secret <TOKEN>`: Optional RPC secret authorization token.
- `--aria2-bin <PATH>`: Path to the `aria2c` executable *(default: `aria2c`)*.
- `-h, --help`: Show help.
- `-V, --version`: Show version.

### Examples
```bash
# Launch with a specific download directory
grav-aria2-tui --dir /media/storage/torrents

# On Windows with an existing aria2c instance
grav-aria2-tui -d "D:\Downloads\Torrents" --rpc-url "http://127.0.0.1:6800/jsonrpc"
```

---

## Keybindings

| Key | Action |
|-----|--------|
| `j` / `Down` | Move down in torrent list |
| `k` / `Up` | Move up in torrent list |
| `Tab` / `]` | Next tag filter (`[All]`, `[tag1]`, ...) |
| `Shift+Tab` / `[` | Previous tag filter |
| `s` or `Space` | Toggle Start/Resume or Pause (loads from `.torrent/` if stopped) |
| `S` *(Shift+s)* | Batch Start/Resume all torrents in the active tag filter |
| `P` *(Shift+p)* | Batch Pause all torrents in the active tag filter |
| `a` | Add new torrent (`.torrent` file path or `magnet:` URI + optional tags) |
| `f` | Open File Selection modal for the selected torrent |
| `t` | Edit tags for the selected torrent |
| `o` | Open Per-Torrent limits modal (DL limit, UL limit, seed ratio, seed time) |
| `g` | Open Global speed & seeding limits modal |
| `v` | Force integrity re-check (`check-integrity=true`) and resume |
| `d` / `Delete` | Remove torrent (Choice 1: keep files, Choice 2: delete files from disk) |
| `q` | Quit cleanly |

---

## File Selection Modal

When adding a `.torrent` file or when magnet metadata finishes downloading:
- Navigate files with `j`/`k` or `Up`/`Down`.
- Toggle individual files with `Space`.
- Select all files with `a`.
- Deselect all files with `n`.
- Confirm selection and begin download with `Enter`.
- Cancel/close with `Esc`.

---

## Directory Structure & Persistence

```text
<DIR>/
├── .torrent/
│   ├── state.json                    <-- Persistent metadata, limits, tags
│   ├── <info_hash_1>.torrent         <-- Archived .torrent file
│   └── <info_hash_2>.torrent
├── payload_folder_1/
└── payload_file_2.iso
```

Any `.torrent` files dropped directly into `<DIR>/.torrent/` are automatically detected and registered on startup.
