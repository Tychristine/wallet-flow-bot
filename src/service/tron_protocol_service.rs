use std::collections::HashMap;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::sleep;

use log::{error, info, warn};

use crate::interface::tron_interface::TronHttpClient;
use crate::utils::account_convert::normalize_tron_address;
use crate::utils::conf_util::Config;
use crate::utils::warpper::{
    address_mask, format_token_amount, markdown_v2_escape, time_format_gmt8,
};

/// 单次拉取最大记录数
const PAGE_LIMIT: u32 = 200;

/// 轮询间隔s
const DEFAULT_POLL_INTERVAL_SECS: f64 = 3.0;

/// 解析 TronGrid 返回的 block_timestamp
///
/// TronGrid 的 block_timestamp 字段可能以整数或字符串形式返回，
/// 统一解析为毫秒时间戳。解析失败时返回 0 并记录警告。
fn parse_block_timestamp(value: &serde_json::Value) -> i64 {
    // 优先尝试整数
    if let Some(ts) = value.as_i64() {
        return ts;
    }
    // 再尝试字符串 (TronGrid 偶有以字符串形式返回)
    if let Some(s) = value.as_str()
        && let Ok(ts) = s.parse::<i64>()
    {
        return ts;
    }
    warn!("无法解析 block_timestamp: {:?}", value);
    0
}

/// 转账通知
#[derive(Debug, Clone)]
pub struct TransferNotification {
    /// 方向："🔺传出" 或 "接收"
    pub direction: String,
    /// 代币符号
    pub token_symbol: String,
    /// 格式化后的金额
    pub amount: String,
    /// 发送方地址
    pub from_address: String,
    /// 接收方地址
    pub to_address: String,
    /// 交易哈希
    pub tx_hash: String,
    /// 区块时间戳
    pub timestamp: i64,
}

/// Tron 链上转账监控器
///
/// 通过 TronGrid REST API(地址过滤)拉取交易，替代全块扫描。
pub struct TronMonitor {
    client: TronHttpClient,
    /// 地址b58
    monitor_address: String,
    /// 支持的 TRC-20 代币：合约地址 → (符号, 精度)
    token_contracts: HashMap<String, (String, u32)>,
    poll_interval_secs: f64,
}

impl TronMonitor {
    /// 从全局配置创建监控器实例
    pub fn from_config() -> Result<Self, String> {
        let config = Config::get();

        let target_addr: String = normalize_tron_address(&config.TARGET_ADDRESS)
            .map_err(|e| format!("无效的 TARGET_ADDRESS: {}", e))?;

        let mut token_contracts = HashMap::new();
        // USDT-TRC20
        token_contracts.insert(config.USDT_ADDRESS.clone(), ("USDT-TRC20".to_string(), 6));

        let client =
            TronHttpClient::new(config.RPC_URL.clone(), config.API_KEY.clone(), 15.0, 2, 1.0)?;

        Ok(Self {
            client,
            monitor_address: target_addr,
            token_contracts,
            poll_interval_secs: DEFAULT_POLL_INTERVAL_SECS,
        })
    }

    /// 启动监控主循环，通知通过 `tx` 发送
    pub async fn run(&self, tx: mpsc::Sender<String>) {
        let mut last_timestamp = self.resolve_last_timestamp().await;
        info!(
            "Tron 监控启动: 地址 {}, {} 个代币合约, 轮询间隔 {:.1}s, cursor={}",
            address_mask(&self.monitor_address),
            self.token_contracts.len(),
            self.poll_interval_secs,
            last_timestamp
        );

        loop {
            sleep(Duration::from_secs_f64(self.poll_interval_secs)).await;

            // 拉取 TRC-20 转账
            match self.fetch_and_process_trc20(last_timestamp, &tx).await {
                Ok(ts) => {
                    if ts > last_timestamp {
                        info!("TRC-20 cursor 推进: {} → {}", last_timestamp, ts);
                        last_timestamp = ts;
                    }
                }
                Err(e) => error!("拉取 TRC-20 失败: {}", e),
            }

            // 拉取原生 TRX 转账
            match self.fetch_and_process_native(last_timestamp, &tx).await {
                Ok(ts) => {
                    if ts > last_timestamp {
                        info!("TRX cursor 推进: {} → {}", last_timestamp, ts);
                        last_timestamp = ts;
                    }
                }
                Err(e) => error!("拉取原生 TRX 失败: {}", e),
            }
        }
    }

    /// 拉取 TRC-20 转账并发送通知
    /// 返回新的 cursor timestamp(处理后最大 ts + 1s)，下次用此值做 min_timestamp
    /// 已固化接口 `/v1/accounts/{address}/transactions/trc20`
    async fn fetch_and_process_trc20(
        &self,
        min_ts: i64,
        tx: &mpsc::Sender<String>,
    ) -> Result<i64, String> {
        let url = format!(
            "/v1/accounts/{}/transactions/trc20?only_confirmed=true&limit={}&min_timestamp={}",
            self.monitor_address, PAGE_LIMIT, min_ts
        );
        // 检查date字段数据，若空数据时接口返回"data.[0]"，若有记录则"date.[n]"
        let records = match self.client.share_http(&url).await {
            Ok(r) => r["data"].as_array().cloned().unwrap_or_default(),
            Err(e) => return Err(e),
        };

        // 提取数据
        for record in &records {
            let block_ts = parse_block_timestamp(&record["block_timestamp"]);

            // 只处理 USDT(token_info.address 匹配)
            let token_addr = record["token_info"]["address"].as_str().unwrap_or("");
            if !self.token_contracts.contains_key(token_addr) {
                continue;
            }
            // 处理地址过滤
            let from = record["from"].as_str().unwrap_or("");
            let to = record["to"].as_str().unwrap_or("");
            let from_monitored = from == self.monitor_address;
            let to_monitored = to == self.monitor_address;
            if !from_monitored && !to_monitored {
                continue;
            }

            let value_str = record["value"].as_str().unwrap_or("0");
            let raw_amount: i64 = value_str.parse().unwrap_or(0);
            let (symbol, decimals) = &self.token_contracts[token_addr];
            let amount = format_token_amount(raw_amount, *decimals);

            let tx_id = record["transaction_id"].as_str().unwrap_or("");

            // 处理通知消息，出账类型
            if from_monitored {
                let msg = self.format_message(&TransferNotification {
                    direction: "🔺传出".to_string(),
                    token_symbol: symbol.clone(),
                    amount: amount.clone(),
                    from_address: from.to_string(),
                    to_address: to.to_string(),
                    tx_hash: tx_id.to_string(),
                    timestamp: block_ts,
                });
                // cursor 步进+1，从新的时间戳拉取交易事件
                // 步进1000ms=1s，步进时间合理
                if tx.send(msg).await.is_err() {
                    warn!("通知消息发送失败(channel 已关闭)");
                    return Ok(min_ts + 1000);
                }
            }

            // 处理通知消息，入账类型
            if to_monitored && to != from {
                let msg = self.format_message(&TransferNotification {
                    direction: "🟢接收".to_string(),
                    token_symbol: symbol.clone(),
                    amount,
                    from_address: from.to_string(),
                    to_address: to.to_string(),
                    tx_hash: tx_id.to_string(),
                    timestamp: block_ts,
                });
                if tx.send(msg).await.is_err() {
                    warn!("通知消息发送失败(channel 已关闭)");
                    return Ok(min_ts);
                }
            }
        }

        // 无记录不步进；有记录则取 data[0] 首位元素的 block_timestamp +1000ms(1秒) 作为下一轮 min_ts
        if records.is_empty() {
            Ok(min_ts)
        } else {
            let first_ts: i64 = parse_block_timestamp(&records[0]["block_timestamp"]);
            Ok(first_ts.max(min_ts) + 1000)
        }
    }

    /// 拉取原生 TRX 转账并发送通知
    /// 接口`/v1/accounts/{address}/transactions/?only_confirmed={true}&limit={200}&min_timestamp={timesatamp}`
    async fn fetch_and_process_native(
        &self,
        min_ts: i64,
        tx: &mpsc::Sender<String>,
    ) -> Result<i64, String> {
        let url = format!(
            "/v1/accounts/{}/transactions?only_confirmed=true&limit={}&min_timestamp={}",
            self.monitor_address, PAGE_LIMIT, min_ts
        );
        let records = match self.client.share_http(&url).await {
            Ok(r) => r["data"].as_array().cloned().unwrap_or_default(),
            Err(e) => return Err(e),
        };

        for record in &records {
            let tx_id = record["txID"].as_str().unwrap_or("");
            if tx_id.is_empty() {
                continue;
            }

            // 只处理成功的交易 (不是必须)
            let is_success = record["ret"]
                .as_array()
                .and_then(|arr| arr.first())
                .and_then(|r| r["contractRet"].as_str())
                .map(|s| s == "SUCCESS")
                .unwrap_or(false);
            // 不必要的
            if !is_success {
                continue;
            }

            // 只处理 "type": "TransferContract" 的即为原生TRX转移事件
            let contracts = record["raw_data"]["contract"].as_array();
            let contract = contracts.and_then(|c| c.first());
            let contract_type = contract.and_then(|c| c["type"].as_str());
            if contract_type != Some("TransferContract") {
                continue;
            }

            let value = contract.and_then(|c| c["parameter"]["value"].as_object());
            let amount_sun = value
                .and_then(|v| v.get("amount"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            // 跳过粉尘
            if amount_sun < 9999 {
                continue;
            }

            let from_hex = value
                .and_then(|v| v.get("owner_address"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let to_hex = value
                .and_then(|v| v.get("to_address"))
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let from = normalize_tron_address(from_hex).unwrap_or_default();
            let to = normalize_tron_address(to_hex).unwrap_or_default();
            if from.is_empty() || to.is_empty() {
                continue;
            }

            let from_monitored = from == self.monitor_address;
            let to_monitored = to == self.monitor_address;
            if !from_monitored && !to_monitored {
                continue;
            }

            let block_ts = parse_block_timestamp(&record["block_timestamp"]);

            // 消息通知所用的数据包装
            let amount = format_token_amount(amount_sun, 6);

            if from_monitored {
                let msg = self.format_message(&TransferNotification {
                    direction: "🔺传出".to_string(),
                    token_symbol: "TRX".to_string(),
                    amount: amount.clone(),
                    from_address: from.clone(),
                    to_address: to.clone(),
                    tx_hash: tx_id.to_string(),
                    timestamp: block_ts,
                });
                if tx.send(msg).await.is_err() {
                    warn!("通知发送失败(channel 已关闭)");
                    return Ok(min_ts + 1000);
                }
            }

            if to_monitored && to != from {
                let msg = self.format_message(&TransferNotification {
                    direction: "🟢接收".to_string(),
                    token_symbol: "TRX".to_string(),
                    amount,
                    from_address: from,
                    to_address: to,
                    tx_hash: tx_id.to_string(),
                    timestamp: block_ts,
                });
                if tx.send(msg).await.is_err() {
                    warn!("通知发送失败(channel 已关闭)");
                    return Ok(min_ts + 1000);
                }
            }
        }

        // 无记录不步进；有记录则取 data[0] 首位元素的 block_timestamp +1000ms(1秒) 作为下一轮 min_ts
        if records.is_empty() {
            Ok(min_ts)
        } else {
            let first_ts = parse_block_timestamp(&records[0]["block_timestamp"]);
            Ok(first_ts.max(min_ts) + 1000)
        }
    }

    async fn resolve_last_timestamp(&self) -> i64 {
        // 3*19 = 57
        chrono::Utc::now().timestamp_millis() - 60_000
    }

    ///  格式化
    /// 格式化通知消息(Telegram MarkdownV2，仅时间/哈希用 backtick)
    pub fn format_message(&self, notification: &TransferNotification) -> String {
        format!(
            "Tron Network\n\
             交易类型\\*: {}\n\
             交易货币\\*: {}\n\
             交易金额\\*: {}\n\
             出账地址\\*: `{}`\n\
             入账地址\\*: `{}`\n\
             交易时间\\*: `{} GMT+8`\n\
             交易Hash\\*: `{}`",
            markdown_v2_escape(&notification.direction),
            markdown_v2_escape(&notification.token_symbol),
            markdown_v2_escape(&notification.amount),
            address_mask(&notification.from_address),
            address_mask(&notification.to_address),
            time_format_gmt8(notification.timestamp),
            markdown_v2_escape(&notification.tx_hash),
        )
    }
}

///  公开入口
/// 启动 Tron 监听服务
/// 返回 `mpsc::Receiver<String>`，接收侧可读取格式化后的通知消息并通过 Telegram 发送。
/// 监控器在独立的 tokio task 中运行，channel 关闭意味着监控器已停止。
pub fn start_monitor() -> Result<mpsc::Receiver<String>, String> {
    let monitor = TronMonitor::from_config()?;
    let (tx, rx) = mpsc::channel::<String>(32);

    tokio::spawn(async move {
        monitor.run(tx).await;
    });

    Ok(rx)
}
