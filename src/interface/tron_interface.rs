use crate::utils::warpper::retry_post_json;
use reqwest::Client;
use serde_json::Value;
use std::time::Duration;

/// Tron Full Node HTTP-JSON RPC 客户端
/// 封装 POST {url}/{path} 调用，支持：
/// - TRON-PRO-API-KEY 请求头鉴权
/// - 可配置超时、重试次数、重试间隔
/// - 自动检查返回体中的 "Error" 字段
pub struct TronHttpClient {
    url: String,
    api_key: String,
    retries: u32,
    retry_delay: Duration,
    client: Client,
}

impl TronHttpClient {
    /// 创建新的客户端
    /// - `url`:      全节点 RPC 地址
    /// - `api_key`:  TronGrid API Key(可为空字符串)
    /// - `timeout_secs`: 请求超时秒数
    /// - `retries`:  失败重试次数
    /// - `retry_delay_secs`: 重试间隔秒数
    pub fn new(
        url: String,
        api_key: String,
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
            url: url.trim_end_matches('/').to_string(),
            api_key: api_key.trim().to_string(),
            retries,
            retry_delay: Duration::from_secs_f64(retry_delay_secs),
            client,
        })
    }

    /// 调用 Tron RPC 接口
    /// - path: 请求路径
    /// - payload: json请求体 (可选)
    ///   返回解析后的 JSON Value；失败时返回错误描述字符串
    pub async fn call(&self, path: &str, payload: Option<Value>) -> Result<Value, String> {
        let url = format!("{}/{}", self.url, path.trim_start_matches('/'));
        let body = payload.unwrap_or(Value::Object(Default::default()));
        // 检查请求头
        let headers = if !self.api_key.is_empty() {
            vec![("TRON-PRO-API-KEY", self.api_key.as_str())]
        } else {
            vec![]
        };
        // 请求体
        let result = retry_post_json(
            &self.client,
            &url,
            &headers,
            &body,
            self.retries,
            self.retry_delay,
            path,
        )
        .await?;

        // Tron API 层面错误检查
        if let Some(err_msg) = result.get("Error").and_then(|v| v.as_str()) {
            return Err(format!("Tron API 错误 [{}]: {}", path, err_msg));
        }

        Ok(result)
    }

    /// GET 请求（路径+查询参数已由调用方传递），用于 REST 查询接口
    /// 与RPC调用不同，纯纯GET请求用于Tron相关业务
    pub async fn share_http(&self, path_with_query: &str) -> Result<Value, String> {
        let url = format!("{}/{}", self.url, path_with_query.trim_start_matches('/'));

        let mut last_error = String::new();
        for attempt in 0..=self.retries {
            let mut request = self.client.get(&url).header("Accept", "application/json");
            if !self.api_key.is_empty() {
                request = request.header("TRON-PRO-API-KEY", &self.api_key);
            }

            match request.send().await {
                Ok(resp) => {
                    let status = resp.status();
                    let resp_text = match resp.text().await {
                        Ok(t) => t,
                        Err(e) => {
                            last_error = format!("读取响应失败: {}", e);
                            if attempt < self.retries {
                                tokio::time::sleep(self.retry_delay).await;
                            }
                            continue;
                        }
                    };

                    if !status.is_success() {
                        let preview: String = resp_text.chars().take(200).collect();
                        last_error = format!("HTTP {}: {}", status.as_u16(), preview);
                        if attempt < self.retries {
                            tokio::time::sleep(self.retry_delay).await;
                        }
                        continue;
                    }

                    if resp_text.trim().is_empty() {
                        last_error = "响应体为空".to_string();
                        if attempt < self.retries {
                            tokio::time::sleep(self.retry_delay).await;
                        }
                        continue;
                    }

                    let body: Value = match serde_json::from_str(&resp_text) {
                        Ok(v) => v,
                        Err(e) => {
                            let preview: String = resp_text.chars().take(200).collect();
                            last_error = format!("JSON 解析失败(前200字符: {}): {}", preview, e);
                            if attempt < self.retries {
                                tokio::time::sleep(self.retry_delay).await;
                            }
                            continue;
                        }
                    };

                    if let Some(err_msg) = body.get("Error").and_then(|v| v.as_str()) {
                        return Err(format!("Tron API 错误: {}", err_msg));
                    }
                    return Ok(body);
                }
                Err(e) => {
                    last_error = format!("{}", e);
                    if attempt < self.retries {
                        tokio::time::sleep(self.retry_delay).await;
                    }
                }
            }
        }
        Err(format!(
            "[GET {}] 请求失败（已重试 {} 次）: {}",
            path_with_query, self.retries, last_error
        ))
    }
}
