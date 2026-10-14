use serde::Deserialize;
use std::path::Path;
use std::sync::OnceLock;

/// USDT-TRC20 合约地址
const USDT_ADDRESS: &str = "TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t";

/**
 * 配置文件的序列化结构
 */
#[derive(Debug, Deserialize)]
struct FileConfig {
    rpc_url: Option<String>,
    api_key: Option<String>,
    target_address: Option<String>,
    bot_token: Option<String>,
    admin_id: Option<String>,
    polygon_rpc_url: Option<String>,
    polygon_wss_url: Option<String>,
    polygon_monitor_addresses: Option<String>,
}

/**
 * 运行时配置
 */
#[allow(non_snake_case)]
#[derive(Debug, Clone)]
pub struct Config {
    pub RPC_URL: String,
    pub API_KEY: String,
    pub TARGET_ADDRESS: String,
    pub USDT_ADDRESS: String,
    pub BOT_TOKEN: String,
    pub USERADMIN_ID: String,
    pub POLYGON_RPC_URL: String,
    pub POLYGON_WSS_URL: String,
    pub POLYGON_MONITOR_ADDRESSES: String,
}

/**
 * 从外部加载
 * 文件路径
 */
impl Config {
    pub fn load_from_file<P: AsRef<Path>>(path: P) -> Result<&'static Config, String> {
        // 读取文件
        let content: String = std::fs::read_to_string(path.as_ref())
            .map_err(|e| format!("无法读取配置文件 {}: {}", path.as_ref().display(), e))?;
        // 序列化解析
        let file_cfg: FileConfig =
            serde_json::from_str(&content).map_err(|e| format!("配置文件 JSON 解析失败: {}", e))?;

        // 逐字段校验
        let mut missing: Vec<&str> = Vec::new();

        let rpc_url = file_cfg.rpc_url.unwrap_or_default();
        if rpc_url.is_empty() {
            missing.push("rpc_url");
        }

        let api_key = file_cfg.api_key.unwrap_or_default();
        if api_key.is_empty() {
            missing.push("api_key");
        }

        let target_address = file_cfg.target_address.unwrap_or_default();
        if target_address.is_empty() {
            missing.push("target_address");
        }

        let bot_token = file_cfg.bot_token.unwrap_or_default();
        if bot_token.is_empty() {
            missing.push("bot_token");
        }

        let admin_id = file_cfg.admin_id.unwrap_or_default();
        if admin_id.is_empty() {
            missing.push("admin_id");
        }

        let polygon_rpc_url = file_cfg.polygon_rpc_url.unwrap_or_default();
        // 可选字段：配置后 Polygon 走 WSS 订阅，留空则沿用轮询
        let polygon_wss_url = file_cfg.polygon_wss_url.unwrap_or_default();
        let polygon_addrs = file_cfg.polygon_monitor_addresses.unwrap_or_default();
        // 缺配置异常抛出
        if !missing.is_empty() {
            return Err(format!(
                "配置文件缺少必填字段: {}\n请检查 {}",
                missing.join(", "),
                path.as_ref().display()
            ));
        }

        // 配置实例
        let config = Config {
            RPC_URL: rpc_url,
            API_KEY: api_key,
            TARGET_ADDRESS: target_address,
            USDT_ADDRESS: USDT_ADDRESS.to_string(),
            BOT_TOKEN: bot_token,
            USERADMIN_ID: admin_id,
            POLYGON_RPC_URL: polygon_rpc_url,
            POLYGON_WSS_URL: polygon_wss_url,
            POLYGON_MONITOR_ADDRESSES: polygon_addrs,
        };
        CONFIG
            .set(config)
            .map_err(|_| "配置已初始化，不能重复加载".to_string())?;

        Ok(Config::get())
    }

    // 获取全局配置的不可变引用
    pub fn get() -> &'static Config {
        CONFIG.get().expect("Config not initialized")
    }
}

static CONFIG: OnceLock<Config> = OnceLock::new();
