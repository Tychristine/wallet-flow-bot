use teloxide::utils::command::BotCommands;

/// 机器人支持的命令
#[derive(BotCommands, Clone)]
#[command(rename_rule = "lowercase")]
pub enum Command {
    #[command(description = "显示开始信息")]
    Start,
    #[command(description = "查询Tron资产")]
    Balance,
    #[command(description = "验证Tron地址")]
    Verify(String),
    #[command(description = "显示Tron地址")]
    Tron,
    #[command(description = "显示Polygon地址")]
    Pol,
}
