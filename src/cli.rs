use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug, Clone)]
#[command(
    name = "grav-aria2-tui",
    author = "Antigravity",
    version,
    about = "BitTorrent TUI management interface for aria2c"
)]
pub struct Cli {
    /// Root directory for torrent downloads and .torrent/ metadata
    #[arg(short = 'd', long = "dir", value_name = "PATH")]
    pub dir: PathBuf,

    /// aria2c JSON-RPC endpoint URL
    #[arg(long = "rpc-url", default_value = "http://127.0.0.1:6800/jsonrpc")]
    pub rpc_url: String,

    /// Optional aria2c RPC secret token
    #[arg(long = "rpc-secret", value_name = "TOKEN")]
    pub rpc_secret: Option<String>,

    /// Path to aria2c binary executable
    #[arg(long = "aria2-bin", default_value = "aria2c")]
    pub aria2_bin: String,
}
