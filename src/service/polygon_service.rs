use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use log::{debug, error, info, warn};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::time::{sleep, timeout};
use tokio_tungstenite::tungstenite::Message;

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

// 轮询频率4.0 seconds/次（仅轮询模式使用）
const DEFAULT_POLL_INTERVAL_SECS: f64 = 4.0;
const DEFAULT_CONFIRMATIONS: u64 = 1;
const DEFAULT_RPC_TIMEOUT_SECS: f64 = 15.0;
const DEFAULT_RPC_RETRIES: u32 = 2;
const DEFAULT_RPC_RETRY_DELAY_SECS: f64 = 1.0;

// ===== WSS 订阅参数 =====
/// 重连最小退避(秒)
const WSS_RECONNECT_MIN_SECS: u64 = 1;
/// 重连最大退避(秒)
const WSS_RECONNECT_MAX_SECS: u64 = 30;
/// 等待 eth_subscribe 确认的超时(秒)
const WSS_SUBSCRIBE_TIMEOUT_SECS: u64 = 20;
/// 订阅空闲超过该秒数发送 ping 保活(秒)
const WSS_IDLE_PING_SECS: u64 = 15;
/// 首次订阅连续失败多少次后降级为轮询
const WSS_FALLBACK_ATTEMPTS: u32 = 3;
/// 日志去重窗口大小
const DEDUP_WINDOW: usize = 4096;
/// 区块时间戳缓存上限
const BLOCK_TS_CACHE: usize = 512;

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

/// 有界的最近日志去重窗口（key = txHash:logIndex，FIFO 淘汰）
struct RecentLogs {
    set: HashSet<String>,
    order: VecDeque<String>,
    cap: usize,
}

impl RecentLogs {
    fn new(cap: usize) -> Self {
        Self {
            set: HashSet::new(),
            order: VecDeque::new(),
            cap,
        }
    }

    /// 首次见到该日志返回 true
    fn insert(&mut self, key: String) -> bool {
        if !self.set.insert(key.clone()) {
            return false;
        }
        self.order.push_back(key);
        while self.order.len() > self.cap {
            if let Some(old) = self.order.pop_front() {
                self.set.remove(&old);
            }
        }
        true
    }
}

/// 通知时间戳来源
#[derive(Clone, Copy)]
enum TimestampSource {
    /// 本地接收时间（与轮询行为一致，不产生额外请求）
    Local,
    /// 链上区块时间（回补历史转账时更准确，需要一次 eth_getBlockByNumber）
    Block,
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

    //  ERC-20 转账日志：过滤与解析（轮询、订阅共用）

    /// eth_getLogs / eth_subscribe 使用的两个过滤条件：from 方向、to 方向
    fn log_filters(&self) -> [Value; 2] {
        let filter_from = json!({
            "address": self.token_contracts.iter().collect::<Vec<_>>(),
            "topics": [TRANSFER_EVENT_TOPIC, self.monitored_topics],
        });
        let filter_to = json!({
            "address": self.token_contracts.iter().collect::<Vec<_>>(),
            "topics": [TRANSFER_EVENT_TOPIC, Value::Null, self.monitored_topics],
        });
        [filter_from, filter_to]
    }

    /// 日志去重 key（与旧轮询逻辑保持一致）
    fn log_key(log: &Value) -> String {
        format!(
            "{}:{}",
            log["transactionHash"].as_str().unwrap_or(""),
            log["logIndex"].as_str().unwrap_or("0x0")
        )
    }

    /// 单条 Transfer 日志 → 通知（0~2 条：出账 / 入账）
    fn decode_transfer_log(&self, log: &Value, timestamp: i64) -> Vec<TransferNotification> {
        let mut notifications = Vec::new();

        let topics = match log.get("topics").and_then(|v| v.as_array()) {
            Some(arr) => arr,
            None => return notifications,
        };

        if topics.len() < 3
            || !topics[0]
                .as_str()
                .unwrap_or("")
                .eq_ignore_ascii_case(TRANSFER_EVENT_TOPIC)
        {
            return notifications;
        }

        let contract_address =
            normalize_address(log["address"].as_str().unwrap_or("")).unwrap_or_default();

        if !self.token_contracts.contains(&contract_address) {
            return notifications;
        }

        let from_address = decode_topic_address(topics[1].as_str().unwrap_or(""));
        let to_address = decode_topic_address(topics[2].as_str().unwrap_or(""));
        let raw_amount = hex_to_u64(log["data"].as_str().unwrap_or("0x0"));
        // 跳过粉尘
        if raw_amount < 1000 {
            return notifications;
        }

        let tx_hash = log["transactionHash"].as_str().unwrap_or("").to_string();
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

        notifications
    }

    /// 拉取 [from_block, to_block] 区间内与监控地址相关的唯一日志（key = txHash:logIndex）
    async fn fetch_unique_logs(&self, from_block: u64, to_block: u64) -> HashMap<String, Value> {
        let mut unique_logs: HashMap<String, Value> = HashMap::new();

        // 两轮查询：from 方向 + to 方向
        for mut filter in self.log_filters() {
            filter["fromBlock"] = json!(format!("0x{:x}", from_block));
            filter["toBlock"] = json!(format!("0x{:x}", to_block));

            let logs = match self.rpc.call("eth_getLogs", vec![filter]).await {
                Ok(v) => v,
                Err(e) => {
                    warn!("[Polygon] eth_getLogs 调用失败: {}", e);
                    continue;
                }
            };

            if let Some(arr) = logs.as_array() {
                for log in arr {
                    unique_logs
                        .entry(Self::log_key(log))
                        .or_insert_with(|| log.clone());
                }
            }
        }

        unique_logs
    }

    /// 轮询路径：单个区块内的关联转账
    async fn collect_token_transfers(
        &self,
        block_number: u64,
        timestamp: i64,
    ) -> Vec<TransferNotification> {
        self.fetch_unique_logs(block_number, block_number)
            .await
            .values()
            .flat_map(|log| self.decode_transfer_log(log, timestamp))
            .collect()
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

    //  WSS 订阅路径

    /// 区块时间戳（带缓存；失败回退本地时间）
    async fn resolve_block_timestamp(
        &self,
        block_number: u64,
        cache: &mut HashMap<u64, i64>,
    ) -> i64 {
        if let Some(ts) = cache.get(&block_number) {
            return *ts;
        }

        let ts = match self
            .rpc
            .call(
                "eth_getBlockByNumber",
                vec![json!(format!("0x{:x}", block_number)), json!(false)],
            )
            .await
        {
            Ok(v) => v["timestamp"]
                .as_str()
                .and_then(|s| i64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
                .unwrap_or(0),
            Err(e) => {
                warn!("[Polygon] 获取区块 {} 时间失败: {}", block_number, e);
                0
            }
        };

        let ts = if ts > 0 { ts } else { Utc::now().timestamp() };
        if cache.len() >= BLOCK_TS_CACHE {
            cache.clear();
        }
        cache.insert(block_number, ts);
        ts
    }

    /// 处理一批日志：排序 → 去重 → 解析 → 推送，返回处理到的最大区块号
    async fn dispatch_logs(
        &self,
        logs: Vec<Value>,
        source: TimestampSource,
        dedup: &mut RecentLogs,
        ts_cache: &mut HashMap<u64, i64>,
        tx: &mpsc::Sender<String>,
    ) -> Option<u64> {
        let mut logs = logs;
        // 按 (区块号, logIndex) 排序，保证回补时通知顺序与链上一致
        logs.sort_by_key(|log| {
            (
                hex_to_u64(log["blockNumber"].as_str().unwrap_or("0x0")),
                hex_to_u64(log["logIndex"].as_str().unwrap_or("0x0")),
            )
        });

        let mut max_block: Option<u64> = None;

        for log in &logs {
            let block_number = hex_to_u64(log["blockNumber"].as_str().unwrap_or("0x0"));

            // 链重组：旧链上的日志会带 removed 标记
            if log["removed"].as_bool().unwrap_or(false) {
                debug!("[Polygon] 忽略重组日志 block={}", block_number);
                continue;
            }

            // 订阅推送与断线回补可能投递同一笔
            if !dedup.insert(Self::log_key(log)) {
                continue;
            }

            max_block = Some(max_block.map_or(block_number, |m: u64| m.max(block_number)));

            let timestamp = match source {
                // 实时推送：本地接收时间即可（与轮询路径一致，且不额外发请求）
                TimestampSource::Local => Utc::now().timestamp(),
                // 回补历史转账：用链上区块时间，避免显示成回补时刻
                TimestampSource::Block => {
                    self.resolve_block_timestamp(block_number, ts_cache).await
                }
            };
            for notification in self.decode_transfer_log(log, timestamp) {
                debug!(
                    "[Polygon] 通知: {} {} {} tx={}",
                    notification.direction,
                    notification.token_symbol,
                    notification.amount,
                    notification.tx_hash
                );
                let message = self.format_message(&notification);
                if tx.send(message).await.is_err() {
                    warn!("[Polygon] 通知发送失败（channel 已关闭）");
                    return max_block;
                }
            }
        }

        max_block
    }

    /// 回补 [from_block, to_block] 区间的转账
    async fn backfill(
        &self,
        from_block: u64,
        to_block: u64,
        dedup: &mut RecentLogs,
        ts_cache: &mut HashMap<u64, i64>,
        tx: &mpsc::Sender<String>,
    ) -> Option<u64> {
        if to_block < from_block {
            return None;
        }

        info!("[Polygon] 回补空窗: 区块 {} → {}", from_block, to_block);
        let logs: Vec<Value> = self
            .fetch_unique_logs(from_block, to_block)
            .await
            .into_values()
            .collect();
        self.dispatch_logs(logs, TimestampSource::Block, dedup, ts_cache, tx)
            .await
    }

    /// 建立一次订阅并持续消费推送，连接结束或出错时返回
    async fn subscribe_once(
        &self,
        wss_url: &str,
        last_processed: &mut u64,
        dedup: &mut RecentLogs,
        ts_cache: &mut HashMap<u64, i64>,
        tx: &mpsc::Sender<String>,
    ) -> Result<(), String> {
        let (mut ws, _) = tokio_tungstenite::connect_async(wss_url)
            .await
            .map_err(|e| format!("连接 {} 失败: {}", wss_url, e))?;
        info!("[Polygon] WSS 已连接: {}", wss_url);

        // 订阅 from / to 两个方向
        let filters = self.log_filters();
        for (idx, filter) in filters.iter().enumerate() {
            let request = json!({
                "jsonrpc": "2.0",
                "id": idx + 1,
                "method": "eth_subscribe",
                "params": ["logs", filter],
            });
            ws.send(Message::Text(request.to_string().into()))
                .await
                .map_err(|e| format!("发送订阅请求失败: {}", e))?;
        }

        // 等待订阅确认；服务商不支持 eth_subscribe 时会返回 error
        let deadline =
            tokio::time::Instant::now() + Duration::from_secs(WSS_SUBSCRIBE_TIMEOUT_SECS);
        let mut confirmed = 0usize;
        while confirmed < filters.len() {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err("等待订阅确认超时".to_string());
            }

            match timeout(remaining, ws.next()).await {
                Ok(Some(Ok(Message::Text(text)))) => {
                    let msg: Value = serde_json::from_str(&text)
                        .map_err(|e| format!("订阅响应解析失败: {}", e))?;
                    if let Some(err) = msg.get("error") {
                        return Err(format!("订阅被拒绝: {}", err));
                    }
                    if msg.get("id").is_some() && msg.get("result").is_some() {
                        confirmed += 1;
                    }
                }
                Ok(Some(Ok(Message::Close(frame)))) => {
                    return Err(format!("连接被关闭: {:?}", frame));
                }
                Ok(Some(Ok(_))) => {}
                Ok(Some(Err(e))) => return Err(format!("读取订阅响应失败: {}", e)),
                Ok(None) => return Err("连接已结束".to_string()),
                Err(_) => return Err("等待订阅确认超时".to_string()),
            }
        }
        info!("[Polygon] WSS 订阅成功（{} 个过滤器）", confirmed);

        // 订阅建立后再补一次空窗，覆盖连接建立期间产生的新块
        match self.get_latest_block_number().await {
            Ok(head) => {
                let target = head.saturating_sub(self.confirmations);
                if target > *last_processed {
                    if let Some(max_block) = self
                        .backfill(*last_processed + 1, target, dedup, ts_cache, tx)
                        .await
                    {
                        *last_processed = (*last_processed).max(max_block);
                    }
                    *last_processed = (*last_processed).max(target);
                }
            }
            Err(e) => warn!("[Polygon] 获取最新区块失败（跳过启动回补）: {}", e),
        }

        // 持续消费推送
        loop {
            match timeout(Duration::from_secs(WSS_IDLE_PING_SECS), ws.next()).await {
                Ok(Some(Ok(Message::Text(text)))) => {
                    let msg: Value = match serde_json::from_str(&text) {
                        Ok(v) => v,
                        Err(e) => {
                            warn!("[Polygon] 推送消息解析失败: {}", e);
                            continue;
                        }
                    };

                    if msg["method"].as_str() != Some("eth_subscription") {
                        continue;
                    }
                    let log = msg["params"]["result"].clone();
                    if log.is_null() {
                        continue;
                    }

                    if let Some(max_block) = self
                        .dispatch_logs(vec![log], TimestampSource::Local, dedup, ts_cache, tx)
                        .await
                    {
                        *last_processed = (*last_processed).max(max_block);
                    }
                }
                Ok(Some(Ok(Message::Ping(payload)))) => {
                    let _ = ws.send(Message::Pong(payload)).await;
                }
                Ok(Some(Ok(Message::Close(frame)))) => {
                    return Err(format!("连接被服务端关闭: {:?}", frame));
                }
                Ok(Some(Ok(_))) => {}
                Ok(Some(Err(e))) => return Err(format!("读取推送失败: {}", e)),
                Ok(None) => return Err("连接已结束".to_string()),
                // 空闲保活：维持连接，同时探测连接是否已失效
                Err(_) => {
                    ws.send(Message::Ping(Vec::new().into()))
                        .await
                        .map_err(|e| format!("发送保活 ping 失败: {}", e))?;
                }
            }
        }
    }

    /// 订阅主循环；返回 true 表示订阅不可用，应降级为轮询
    async fn subscribe_forever(&self, wss_url: &str, tx: &mpsc::Sender<String>) -> bool {
        let mut last_processed = self.resolve_last_processed_block().await;
        let mut dedup = RecentLogs::new(DEDUP_WINDOW);
        let mut ts_cache: HashMap<u64, i64> = HashMap::new();
        let mut backoff = WSS_RECONNECT_MIN_SECS;
        let mut failed_attempts: u32 = 0;
        let mut ever_subscribed = false;

        loop {
            match self
                .subscribe_once(wss_url, &mut last_processed, &mut dedup, &mut ts_cache, tx)
                .await
            {
                Ok(()) => {
                    warn!("[Polygon] WSS 连接结束，准备重连");
                    ever_subscribed = true;
                    failed_attempts = 0;
                    backoff = WSS_RECONNECT_MIN_SECS;
                }
                Err(e) => {
                    warn!("[Polygon] WSS 订阅失败: {}", e);
                    failed_attempts += 1;
                    if !ever_subscribed && failed_attempts >= WSS_FALLBACK_ATTEMPTS {
                        return true;
                    }
                }
            }

            // 断线期间的空窗回补
            match self.get_latest_block_number().await {
                Ok(head) => {
                    let target = head.saturating_sub(self.confirmations);
                    if target > last_processed {
                        if let Some(max_block) = self
                            .backfill(last_processed + 1, target, &mut dedup, &mut ts_cache, tx)
                            .await
                        {
                            last_processed = last_processed.max(max_block);
                        }
                        last_processed = last_processed.max(target);
                    }
                }
                Err(e) => warn!("[Polygon] 获取最新区块失败: {}", e),
            }

            info!("[Polygon] {} 秒后重连 WSS", backoff);
            sleep(Duration::from_secs(backoff)).await;
            backoff = (backoff * 2).min(WSS_RECONNECT_MAX_SECS);
        }
    }

    /// WSS 订阅模式入口；订阅不可用时自动降级为轮询
    pub async fn run_with_wss(self, wss_url: String, tx: mpsc::Sender<String>) {
        info!("[Polygon] 监听模式: WSS 订阅");
        if self.subscribe_forever(&wss_url, &tx).await {
            warn!(
                "[Polygon] 连续 {} 次订阅失败，降级为轮询模式",
                WSS_FALLBACK_ATTEMPTS
            );
            self.run(tx).await;
        }
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

/// 启动 Polygon 监听服务
///
/// `wss_url` 非空时使用 WSS 订阅（连续失败会自动降级为轮询），为空时沿用轮询
pub fn start_monitor(
    rpc_url: &str,
    wss_url: &str,
    monitor_addresses: &str,
) -> Result<mpsc::Receiver<String>, String> {
    let monitor = PolygonMonitor::new(rpc_url.to_string(), monitor_addresses)?;
    let (tx, rx) = mpsc::channel::<String>(32);
    let wss_url = wss_url.trim().to_string();

    tokio::spawn(async move {
        if wss_url.is_empty() {
            info!("[Polygon] 未配置 polygon_wss_url，使用轮询模式");
            monitor.run(tx).await;
        } else {
            monitor.run_with_wss(wss_url, tx).await;
        }
    });

    Ok(rx)
}
