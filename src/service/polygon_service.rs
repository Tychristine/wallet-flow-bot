use std::collections::{HashMap, HashSet};

use log::{debug, error, info, warn};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::time::sleep;

use crate::interface::polygon_interface::PolygonRpcClient;
use crate::utils::polygon_convert::{
    decode_topic_address, hex_to_u64, normalize_address, topic_for_address,
};
use crate::utils::warpper::{
    address_mask, format_token_amount, markdown_v2_escape, time_format_gmt8,
};
use chrono::Utc;

const TRANSFER_EVENT_TOPIC: &str =
    "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";

// 轮询频率4.0 seconds/次
const DEFAULT_POLL_INTERVAL_SECS: f64 = 4.0;
const DEFAULT_CONFIRMATIONS: u64 = 1;
const DEFAULT_RPC_TIMEOUT_SECS: f64 = 15.0;
const DEFAULT_RPC_RETRIES: u32 = 2;
const DEFAULT_RPC_RETRY_DELAY_SECS: f64 = 1.0;

//  数据模型
#[derive(Debug, Clone)]
pub struct TransferNotification {
    pub direction: String,
    pub token_symbol: String,
    pub amount: String,
    pub from_address: String,
    pub to_address: String,
    pub tx_hash: String,
    pub timestamp: i64,
}

//  PolygonMonitor
pub struct PolygonMonitor {
    rpc: PolygonRpcClient,
    monitor_addresses: HashSet<String>,
    /// 监控地址对应的 topic（用于 eth_getLogs 过滤）
    monitored_topics: Vec<String>,
    poll_interval_secs: f64,
    confirmations: u64,
    /// 支持的 ERC-20 代币合约地址集合
    token_contracts: HashSet<String>,
}

impl PolygonMonitor {
    pub fn new(rpc_url: String, monitor_addresses_raw: &str) -> Result<Self, String> {
        let rpc = PolygonRpcClient::new(
            rpc_url,
            DEFAULT_RPC_TIMEOUT_SECS,
            DEFAULT_RPC_RETRIES,
            DEFAULT_RPC_RETRY_DELAY_SECS,
        )?;

        let mut monitor_addresses = HashSet::new();
        for addr in monitor_addresses_raw.split(',') {
            let addr = addr.trim();
            if !addr.is_empty() {
                monitor_addresses.insert(normalize_address(addr)?);
            }
        }

        let monitored_topics: Vec<String> = monitor_addresses
            .iter()
            .map(|a| topic_for_address(a))
            .collect();

        let mut token_contracts = HashSet::new();
        // USDT-POLYGON
        token_contracts.insert("0xc2132d05d31c914a87c6611c10748aeb04b58e8f".to_string());

        Ok(Self {
            rpc,
            monitor_addresses,
            monitored_topics,
            poll_interval_secs: DEFAULT_POLL_INTERVAL_SECS,
            confirmations: DEFAULT_CONFIRMATIONS,
            token_contracts,
        })
    }

    //  主循环
    pub async fn run(self, tx: mpsc::Sender<String>) {
        let mut last_processed = self.resolve_last_processed_block().await;
        info!(
            "[Polygon] 监控启动: {} 个地址, {} 个代币合约, 轮询间隔 {:.1}s, 确认块数 {}",
            self.monitor_addresses.len(),
            self.token_contracts.len(),
            self.poll_interval_secs,
            self.confirmations
        );
        info!("[Polygon] 从区块 {} 开始监听", last_processed + 1);

        loop {
            let chain_head = match self.get_latest_block_number().await {
                Ok(n) => n,
                Err(e) => {
                    error!("[Polygon] 获取最新区块失败: {}", e);
                    sleep(std::time::Duration::from_secs_f64(self.poll_interval_secs)).await;
                    continue;
                }
            };

            let target_block = chain_head.saturating_sub(self.confirmations);
            if target_block <= last_processed {
                sleep(std::time::Duration::from_secs_f64(self.poll_interval_secs)).await;
                continue;
            }

            for block_number in (last_processed + 1)..=target_block {
                match self.process_block(block_number, &tx).await {
                    Ok(_) => {
                        last_processed = block_number;
                    }
                    Err(e) => {
                        error!("[Polygon] 处理区块 {} 失败: {}", block_number, e);
                        break;
                    }
                }
            }
        }
    }

    //  区块处理

    async fn process_block(
        &self,
        block_number: u64,
        tx: &mpsc::Sender<String>,
    ) -> Result<(), String> {
        // 仅监听 ERC-20 转账（USDT/USDC/USDC.e），不监听原生 POL 转账
        let timestamp = Utc::now().timestamp();
        let notifications = self.collect_token_transfers(block_number, timestamp).await;

        if !notifications.is_empty() {
            info!(
                "[Polygon] 区块 {} 检测到 {} 笔关联转账",
                block_number,
                notifications.len()
            );
        }

        for notification in &notifications {
            debug!(
                "[Polygon] 通知: {} {} {} tx={}",
                notification.direction,
                notification.token_symbol,
                notification.amount,
                notification.tx_hash
            );
            let message: String = self.format_message(notification);
            if tx.send(message).await.is_err() {
                warn!("[Polygon] 通知发送失败（channel 已关闭）");
            }
        }

        Ok(())
    }

    //  ERC-20 代币转账收集
    async fn collect_token_transfers(
        &self,
        block_number: u64,
        timestamp: i64,
    ) -> Vec<TransferNotification> {
        let mut notifications = Vec::new();

        let block_hex = format!("0x{:x}", block_number);
        let base_filter = serde_json::json!({
            "fromBlock": block_hex,
            "toBlock": block_hex,
            "address": self.token_contracts.iter().collect::<Vec<_>>(),
        });

        // 两轮查询：from 方向 + to 方向
        let filter_from = {
            let mut f = base_filter.clone();
            f["topics"] = serde_json::json!([TRANSFER_EVENT_TOPIC, self.monitored_topics]);
            f
        };

        let filter_to = {
            let mut f = base_filter;
            f["topics"] =
                serde_json::json!([TRANSFER_EVENT_TOPIC, Value::Null, self.monitored_topics]);
            f
        };

        let mut unique_logs: HashMap<String, Value> = HashMap::new();

        for filter_params in [filter_from, filter_to] {
            let logs = match self.rpc.call("eth_getLogs", vec![filter_params]).await {
                Ok(v) => v,
                Err(e) => {
                    warn!("[Polygon] eth_getLogs 调用失败: {}", e);
                    continue;
                }
            };

            if let Some(arr) = logs.as_array() {
                for log in arr {
                    let tx_hash = log["transactionHash"].as_str().unwrap_or("");
                    let log_index = log["logIndex"].as_str().unwrap_or("0x0");
                    let key = format!("{}:{}", tx_hash, log_index);
                    unique_logs.entry(key).or_insert_with(|| log.clone());
                }
            }
        }

        for log in unique_logs.values() {
            let topics = match log.get("topics").and_then(|v| v.as_array()) {
                Some(arr) => arr,
                None => continue,
            };

            if topics.len() < 3
                || !topics[0]
                    .as_str()
                    .unwrap_or("")
                    .eq_ignore_ascii_case(TRANSFER_EVENT_TOPIC)
            {
                continue;
            }

            let contract_address =
                normalize_address(log["address"].as_str().unwrap_or("")).unwrap_or_default();

            if !self.token_contracts.contains(&contract_address) {
                continue;
            }

            let from_address = decode_topic_address(topics[1].as_str().unwrap_or(""));
            let to_address = decode_topic_address(topics[2].as_str().unwrap_or(""));
            let raw_amount = hex_to_u64(log["data"].as_str().unwrap_or("0x0"));
            let tx_hash = log["transactionHash"].as_str().unwrap_or("").to_string();
            // 跳过粉尘
            if raw_amount < 1000 {
                continue;
            }

            let (token_symbol, decimals) = self.get_token_metadata(&contract_address);
            let amount = format_token_amount(raw_amount as i64, decimals);

            let from_monitored = self.monitor_addresses.contains(&from_address);
            let to_monitored = self.monitor_addresses.contains(&to_address);

            if from_monitored {
                notifications.push(TransferNotification {
                    direction: "🔺传出".to_string(),
                    token_symbol: token_symbol.to_string(),
                    amount: amount.clone(),
                    from_address: from_address.clone(),
                    to_address: to_address.clone(),
                    tx_hash: tx_hash.clone(),
                    timestamp,
                });
            }

            if to_monitored && to_address != from_address {
                notifications.push(TransferNotification {
                    direction: "🟢接收".to_string(),
                    token_symbol: token_symbol.to_string(),
                    amount,
                    from_address,
                    to_address,
                    tx_hash,
                    timestamp,
                });
            }
        }

        notifications
    }

    //  RPC 调用

    async fn get_latest_block_number(&self) -> Result<u64, String> {
        let result = self.rpc.call("eth_blockNumber", vec![]).await?;
        let hex = result.as_str().unwrap_or("0x0");
        Ok(hex_to_u64(hex))
    }

    //  状态持久化

    async fn resolve_last_processed_block(&self) -> u64 {
        let latest = self.get_latest_block_number().await.unwrap_or(0);
        let target = latest.saturating_sub(self.confirmations);
        info!("[Polygon] 当前最新区块: {}, 起始高度: {}", latest, target);
        target
    }

    //  辅助方法

    fn get_token_metadata(&self, _contract_address: &str) -> (&'static str, u32) {
        ("USDT", 6)
    }

    fn format_message(&self, notification: &TransferNotification) -> String {
        format!(
            "Polygon Network\n\
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

pub fn start_monitor(
    rpc_url: &str,
    monitor_addresses: &str,
) -> Result<mpsc::Receiver<String>, String> {
    let monitor = PolygonMonitor::new(rpc_url.to_string(), monitor_addresses)?;
    let (tx, rx) = mpsc::channel::<String>(32);

    tokio::spawn(async move {
        monitor.run(tx).await;
    });

    Ok(rx)
}
