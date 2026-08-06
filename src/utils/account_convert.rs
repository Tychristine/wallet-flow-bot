use sha2::{Digest, Sha256};

// tron主网地址前缀 (0x41)
const TRON_PREFIX: u8 = 0x41;

// tron地址字节数 (1字节前缀 + 20字节哈希)
const ADDRESS_BYTES: usize = 21;

// 校验和字节数
const CHECKSUM_BYTES: usize = 4;

// Base58Check 完整载荷字节数 (21 + 4)
const FULL_PAYLOAD_BYTES: usize = ADDRESS_BYTES + CHECKSUM_BYTES;

//  错误类型

// 地址转换相关的错误类型
#[derive(Debug)]
pub enum AddressError {
    // Hex 字符串解码失败
    InvalidHex,
    // Base58 解码失败或校验和验证失败
    InvalidBase58,
    // 地址长度不是 21 字节
    InvalidLength,
    // 地址前缀不是 0x41
    InvalidPrefix,
    // 校验和不匹配
    ChecksumFailed,
}

impl std::fmt::Display for AddressError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidHex => write!(f, "无效的 Hex 编码"),
            Self::InvalidBase58 => write!(f, "无效的 Base58 编码或校验和验证失败"),
            Self::InvalidLength => write!(f, "地址长度错误，期望 21 字节"),
            Self::InvalidPrefix => write!(f, "地址前缀错误，期望 0x41 (tron主网)"),
            Self::ChecksumFailed => write!(f, "校验和不匹配"),
        }
    }
}

impl std::error::Error for AddressError {}

// 计算双重 SHA256 校验和的前 4 个字节
fn double_sha256_checksum(payload: &[u8]) -> [u8; CHECKSUM_BYTES] {
    let hash1 = Sha256::digest(payload);
    let hash2 = Sha256::digest(hash1);

    let mut checksum = [0u8; CHECKSUM_BYTES];
    checksum.copy_from_slice(&hash2[..CHECKSUM_BYTES]);
    checksum
}

// 验证解码后的tron地址：长度 21 字节，前缀 0x41
fn validate_tron_address(decoded: &[u8]) -> Result<(), AddressError> {
    if decoded.len() != ADDRESS_BYTES {
        return Err(AddressError::InvalidLength);
    }
    if decoded[0] != TRON_PREFIX {
        return Err(AddressError::InvalidPrefix);
    }
    Ok(())
}

/**
 * 公开接口
 */
// Base58Check 地址 (T...) 转为 Hex 地址 (41...)
pub fn base58_to_hex(address_b58: &str) -> Result<String, AddressError> {
    // 解码 Base58 得到 25 字节原始数据
    let decoded = bs58::decode(address_b58)
        .into_vec()
        .map_err(|_| AddressError::InvalidBase58)?;

    // 长度校验：必须为 25 字节 (21 字节地址 + 4 字节校验和)
    if decoded.len() != FULL_PAYLOAD_BYTES {
        return Err(AddressError::InvalidLength);
    }

    // 分离地址载荷和校验和
    let payload = &decoded[..ADDRESS_BYTES];
    let checksum = &decoded[ADDRESS_BYTES..];

    // 验证双重 SHA256 校验和
    let expected_checksum = double_sha256_checksum(payload);
    if checksum != expected_checksum {
        return Err(AddressError::ChecksumFailed);
    }

    // 验证tron地址格式 (前缀 0x41)
    validate_tron_address(payload)?;

    Ok(hex::encode(payload))
}

// Hex 地址 (41...) 转为 Base58Check 地址 (T...)
pub fn hex_to_base58(address_hex: &str) -> Result<String, AddressError> {
    // 去除可能存在的 0x 前缀
    let clean_hex = address_hex.trim_start_matches("0x");
    let decoded_hex = hex::decode(clean_hex).map_err(|_| AddressError::InvalidHex)?;

    // 验证tron地址格式
    validate_tron_address(&decoded_hex)?;

    // 计算双重 SHA256 校验和
    let checksum = double_sha256_checksum(&decoded_hex);

    // 使用栈上数组拼接：21 字节数据 + 4 字节校验和 = 25 字节
    let mut full_payload = [0u8; FULL_PAYLOAD_BYTES];
    full_payload[..ADDRESS_BYTES].copy_from_slice(&decoded_hex);
    full_payload[ADDRESS_BYTES..].copy_from_slice(&checksum);

    // 进行 Base58 编码
    Ok(bs58::encode(full_payload).into_string())
}

/// 规范化 Tron 地址：无论输入是 Base58（T 开头）还是 Hex，统一返回 Base58 格式
pub fn normalize_tron_address(address: &str) -> Result<String, AddressError> {
    let trimmed: &str = address.trim();
    if trimmed.is_empty() {
        return Err(AddressError::InvalidBase58);
    }
    if trimmed.starts_with('T') {
        // 是b58则原地返回
        Ok(trimmed.to_string())
    } else {
        // 是hex格式则调用转换
        hex_to_base58(trimmed)
    }
}

/// 从事件日志 topic 中解码地址（topic 后 40 位 hex + 41 前缀 → Base58）
pub fn decode_topic_address(topic: &str) -> Result<String, AddressError> {
    let normalized = topic.trim().to_lowercase().replace("0x", "");
    // topic 是 64 位 hex 字符串，取最后 40 位（即 20 字节地址）
    if normalized.len() < 40 {
        return Err(AddressError::InvalidHex);
    }
    let addr_hex = format!("41{}", &normalized[normalized.len() - 40..]);
    hex_to_base58(&addr_hex)
}

/// 从日志记录中的合约地址字段解码 Base58 地址
pub fn decode_log_address(address: &str) -> Result<String, AddressError> {
    let mut normalized = address.trim().to_lowercase().replace("0x", "");
    // 若为 40 位 hex（缺少 41 前缀），自动补齐
    if normalized.len() == 40 {
        normalized = format!("41{}", normalized);
    }
    hex_to_base58(&normalized)
}
