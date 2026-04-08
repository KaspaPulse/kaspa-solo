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
            let _ = bot.send_message(msg.chat.id, "🎙️ <b>Audio Detected:</b> Voice analysis is currently disabled (Missing API Key). Please use text.").parse_mode(teloxide::types::ParseMode::Html).await;
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
            "🎧 <i>Listening and processing your voice note...</i>",
        )
        .parse_mode(teloxide::types::ParseMode::Html)
        .await;

    let file = bot.get_file(voice.file.id.clone()).await?;
    let temp_path = format!("temp_{}.ogg", msg.id.0);
    let mut local_file = File::create(&temp_path).await?;
    bot.download_file(&file.path, &mut local_file).await?;
    local_file.flush().await?;

    let client = Client::new();
    let file_bytes = tokio::fs::read(&temp_path).await?;
    let part = multipart::Part::bytes(file_bytes)
        .file_name("voice.ogg")
        .mime_str("audio/ogg")?;
    let form = multipart::Form::new()
        .part("file", part)
        .text("model", "whisper-1")
        .text("language", "ar");

    let res = client
        .post(WHISPER_API_URL)
        .bearer_auth(&api_key)
        .multipart(form)
        .send()
        .await?;

    let _ = tokio::fs::remove_file(&temp_path).await;

    if let Ok(json_resp) = res.json::<Value>().await {
        if let Some(text) = json_resp.get("text").and_then(|t| t.as_str()) {
            let user_id = msg.from.as_ref().map(|u| u.id.0 as i64).unwrap_or(0);
            return process_conversational_intent(bot, msg.chat.id, user_id, text.to_string(), ctx)
                .await;
        }
    }

    let _ = bot
        .send_message(
            msg.chat.id,
            "⚠️ Sorry, I could not transcribe the audio clearly. Please try again.",
        )
        .await;
    Ok(())
}

pub async fn process_conversational_intent(
    bot: Bot,
    chat_id: teloxide::types::ChatId,
    user_id: i64,
    user_text: String,
    ctx: AppContext,
) -> anyhow::Result<()> {
    let api_key = match std::env::var("OPENAI_API_KEY") {
        Ok(k) if !k.is_empty() => k,
        _ => {
            return crate::handlers::fallback_heuristic_text(bot, chat_id, &user_text, ctx).await;
        }
    };

    let _ = bot
        .send_chat_action(chat_id, teloxide::types::ChatAction::Typing)
        .await;

    let client = Client::new();

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
        },
        {
            "type": "function",
            "function": {
                "name": "search_kaspa_docs",
                "description": "Searches the official Kaspa knowledge base for technical questions, algorithms, mining guides, or troubleshooting.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "The technical question or topic to search for." }
                    },
                    "required": ["query"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "search_live_internet",
                "description": "Searches the global internet for real-time news, live updates, or information that is not available locally.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "The precise search query to look up on the global internet." }
                    },
                    "required": ["query"]
                }
            }
        }
    ]);

    let system_prompt = "You are the Kaspa Solo Enterprise AI. You are a highly professional assistant for crypto miners. You analyze intents, use tools for real-time data, and remember the recent conversation context.";

    let mut history_queue = ctx
        .memory
        .entry(user_id)
        .or_insert_with(|| std::collections::VecDeque::with_capacity(10));

    history_queue.push_back(json!({ "role": "user", "content": user_text.clone() }));
    if history_queue.len() > 10 {
        history_queue.pop_front();
    }

    let mut messages = vec![json!({ "role": "system", "content": system_prompt })];
    messages.extend(history_queue.iter().cloned());
    let messages_json = serde_json::Value::Array(messages.clone());

    let req_body = json!({
        "model": "gpt-4o-mini",
        "messages": messages_json,
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

    if let Some(tool_calls) = message.get("tool_calls") {
        let mut tool_responses = Vec::new();

        for tool_call in tool_calls.as_array().unwrap() {
            let func_name = tool_call["function"]["name"].as_str().unwrap_or("");
            let call_id = tool_call["id"].as_str().unwrap_or("");

            let function_result = match func_name {
                "get_wallet_balance" => execute_get_balance(&ctx, chat_id.0).await,
                "get_network_stats" => execute_get_network(&ctx).await,
                "search_kaspa_docs" => {
                    let args: Value = serde_json::from_str(
                        tool_call["function"]["arguments"].as_str().unwrap_or("{}"),
                    )
                    .unwrap_or_default();
                    let query = args["query"].as_str().unwrap_or("");
                    crate::rag::search_kaspa_docs(query).await
                }
                "search_live_internet" => {
                    let args: Value = serde_json::from_str(
                        tool_call["function"]["arguments"].as_str().unwrap_or("{}"),
                    )
                    .unwrap_or_default();
                    let query = args["query"].as_str().unwrap_or("");
                    execute_global_search(query).await
                }
                _ => "Unknown function".to_string(),
            };

            tool_responses.push(json!({
                "role": "tool",
                "tool_call_id": call_id,
                "name": func_name,
                "content": function_result
            }));
        }

        let mut next_messages = messages.clone();
        next_messages.push(message.clone());
        next_messages.extend(tool_responses);
        let next_messages_json = serde_json::Value::Array(next_messages);

        let final_req = json!({ "model": "gpt-4o-mini", "messages": next_messages_json });
        let final_res: Value = client
            .post(OPENAI_API_URL)
            .bearer_auth(&api_key)
            .json(&final_req)
            .send()
            .await?
            .json()
            .await?;

        if let Some(final_text) = final_res["choices"][0]["message"]["content"].as_str() {
            history_queue.push_back(json!({ "role": "assistant", "content": final_text }));
            if history_queue.len() > 10 {
                history_queue.pop_front();
            }

            let _ = bot
                .send_message(chat_id, final_text)
                .parse_mode(teloxide::types::ParseMode::Html)
                .await;
            return Ok(());
        }
    }

    if let Some(content) = message.get("content").and_then(|c| c.as_str()) {
        history_queue.push_back(json!({ "role": "assistant", "content": content }));
        if history_queue.len() > 10 {
            history_queue.pop_front();
        }

        let _ = bot
            .send_message(chat_id, content)
            .parse_mode(teloxide::types::ParseMode::Html)
            .await;
    }

    Ok(())
}

async fn execute_get_balance(ctx: &AppContext, chat_id: i64) -> String {
    let mut total = 0.0;
    let mut details = String::new();

    // FIX: Collect wallets into a vector first to avoid DashMap deadlocks across .await boundaries.
    let tracked_wallets: Vec<String> = ctx
        .state
        .iter()
        .filter(|e| e.value().contains(&chat_id))
        .map(|e| e.key().clone())
        .collect();

    for wallet_str in tracked_wallets {
        if let Ok(a) = Address::try_from(wallet_str.as_str()) {
            if let Ok(utxos) = ctx.rpc.get_utxos_by_addresses(vec![a.clone()]).await {
                let k = utxos
                    .iter()
                    .map(|u| u.utxo_entry.amount as f64)
                    .sum::<f64>()
                    / 1e8;
                total += k;
                details.push_str(&format!(
                    "Wallet {}: {} KAS. ",
                    format_short_wallet(&wallet_str),
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

async fn execute_global_search(query: &str) -> String {
    tracing::info!("[WEB AGENT] AI initiated global search for: {}", query);

    // Using Wikipedia's open global API as a highly reliable, zero-config enterprise data source.
    // In a fully scaled enterprise scenario, this can be swapped with Tavily API or SerpApi.
    let url = format!(
        "https://en.wikipedia.org/w/api.php?action=query&list=search&srsearch={}&utf8=&format=json",
        urlencoding::encode(query)
    );

    let client = Client::new();
    match client.get(&url).send().await {
        Ok(res) => {
            if let Ok(json_res) = res.json::<Value>().await {
                if let Some(results) = json_res["query"]["search"].as_array() {
                    let mut findings = String::new();
                    for item in results.iter().take(3) {
                        let title = item["title"].as_str().unwrap_or("");
                        let snippet = item["snippet"].as_str().unwrap_or("");
                        let clean_snippet = crate::utils::clean_for_log(snippet); // Reuse our HTML cleaner
                        findings.push_str(&format!(
                            "Source: {}\\nContent: {}\\n\\n",
                            title, clean_snippet
                        ));
                    }
                    if findings.is_empty() {
                        return "Global search completed but found no highly relevant new data."
                            .to_string();
                    }
                    return format!("Global Web Search Results:\\n{}", findings);
                }
            }
            "Error: Could not parse global server response.".to_string()
        }
        Err(e) => {
            tracing::error!("[WEB AGENT] Failed to connect to global servers: {}", e);
            "Error: Global internet connection failed.".to_string()
        }
    }
}
