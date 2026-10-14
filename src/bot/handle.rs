use teloxide::Bot;
use teloxide::payloads::SendMessageSetters;
use teloxide::prelude::Requester;
use teloxide::sugar::request::RequestLinkPreviewExt;
use teloxide::types::{ChatId, Message, ParseMode};

use crate::interface::account_interface;
use crate::utils::account_convert::{base58_to_hex, hex_to_base58};
use crate::utils::conf_util::Config;
use crate::utils::warpper::markdown_v2_escape;

// 处理 /start 命令
pub async fn handle_start(
    bot: Bot,
    msg: Message,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    bot.send_message(
        msg.chat.id,
        "欢迎使用！\n\
         可用命令:\n\
         /verify <地址> - 验证Tron地址\n\
         /tron - 显示Tron地址\n\
         /pol - 显示Polygon地址",
    )
    .await?;
    Ok(())
}

// 处理 /verify <地址> 命令
pub async fn handle_verify(
    bot: Bot,
    msg: Message,
    address: String,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let address = address.trim().to_string();
    if address.is_empty() {
        bot.send_message(msg.chat.id, "请提供要验证的地址，例如: `/verify TX115514`")
            .parse_mode(ParseMode::MarkdownV2)
            .await?;
        return Ok(());
    }

    // 调用 Tron API 验证
    let result = account_interface::verify_address(&address).await;

    match result {
        Ok(r) if r.valid => {
            // 尝试归一化为 Base58 + Hex 双格式展示
            let (base58, hex) = if address.starts_with('T') {
                let h = base58_to_hex(&address)
                    .map(|h| format!("`{}`", h))
                    .unwrap_or_else(|_| "转换失败".to_string());
                (format!("`{}`", address), h)
            } else {
                let b = hex_to_base58(&address)
                    .map(|b| format!("`{}`", b))
                    .unwrap_or_else(|_| "转换失败".to_string());
                (b, format!("`{}`", address))
            };

            let reply = format!(
                "✅ *地址有效*\n\
                 Base58: {}\n\
                 Hex: {}\n\
                 {}",
                base58,
                hex,
                markdown_v2_escape(&r.message)
            );
            bot.send_message(msg.chat.id, reply)
                .parse_mode(ParseMode::MarkdownV2)
                .await?;
        }
        Ok(r) => {
            bot.send_message(
                msg.chat.id,
                format!(
                    "❌ *地址无效*\n`{}`\n{}",
                    markdown_v2_escape(&address),
                    markdown_v2_escape(&r.message)
                ),
            )
            .parse_mode(ParseMode::MarkdownV2)
            .await?;
        }
        Err(e) => {
            bot.send_message(msg.chat.id, format!("⚠️ 验证请求失败: {}", e))
                .await?;
        }
    }

    Ok(())
}

// 处理 /tron 命令，非管理员驳回
pub async fn handle_tron(
    bot: Bot,
    msg: Message,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let config = Config::get();
    let admin_id = config.USERADMIN_ID.parse::<i64>().unwrap_or(0);
    if msg.chat.id.0 != admin_id {
        bot.send_message(msg.chat.id, "⛔ Permission Denied")
            .await?;
        return Ok(());
    }

    let reply = format!("🔴 *TRON 地址*\n`{}`", config.TARGET_ADDRESS);
    bot.send_message(msg.chat.id, reply)
        .parse_mode(ParseMode::MarkdownV2)
        .await?;
    Ok(())
}

// 处理 /pol 命令，非管理员驳回
pub async fn handle_pol(
    bot: Bot,
    msg: Message,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let config = Config::get();
    let admin_id = config.USERADMIN_ID.parse::<i64>().unwrap_or(0);
    if msg.chat.id.0 != admin_id {
        bot.send_message(msg.chat.id, "⛔ Permission Denied")
            .await?;
        return Ok(());
    }

    let reply = format!("🟣 *Polygon 地址*\n`{}`", config.POLYGON_MONITOR_ADDRESSES);
    bot.send_message(msg.chat.id, reply)
        .parse_mode(ParseMode::MarkdownV2)
        .await?;
    Ok(())
}

/// 向管理员发送消息，失败自动重试 3 次
pub async fn send_to_admin(
    bot: &Bot,
    message: String,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let config = Config::get();
    let chat_id = config
        .USERADMIN_ID
        .parse::<i64>()
        .map(ChatId)
        .map_err(|_| format!("无效的管理员 ID: {}", config.USERADMIN_ID))?;

    let mut last_err = String::new();
    for attempt in 0..3 {
        match bot
            .send_message(chat_id, message.clone())
            .parse_mode(ParseMode::MarkdownV2)
            .disable_link_preview(true)
            .await
        {
            Ok(_) => return Ok(()),
            Err(e) => {
                last_err = e.to_string();
                if attempt < 2 {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
            }
        }
    }

    Err(format!("发送失败（已重试3次）: {}", last_err).into())
}
