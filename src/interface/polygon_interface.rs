use reqwest::Client;
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::utils::warpper::retry_post_json;

/// Polygon JSON-RPC 2.0 客户端
///
/// 封装 `POST {url}` 调用，请求格式：
/// `{"jsonrpc":"2.0","id":N,"method":"...","params":[...]}`
pub struct PolygonRpcClient {
    url: String,
    retries: u32,
    retry_delay: Duration,
    client: Client,
    request_id: AtomicU64,
}

impl PolygonRpcClient {
    pub fn new(
        url: String,
        timeout_secs: f64,
        retries: u32,
        retry_delay_secs: f64,
    ) -> Result<Self, String> {
        let client = Client::builder()
            .timeout(Duration::from_secs_f64(timeout_secs))
            .pool_max_idle_per_host(2)
            .build()
            .map_err(|e| format!("创建 HTTP 客户端失败: {}", e))?;

        Ok(Self {
            url,
            retries,
            retry_delay: Duration::from_secs_f64(retry_delay_secs),
            client,
            request_id: AtomicU64::new(1),
        })
    }

    pub async fn call(&self, method: &str, params: Vec<Value>) -> Result<Value, String> {
        let id = self.request_id.fetch_add(1, Ordering::Relaxed);

        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        let result = retry_post_json(
            &self.client,
            &self.url,
            &[],
            &payload,
            self.retries,
            self.retry_delay,
            method,
        )
        .await?;

        // JSON-RPC error
        if let Some(err) = result.get("error") {
            return Err(format!(
                "RPC 错误 [{}]: {}",
                method,
                serde_json::to_string(&err).unwrap_or_default()
            ));
        }

        Ok(result["result"].clone())
    }
}

