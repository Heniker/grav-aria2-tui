use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::{Child, Command};

use crate::rpc::Aria2RpcClient;
use crate::utils::clean_path;

pub struct ProcessManager {
    pub client: Aria2RpcClient,
    child: Option<Child>,
}

impl ProcessManager {
    pub async fn init(
        aria2_bin: &str,
        rpc_url: &str,
        rpc_secret: Option<String>,
        download_dir: &Path,
    ) -> Result<Self> {
        let client = Aria2RpcClient::new(rpc_url.to_string(), rpc_secret.clone());
        let clean_dir = clean_path(download_dir);
        let dir_str = clean_dir.to_string_lossy().to_string();

        // 1. Determine listen port from rpc_url (default 6800)
        let port = extract_port_from_url(rpc_url).unwrap_or(6800);

        // 2. Build spawn command for dedicated aria2c process
        let mut cmd = Command::new(aria2_bin);
        cmd.arg("--enable-rpc=true")
            .arg("--rpc-listen-all=false")
            .arg(format!("--rpc-listen-port={}", port))
            .arg(format!("--dir={}", dir_str))
            .arg("--file-allocation=none")
            .arg("--bt-save-metadata=true")
            .arg("--bt-load-saved-metadata=true")
            .arg("--continue=true")
            .arg("--quiet=true")
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        if let Some(secret) = &rpc_secret {
            cmd.arg(format!("--rpc-secret={}", secret));
        }

        let child = cmd
            .spawn()
            .with_context(|| format!("Failed to spawn aria2c binary '{}'. Ensure aria2c is installed and on PATH or passed via --aria2-bin.", aria2_bin))?;

        let mut mgr = Self {
            client,
            child: Some(child),
        };

        // 3. Poll getVersion until RPC server is ready
        let mut ready = false;
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(100)).await;

            // Check if child process died early (e.g. port already in use)
            if let Some(ref mut c) = mgr.child {
                if let Ok(Some(status)) = c.try_wait() {
                    bail!(
                        "aria2c exited unexpectedly with code {:?} before RPC was ready. Port {} may already be in use.",
                        status.code(),
                        port
                    );
                }
            }

            if mgr.client.get_version().await.is_ok() {
                ready = true;
                break;
            }
        }

        if !ready {
            mgr.shutdown().await;
            bail!(
                "Timed out waiting for spawned aria2c RPC server to start at {}",
                rpc_url
            );
        }

        // Set global dir explicitly
        let _ = mgr
            .client
            .change_global_option(serde_json::json!({ "dir": &dir_str }))
            .await;

        Ok(mgr)
    }

    pub async fn shutdown(&mut self) {
        // Attempt graceful RPC shutdown; only kill the child if it doesn't respond
        let graceful = tokio::time::timeout(Duration::from_millis(800), self.client.shutdown())
            .await
            .is_ok();

        if let Some(mut child) = self.child.take() {
            if !graceful {
                let _ = child.kill().await;
            }
            let _ = child.wait().await;
        }
    }
}

impl Drop for ProcessManager {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
        }
    }
}

fn extract_port_from_url(url_str: &str) -> Option<u16> {
    if let Ok(parsed) = url::Url::parse(url_str) {
        if let Some(p) = parsed.port() {
            return Some(p);
        }
    }
    // Fallback simple search for :port/
    if let Some(pos) = url_str.find("://") {
        let rest = &url_str[pos + 3..];
        let host_port = rest.split('/').next().unwrap_or(rest);
        if let Some(colon) = host_port.rfind(':') {
            if let Ok(p) = host_port[colon + 1..].parse::<u16>() {
                return Some(p);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_port_from_url() {
        assert_eq!(extract_port_from_url("http://127.0.0.1:6800/jsonrpc"), Some(6800));
        assert_eq!(extract_port_from_url("http://localhost:8080/jsonrpc"), Some(8080));
        assert_eq!(extract_port_from_url("http://127.0.0.1/jsonrpc"), None);
    }

    #[tokio::test]
    async fn test_process_spawn_and_shutdown() {
        let temp_dir = std::env::temp_dir().join(format!("aria2_proc_test_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let _ = std::fs::create_dir_all(&temp_dir);

        let url = "http://127.0.0.1:6891/jsonrpc";
        let res = ProcessManager::init("aria2c", url, None, &temp_dir).await;
        if let Ok(mut mgr) = res {
            let ver = mgr.client.get_version().await;
            assert!(ver.is_ok());

            // Verify file-allocation is "none" by default
            let opts = mgr.client.get_global_option().await;
            if let Ok(val) = opts {
                assert_eq!(val.get("file-allocation").and_then(|v| v.as_str()), Some("none"));
            }

            mgr.shutdown().await;
        }

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}

