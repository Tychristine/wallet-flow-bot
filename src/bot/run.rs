use log::{error, info};
use teloxide::prelude::*;
use teloxide::types::Message;
use teloxide::utils::command::BotCommands;

use super::command::Command;
use super::handle;

/// 启动 Telegram Bot（使用外部传入的 Bot 实例，便于多任务共享）
pub async fn start_bot_with(bot: Bot) {
    // 注册命令列表到 Telegram，使聊天输入框显示命令提示
    if let Err(e) = bot.set_my_commands(Command::bot_commands()).await {
        error!("命令注册失败: {}", e);
    } else {
        info!("命令列表已注册");
    }

    info!("Bot 已启动，开始轮询消息...");

    teloxide::repl(bot, |msg: Message, bot: Bot| async move {
        if let Some(text) = msg.text()
            && text.starts_with('/')
        {
            match BotCommands::parse(text, "wallet-m") {
                Ok(Command::Start) => {
                    let _ = handle::handle_start(bot, msg).await;
                }
                Ok(Command::Balance) => {
                    let _ = handle::handle_balance(bot, msg).await;
                }
                Ok(Command::Verify(address)) => {
                    let _ = handle::handle_verify(bot, msg, address).await;
                }
                Ok(Command::Tron) => {
                    let _ = handle::handle_tron(bot, msg).await;
                }
                Ok(Command::Pol) => {
                    let _ = handle::handle_pol(bot, msg).await;
                }
                Err(_) => {
                    let _ = bot
                        .send_message(msg.chat.id, "未知命令，请使用 /start 查看可用命令")
                        .await;
                }
            }
        }
        Ok(())
    })
    .await;
}
