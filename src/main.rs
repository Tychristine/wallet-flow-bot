use log::{debug, error, info, warn};
use mimalloc::MiMalloc;
use std::env;
use std::process;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;
use teloxide::Bot;
use wallet_flow_bot::bot::handle;
use wallet_flow_bot::bot::run::start_bot_with;
use wallet_flow_bot::service::polygon_service;
use wallet_flow_bot::service::tron_protocol_service;
use wallet_flow_bot::utils::conf_util;

#[tokio::main]
async fn main() {
    // 0.初始化日志（可通过 RUST_LOG 环境变量控制级别，默认 info）
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // 1. 解析命令行参数配置路径
    let config_path: String = parse_config_path();

    // 2. 从外部 JSON 文件加载配置，无配置直接退出
    match conf_util::Config::load_from_file(&config_path) {
        Ok(_) => info!("配置已从 {} 加载", config_path),
        Err(e) => {
            error!("{}", e);
            process::exit(1);
        }
    }
    // 获得配置对象
    let config: &conf_util::Config = conf_util::Config::get();
    info!("Wallet-M Bot 正在启动...");

    // 3. 创建共享的 Bot 实例
    let bot: Bot = Bot::new(&config.BOT_TOKEN);

    // 4. 启动 Tron 监听服务
    let mut tron_rx = tron_protocol_service::start_monitor().expect("Tron 监听服务启动失败");
    let tron_bot = bot.clone();
    tokio::spawn(async move {
        info!("Tron 动账监听已启动");
        while let Some(msg) = tron_rx.recv().await {
            debug!("[Tron] 推送消息: {}", msg.lines().next().unwrap_or(""));
            if let Err(e) = handle::send_to_admin(&tron_bot, msg).await {
                error!("Tron 推送失败: {}", e);
            }
        }
        warn!("Tron 监听 channel 已关闭");
    });

    // 5. 启动 Polygon 监听服务
    if !config.POLYGON_RPC_URL.is_empty() && !config.POLYGON_MONITOR_ADDRESSES.is_empty() {
        let mut polygon_rx = polygon_service::start_monitor(
            &config.POLYGON_RPC_URL,
            &config.POLYGON_WSS_URL,
            &config.POLYGON_MONITOR_ADDRESSES,
        )
        .expect("Polygon 监听服务启动失败");
        let polygon_bot = bot.clone();
        tokio::spawn(async move {
            info!("Polygon 动账监听已启动");
            while let Some(msg) = polygon_rx.recv().await {
                debug!("[Polygon] 推送消息: {}", msg.lines().next().unwrap_or(""));
                if let Err(e) = handle::send_to_admin(&polygon_bot, msg).await {
                    error!("Polygon 推送失败: {}", e);
                }
            }
            warn!("Polygon 监听 channel 已关闭");
        });
    } else {
        info!("未配置 Polygon 监听（缺少 polygon_rpc_url 或 polygon_monitor_addresses）");
    }

    // 6. 启动 Bot 实例
    start_bot_with(bot).await;
}

/**
 * 解析命令行参数，返回配置文件路径，默认 "config.json"
 */
fn parse_config_path() -> String {
    let args: Vec<String> = env::args().collect();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--c" | "-c" => {
                if i + 1 < args.len() {
                    return args[i + 1].clone();
                } else {
                    error!("--c 需要指定配置文件路径");
                    process::exit(1);
                }
            }
            _ => {}
        }
        i += 1;
    }
    // 默认值
    "config.json".to_string()
}
