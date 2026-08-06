use crate::interface::account_interface::fetch_accountinfo;
use crate::utils::conf_util::Config;

/// 获取余额，返回TRX余额，USDT余额
pub async fn get_balance() -> Result<(i64, String), String> {
    let json = fetch_accountinfo()
        .await
        .map_err(|e| format!("请求失败: {}", e))?;

    parse_balance_response(json)
}

/// 接口/v1/accounts/{address} 响应，返回 (TRX余额sun, USDT余额字符串)
fn parse_balance_response(json: serde_json::Value) -> Result<(i64, String), String> {
    let data = json["data"][0].clone();
    let balance: i64 = data["balance"].as_i64().unwrap_or(0);
    let usdt_balance = data["trc20"]
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|obj| obj.get(&Config::get().USDT_ADDRESS))
        .and_then(|v| v.as_str())
        .unwrap_or("0")
        .to_string();
    Ok((balance, usdt_balance))
}
