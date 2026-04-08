use crate::context::AppContext;
use crate::utils::{f_num, format_short_wallet};
use kaspa_addresses::Address;
use kaspa_rpc_core::api::rpc::RpcApi;
use reqwest::{multipart, Client};
use serde_json::{json, Value};
use teloxide::net::Download;
use teloxide::prelude::*;
use tokio::fs::File;
use tokio::io::AsyncWriteExt;

const OPENAI_API_URL: &str = "https://api.openai.com/v1/chat/completions";
const WHISPER_API_URL: &str = "https://api.openai.com/v1/audio/transcriptions";

pub async fn process_voice_message(bot: Bot, msg: Message, ctx: AppContext) -> anyhow::Result<()> {
    let api_key = match std::env::var("OPENAI_API_KEY") {
        Ok(k) if !k.is_empty() => k,
        _ => {
            let _ = bot.send_message(msg.chat.id, "ÃƒÂ°Ã…Â¸Ã…Â½Ã¢â€žÂ¢ÃƒÂ¯Ã‚Â¸Ã‚Â <b>Audio Detected:</b> Voice analysis is currently disabled (Missing API Key). Please use text.").parse_mode(teloxide::types::ParseMode::Html).await;
            return Ok(());
        }
    };

    let voice = match msg.voice() {
        Some(v) => v,
        None => return Ok(()),
    };

    let _ = bot
        .send_message(
            msg.chat.id,
            "ÃƒÂ°Ã…Â¸Ã…Â½Ã‚Â§ <i>Listening and processing your voice note...</i>",
        )
        .parse_mode(teloxide::types::ParseMode::Html)
        .await;

    // Download Voice from Telegram
    let file = bot.get_file(voice.file.id.clone()).await?;
    let temp_path = format!("temp_{}.ogg", msg.id.0);
    let mut local_file = File::create(&temp_path).await?;
    bot.download_file(&file.path, &mut local_file).await?;
    local_file.flush().await?;

    // Send to Whisper API for transcription
    let client = Client::new();
    let file_bytes = tokio::fs::read(&temp_path).await?;
    let part = multipart::Part::bytes(file_bytes)
        .file_name("voice.ogg")
        .mime_str("audio/ogg")?;
    let form = multipart::Form::new()
        .part("file", part)
        .text("model", "whisper-1")
        .text("language", "ar"); // Support Arabic & English

    let res = client
        .post(WHISPER_API_URL)
        .bearer_auth(&api_key)
        .multipart(form)
        .send()
        .await?;

    let _ = tokio::fs::remove_file(&temp_path).await; // Clean up temp file

    if let Ok(json_resp) = res.json::<Value>().await {
        if let Some(text) = json_resp.get("text").and_then(|t| t.as_str()) {
            // Pass transcribed text to the Intent engine
            return process_conversational_intent(
                bot,
                msg.chat.id,
                msg.from.as_ref().map(|u| u.id.0 as i64).unwrap_or(0),
                text.to_string(),
                ctx,
            )
            .await;
        }
    }

    let _ = bot
        .send_message(
            msg.chat.id,
            "ÃƒÂ¢Ã…Â¡Ã‚Â ÃƒÂ¯Ã‚Â¸Ã‚Â Sorry, I could not transcribe the audio clearly. Please try again.",
        )
        .await;
    Ok(())
}

pub async fn process_conversational_intent(
    bot: Bot,
    chat_id: ChatId,
    _user_id: i64,
    user_text: String,
    ctx: AppContext,
) -> anyhow::Result<()> {
    let api_key = match std::env::var("OPENAI_API_KEY") {
        Ok(k) if !k.is_empty() => k,
        _ => {
            // Graceful Degradation: Fallback to Heuristic NLP if no AI API key is configured
            return crate::handlers::fallback_heuristic_text(bot, chat_id, &user_text, ctx).await;
        }
    };

    let _ = bot
        .send_chat_action(chat_id, teloxide::types::ChatAction::Typing)
        .await;

    let client = Client::new();

    // Define the Rust Tools (Function Calling Schema)
    let tools = json!([
        {
            "type": "function",
            "function": {
                "name": "get_wallet_balance",
                "description": "Checks the current Kaspa balance and UTXO count for the user's tracked wallets.",
                "parameters": { "type": "object", "properties": {}, "required": [] }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "get_network_stats",
                "description": "Gets Kaspa node sync status, DAA score, and total circulating supply.",
                "parameters": { "type": "object", "properties": {}, "required": [] }
            }
        }
    ]);

    let system_prompt = "You are the Kaspa Solo Enterprise AI. You are a highly professional, expert assistant for cryptocurrency solo miners. You analyze intents and call the appropriate functions to get real-time node data, then formulate a friendly, concise, and accurate response based on the function results. You speak Arabic and English fluently depending on the user's input.";

    let messages = json!([
        { "role": "system", "content": system_prompt },
        { "role": "user", "content": user_text }
    ]);

    // 1st API Call: Ask AI what to do
    let req_body = json!({
        "model": "gpt-4o-mini",
        "messages": messages,
        "tools": tools,
        "tool_choice": "auto"
    });

    let res: Value = client
        .post(OPENAI_API_URL)
        .bearer_auth(&api_key)
        .json(&req_body)
        .send()
        .await?
        .json()
        .await?;

    let message = &res["choices"][0]["message"];

    // If AI decides to use a Tool (Rust Function Calling)
    if let Some(tool_calls) = message.get("tool_calls") {
        let mut tool_responses = Vec::new();

        for tool_call in tool_calls.as_array().unwrap() {
            let func_name = tool_call["function"]["name"].as_str().unwrap_or("");
            let call_id = tool_call["id"].as_str().unwrap_or("");

            // Execute the Native Rust Functions!
            let function_result = match func_name {
                "get_wallet_balance" => execute_get_balance(&ctx, chat_id.0).await,
                "get_network_stats" => execute_get_network(&ctx).await,
                _ => "Unknown function".to_string(),
            };

            tool_responses.push(json!({
                "role": "tool",
                "tool_call_id": call_id,
                "name": func_name,
                "content": function_result
            }));
        }

        // Reconstruct message history for the final AI response
        let mut next_messages = messages.as_array().unwrap().clone();
        next_messages.push(message.clone()); // Append AI's tool request
        next_messages.extend(tool_responses); // Append Rust's execution results

        // 2nd API Call: Get Final Human-like Response
        let final_req = json!({ "model": "gpt-4o-mini", "messages": next_messages });
        let final_res: Value = client
            .post(OPENAI_API_URL)
            .bearer_auth(&api_key)
            .json(&final_req)
            .send()
            .await?
            .json()
            .await?;

        if let Some(final_text) = final_res["choices"][0]["message"]["content"].as_str() {
            let _ = bot
                .send_message(chat_id, final_text)
                .parse_mode(teloxide::types::ParseMode::Html)
                .await;
            return Ok(());
        }
    }

    // If AI responds normally without tools
    if let Some(content) = message.get("content").and_then(|c| c.as_str()) {
        let _ = bot
            .send_message(chat_id, content)
            .parse_mode(teloxide::types::ParseMode::Html)
            .await;
    }

    Ok(())
}

// ---- Rust Native Executions for the AI Engine ----

async fn execute_get_balance(ctx: &AppContext, chat_id: i64) -> String {
    let mut total = 0.0;
    let mut details = String::new();
    for e in ctx.state.iter().filter(|e| e.value().contains(&chat_id)) {
        if let Ok(a) = Address::try_from(e.key().as_str()) {
            if let Ok(utxos) = ctx.rpc.get_utxos_by_addresses(vec![a.clone()]).await {
                let k = utxos
                    .iter()
                    .map(|u| u.utxo_entry.amount as f64)
                    .sum::<f64>()
                    / 1e8;
                total += k;
                details.push_str(&format!(
                    "Wallet {}: {} KAS. ",
                    format_short_wallet(e.key()),
                    k
                ));
            }
        }
    }
    if total == 0.0 {
        return "User has 0 balances or no wallets tracked.".to_string();
    }
    format!("Total Balance: {} KAS. Details: {}", total, details)
}

async fn execute_get_network(ctx: &AppContext) -> String {
    let mut report = String::new();
    if let Ok(sync) = ctx.rpc.get_sync_status().await {
        report.push_str(&format!(
            "Sync Status: {}. ",
            if sync { "100% Synced" } else { "Syncing" }
        ));
    }
    if let Ok(dag) = ctx.rpc.get_block_dag_info().await {
        report.push_str(&format!("DAA Score: {}. ", dag.virtual_daa_score));
    }
    if let Ok(supply) = ctx.rpc.get_coin_supply().await {
        report.push_str(&format!(
            "Circulating Supply: {} KAS. ",
            f_num(supply.circulating_sompi as f64 / 1e8)
        ));
    }
    report
}
