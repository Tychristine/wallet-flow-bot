use crate::interface::tron_interface::TronHttpClient;
use crate::utils::conf_util::Config;

/// 验证Tron地址是否有效
pub async fn verify_address(address: &str) -> Result<VerifyResult, String> {
    let config = Config::get();
    let client = TronHttpClient::new(config.RPC_URL.clone(), config.API_KEY.clone(), 10.0, 1, 0.5)?;

    let result = client
        .call(
            "wallet/validateaddress",
            Some(serde_json::json!({"address": address, "visible": true})),
        )
        .await?;

    Ok(VerifyResult {
        valid: result["result"].as_bool().unwrap_or(false),
        message: result["message"].as_str().unwrap_or("").to_string(),
    })
}

/// 地址验证结果
#[derive(Debug)]
pub struct VerifyResult {
    pub valid: bool,
    pub message: String,
}
