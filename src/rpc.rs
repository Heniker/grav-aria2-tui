#![allow(dead_code)]

use anyhow::{Context, Result, bail};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

#[derive(Debug, Clone, Deserialize)]
pub struct VersionInfo {
    pub version: String,
    #[serde(rename = "enabledFeatures", default)]
    pub enabled_features: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct GlobalStat {
    #[serde(rename = "downloadSpeed", default = "default_zero_str")]
    pub download_speed: String,
    #[serde(rename = "uploadSpeed", default = "default_zero_str")]
    pub upload_speed: String,
    #[serde(rename = "numActive", default = "default_zero_str")]
    pub num_active: String,
    #[serde(rename = "numWaiting", default = "default_zero_str")]
    pub num_waiting: String,
    #[serde(rename = "numStopped", default = "default_zero_str")]
    pub num_stopped: String,
}

fn default_zero_str() -> String {
    "0".to_string()
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct BittorrentInfo {
    pub name: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct BittorrentStatus {
    pub info: Option<BittorrentInfo>,
    pub mode: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct FileInfo {
    pub index: String,
    pub path: String,
    pub length: String,
    #[serde(rename = "completedLength", default = "default_zero_str")]
    pub completed_length: String,
    #[serde(default)]
    pub selected: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct DownloadStatus {
    pub gid: String,
    pub status: String,
    #[serde(rename = "totalLength", default = "default_zero_str")]
    pub total_length: String,
    #[serde(rename = "completedLength", default = "default_zero_str")]
    pub completed_length: String,
    #[serde(rename = "uploadLength", default = "default_zero_str")]
    pub upload_length: String,
    #[serde(rename = "downloadSpeed", default = "default_zero_str")]
    pub download_speed: String,
    #[serde(rename = "uploadSpeed", default = "default_zero_str")]
    pub upload_speed: String,
    #[serde(rename = "infoHash")]
    pub info_hash: Option<String>,
    #[serde(rename = "numPieces")]
    pub num_pieces: Option<String>,
    #[serde(rename = "pieceLength")]
    pub piece_length: Option<String>,
    #[serde(rename = "numSeeders")]
    pub num_seeders: Option<String>,
    pub seeder: Option<String>,
    pub connections: Option<String>,
    #[serde(rename = "errorCode")]
    pub error_code: Option<String>,
    #[serde(rename = "errorMessage")]
    pub error_message: Option<String>,
    #[serde(rename = "followedBy")]
    pub followed_by: Option<Vec<String>>,
    pub following: Option<String>,
    pub bittorrent: Option<BittorrentStatus>,
    pub files: Option<Vec<FileInfo>>,
    pub dir: Option<String>,
}

#[derive(Debug, Serialize)]
struct JsonRpcRequest {
    jsonrpc: &'static str,
    id: String,
    method: String,
    params: Vec<Value>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcResponse<T> {
    #[allow(dead_code)]
    pub id: Option<Value>,
    pub result: Option<T>,
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
}

#[derive(Clone)]
pub struct Aria2RpcClient {
    client: Client,
    rpc_url: String,
    rpc_secret: Option<String>,
    req_counter: std::sync::Arc<AtomicU64>,
}

impl Aria2RpcClient {
    pub fn new(rpc_url: String, rpc_secret: Option<String>) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_default();
        Self {
            client,
            rpc_url,
            rpc_secret,
            req_counter: std::sync::Arc::new(AtomicU64::new(1)),
        }
    }

    pub fn rpc_url(&self) -> &str {
        &self.rpc_url
    }

    pub fn rpc_secret(&self) -> Option<&str> {
        self.rpc_secret.as_deref()
    }

    async fn call_rpc<T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        mut params: Vec<Value>,
    ) -> Result<T> {
        if let Some(secret) = &self.rpc_secret {
            params.insert(0, json!(format!("token:{}", secret)));
        }

        let id = self.req_counter.fetch_add(1, Ordering::SeqCst).to_string();
        let payload = JsonRpcRequest {
            jsonrpc: "2.0",
            id,
            method: method.to_string(),
            params,
        };

        let response = self
            .client
            .post(&self.rpc_url)
            .json(&payload)
            .send()
            .await
            .with_context(|| format!("Failed to send RPC request '{}' to {}", method, self.rpc_url))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            bail!("HTTP error {} from aria2 RPC: {}", status, text);
        }

        let rpc_res: JsonRpcResponse<T> = response
            .json()
            .await
            .with_context(|| format!("Failed to parse RPC response for '{}'", method))?;

        if let Some(err) = rpc_res.error {
            bail!("aria2 RPC error ({}) for '{}': {}", err.code, method, err.message);
        }

        match rpc_res.result {
            Some(res) => Ok(res),
            None => bail!("aria2 RPC response for '{}' had neither result nor error", method),
        }
    }

    pub async fn get_version(&self) -> Result<VersionInfo> {
        self.call_rpc("aria2.getVersion", vec![]).await
    }

    pub async fn change_global_option(&self, options: Value) -> Result<()> {
        let _: String = self.call_rpc("aria2.changeGlobalOption", vec![options]).await?;
        Ok(())
    }

    pub async fn get_global_option(&self) -> Result<Value> {
        self.call_rpc("aria2.getGlobalOption", vec![]).await
    }

    pub async fn get_global_stat(&self) -> Result<GlobalStat> {
        self.call_rpc("aria2.getGlobalStat", vec![]).await
    }

    pub async fn add_torrent(
        &self,
        base64_bytes: &str,
        uris: Vec<String>,
        options: Value,
    ) -> Result<String> {
        self.call_rpc(
            "aria2.addTorrent",
            vec![json!(base64_bytes), json!(uris), options],
        )
        .await
    }

    pub async fn add_uri(&self, uris: Vec<String>, options: Value) -> Result<String> {
        self.call_rpc("aria2.addUri", vec![json!(uris), options]).await
    }

    pub async fn tell_active(&self) -> Result<Vec<DownloadStatus>> {
        self.call_rpc("aria2.tellActive", vec![]).await
    }

    pub async fn tell_waiting(&self, offset: i64, num: i64) -> Result<Vec<DownloadStatus>> {
        self.call_rpc("aria2.tellWaiting", vec![json!(offset), json!(num)]).await
    }

    pub async fn tell_stopped(&self, offset: i64, num: i64) -> Result<Vec<DownloadStatus>> {
        self.call_rpc("aria2.tellStopped", vec![json!(offset), json!(num)]).await
    }

    pub async fn tell_status(&self, gid: &str) -> Result<DownloadStatus> {
        self.call_rpc("aria2.tellStatus", vec![json!(gid)]).await
    }

    pub async fn get_files(&self, gid: &str) -> Result<Vec<FileInfo>> {
        self.call_rpc("aria2.getFiles", vec![json!(gid)]).await
    }

    pub async fn change_option(&self, gid: &str, options: Value) -> Result<()> {
        let _: String = self.call_rpc("aria2.changeOption", vec![json!(gid), options]).await?;
        Ok(())
    }

    pub async fn get_option(&self, gid: &str) -> Result<Value> {
        self.call_rpc("aria2.getOption", vec![json!(gid)]).await
    }

    pub async fn pause(&self, gid: &str) -> Result<()> {
        let _: String = self.call_rpc("aria2.pause", vec![json!(gid)]).await?;
        Ok(())
    }

    pub async fn unpause(&self, gid: &str) -> Result<()> {
        let _: String = self.call_rpc("aria2.unpause", vec![json!(gid)]).await?;
        Ok(())
    }

    pub async fn remove(&self, gid: &str) -> Result<()> {
        let _: String = self.call_rpc("aria2.remove", vec![json!(gid)]).await?;
        Ok(())
    }

    pub async fn force_remove(&self, gid: &str) -> Result<()> {
        let _: String = self.call_rpc("aria2.forceRemove", vec![json!(gid)]).await?;
        Ok(())
    }

    pub async fn remove_download_result(&self, gid: &str) -> Result<()> {
        let _: String = self.call_rpc("aria2.removeDownloadResult", vec![json!(gid)]).await?;
        Ok(())
    }

    pub async fn shutdown(&self) -> Result<()> {
        let _: String = self.call_rpc("aria2.shutdown", vec![]).await?;
        Ok(())
    }

    pub async fn force_shutdown(&self) -> Result<()> {
        let _: String = self.call_rpc("aria2.forceShutdown", vec![]).await?;
        Ok(())
    }
}
