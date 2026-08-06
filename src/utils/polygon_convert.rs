/// Polygon 地址规范化：小写 + 验证 0x 前缀 + 42 字符长度
pub fn normalize_address(address: &str) -> Result<String, String> {
    let normalized = address.trim().to_lowercase();
    if !normalized.starts_with("0x") || normalized.len() != 42 {
        return Err(format!("无效的 Polygon 地址: {}", address));
    }
    // 验证是合法 hex
    if !normalized[2..].chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("地址包含非法 hex 字符: {}", address));
    }
    Ok(normalized)
}

/// 16 进制字符串 → u64
pub fn hex_to_u64(hex: &str) -> u64 {
    let cleaned = hex.trim_start_matches("0x");
    if cleaned.is_empty() {
        return 0;
    }
    u64::from_str_radix(cleaned, 16).unwrap_or(0)
}

/// 将地址转换为 32 字节 topic（用于 eth_getLogs 过滤）
/// 输出: 0x000000000000000000000000<地址去掉0x>
/// 注意：调用前 address 应已通过 normalize_address 规范化
pub fn topic_for_address(address: &str) -> String {
    format!(
        "0x000000000000000000000000{}",
        address.trim_start_matches("0x")
    )
}

/// 从事件日志 topic 中解码地址（64 位 hex 的后 40 位 → 0x...）
pub fn decode_topic_address(topic: &str) -> String {
    let hex = topic.trim_start_matches("0x").to_lowercase();
    let start = hex.len().saturating_sub(40);
    format!("0x{}", &hex[start..])
}
