use chrono::{DateTime, FixedOffset};

// 通用代币金额格式化
pub fn format_token_amount(raw_value: i64, decimals: u32) -> String {
    if decimals == 0 {
        return raw_value.to_string();
    }
    // 右对齐补齐，确保至少有 decimals+1 位
    let digits: String = format!("{:0>width$}", raw_value, width = (decimals as usize) + 1);
    let split: usize = digits.len() - decimals as usize;
    let whole: &str = &digits[..split];
    let fraction: &str = digits[split..].trim_end_matches('0');
    if fraction.is_empty() {
        whole.to_string()
    } else {
        format!("{}.{}", whole, fraction)
    }
}

/// Unix 时间戳 → 上海时间 (UTC+8) 可读字符串
/// 自动识别秒 / 毫秒；无效时间戳返回 "N/A"
pub fn time_format_gmt8(timestamp: i64) -> String {
    // 毫秒 → 秒
    let ts: i64 = if timestamp > 10_000_000_000 {
        timestamp / 1000
    } else {
        timestamp
    };

    let utc = match DateTime::from_timestamp(ts, 0) {
        Some(dt) => dt,
        None => return "N/A".to_string(),
    };

    let shanghai = FixedOffset::east_opt(8 * 3600).expect("UTC+8 offset");
    utc.with_timezone(&shanghai)
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
}

/**
 * 地址遮蔽 展示(Txx123...12345ab)
 * 不显示完整地址
 */
pub fn address_mask(address: &str) -> String {
    let chars: Vec<char> = address.chars().collect();
    if chars.len() > 15 {
        let head: String = chars.iter().take(5).collect();
        let tail: String = chars.iter().skip(chars.len() - 7).collect();
        format!("{}...{}", head, tail)
    } else {
        address.to_string()
    }
}

/// 转义 Telegram MarkdownV2 保留字符（单次遍历，零额外分配）
pub fn markdown_v2_escape(s: &str) -> String {
    // 快速路径：无特殊字符直接返回
    if !s.contains(|c: char| ESCAPE_SET.contains(&c)) {
        return s.to_string();
    }

    let mut out = String::with_capacity(s.len() + 16);
    for c in s.chars() {
        if ESCAPE_SET.contains(&c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

use std::collections::HashSet;
use std::sync::LazyLock;
use std::time::Duration;
use tokio::time::sleep;

use reqwest::Client;
use serde_json::Value;

/// 带重试的 HTTP POST 请求
///
/// 处理 transport 层重试（非 2xx、连接错误、JSON 解析失败），
/// 返回解析后的 JSON Value，API 层错误由调用方自行判断。
pub(crate) async fn retry_post_json(
    client: &Client,
    url: &str,
    headers: &[(&str, &str)],
    body: &Value,
    retries: u32,
    retry_delay: Duration,
    context: &str,
) -> Result<Value, String> {
    let mut last_error = String::new();

    for attempt in 0..=retries {
        let mut request = client.post(url).header("Content-Type", "application/json");
        for &(k, v) in headers {
            request = request.header(k, v);
        }

        match request.json(body).send().await {
            Ok(response) => {
                let status = response.status();
                let resp_text = response
                    .text()
                    .await
                    .map_err(|e| format!("读取响应失败: {}", e))?;

                // 非 2xx → 限流 / 网关错误
                if !status.is_success() {
                    let preview: String = resp_text.chars().take(200).collect();
                    last_error = format!("HTTP {}: {}", status.as_u16(), preview);
                    if attempt < retries {
                        sleep(retry_delay).await;
                    }
                    continue;
                }

                // 空响应
                if resp_text.trim().is_empty() {
                    last_error = "响应体为空".to_string();
                    if attempt < retries {
                        sleep(retry_delay).await;
                    }
                    continue;
                }

                // JSON 解析
                match serde_json::from_str(&resp_text) {
                    Ok(v) => return Ok(v),
                    Err(e) => {
                        let preview: String = resp_text.chars().take(200).collect();
                        last_error = format!("JSON 解析失败(前200字符: {}): {}", preview, e);
                        if attempt < retries {
                            sleep(retry_delay).await;
                        }
                        continue;
                    }
                }
            }
            Err(e) => {
                last_error = format!("{}", e);
                if attempt < retries {
                    sleep(retry_delay).await;
                }
            }
        }
    }

    Err(format!(
        "[{}] 请求失败（已重试 {} 次）: {}",
        context, retries, last_error
    ))
}

static ESCAPE_SET: LazyLock<HashSet<char>> = LazyLock::new(|| {
    [
        '\\', '_', '*', '[', ']', '(', ')', '~', '`', '>', '#', '+', '-', '=', '|', '{', '}', '.',
        '!',
    ]
    .into_iter()
    .collect()
});
