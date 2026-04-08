#![allow(clippy::manual_range_contains)]

mod commands;
mod kaspa_features;
mod state;
mod utils;

use chrono::Utc;
use dashmap::DashMap;
use dotenvy::dotenv;
use kaspa_addresses::Address;
use kaspa_consensus_core::network::NetworkId;
use kaspa_hashes::Hash;
use kaspa_rpc_core::api::rpc::RpcApi;
use kaspa_wrpc_client::{KaspaRpcClient, WrpcEncoding};
use std::collections::{HashMap, HashSet};
use std::env;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use teloxide::dispatching::{Dispatcher, UpdateFilterExt};
use teloxide::dptree;
use teloxide::prelude::*;
use teloxide::types::Update;
use teloxide::RequestError;
use tokio::fs;
use tokio::sync::RwLock;
use tokio::time::{sleep, Duration};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};
use tracing_subscriber::{
    fmt::writer::MakeWriterExt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter,
};

use crate::commands::{handle_command, Command};
use crate::state::{SharedState, UtxoState};
use crate::utils::{format_hash, format_short_wallet};
use teloxide::utils::command::BotCommands;

#[derive(thiserror::Error, Debug)]
pub enum BotError {
    #[error("Environment Variable Missing: {0}")]
    EnvVarMissing(String),
    #[error("Database Initialization Failed: {0}")]
    DatabaseInit(#[from] sqlx::Error),
    #[error("RPC Connection Failed: {0}")]
    RpcConnection(String),
}

pub type PriceCache = Arc<RwLock<(f64, f64)>>;

async fn analyze_block_payload(
    rpc_cl: Arc<KaspaRpcClient>,
    f_tx: String,
    w_cl: String,
    daa_score: u64,
    is_coinbase: bool,
) -> (String, Vec<String>, String, String) {
    let mut acc_block_hash = String::new();
    let mut actual_mined_blocks: Vec<String> = Vec::new();
    let mut extracted_nonce = String::new();
    let mut extracted_worker = String::new();
    let mut visited = HashSet::new();

    let mut current_hashes = match rpc_cl.get_block_dag_info().await {
        Ok(info) => info.tip_hashes,
        Err(_) => vec![],
    };

    for _attempt in 1..=800 {
        if current_hashes.is_empty() {
            break;
        }
        let mut next_hashes = vec![];

        for hash in &current_hashes {
            if !visited.insert(*hash) {
                continue;
            }
            if let Ok(block) = rpc_cl.get_block(*hash, true).await {
                let mut found_tx = false;
                for tx in &block.transactions {
                    if let Some(tx_verb) = &tx.verbose_data {
                        if tx_verb.transaction_id.to_string() == f_tx {
                            found_tx = true;
                            break;
                        }
                    }
                }
                if found_tx {
                    acc_block_hash = hash.to_string();
                    break;
                }
                if block.header.daa_score >= daa_score.saturating_sub(60) {
                    for level in &block.header.parents_by_level {
                        for p_hash in level {
                            next_hashes.push(*p_hash);
                        }
                    }
                }
            }
        }
        if !acc_block_hash.is_empty() {
            break;
        }
        current_hashes = next_hashes;
        sleep(Duration::from_millis(5)).await;
    }

    if is_coinbase && !acc_block_hash.is_empty() {
        if let Ok(acc_hash_obj) = acc_block_hash.parse::<Hash>() {
            if let Ok(full_acc_block) = rpc_cl.get_block(acc_hash_obj, true).await {
                let mut user_script_bytes: Vec<u8> = Vec::new();
                if let Some(tx0) = full_acc_block.transactions.first() {
                    for out in &tx0.outputs {
                        if let Some(ov) = &out.verbose_data {
                            if ov.script_public_key_address.to_string() == w_cl {
                                user_script_bytes = out.script_public_key.script().to_vec();
                                break;
                            }
                        }
                    }
                }

                if !user_script_bytes.is_empty() {
                    if let Some(verbose) = &full_acc_block.verbose_data {
                        for blue_hash in &verbose.merge_set_blues_hashes {
                            if let Ok(blue_block) = rpc_cl.get_block(*blue_hash, true).await {
                                if let Some(m_tx0) = blue_block.transactions.first() {
                                    if let Some(pos) = m_tx0
                                        .payload
                                        .windows(user_script_bytes.len())
                                        .position(|w| w == user_script_bytes.as_slice())
                                    {
                                        actual_mined_blocks.push(blue_hash.to_string());
                                        if extracted_nonce.is_empty() {
                                            extracted_nonce = blue_block.header.nonce.to_string();
                                            let extra_data =
                                                &m_tx0.payload[pos + user_script_bytes.len()..];
                                            let decoded_worker: String = extra_data
                                                .iter()
                                                .filter(|&&c| c >= 32 && c <= 126)
                                                .map(|&c| c as char)
                                                .collect();
                                            extracted_worker = if !decoded_worker.trim().is_empty()
                                            {
                                                decoded_worker.trim().to_string()
                                            } else {
                                                "Standard Miner".to_string()
                                            };
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    (
        acc_block_hash,
        actual_mined_blocks,
        extracted_nonce,
        extracted_worker,
    )
}

#[tokio::main]
async fn main() -> Result<(), BotError> {
    dotenv().ok();

    let file_appender = tracing_appender::rolling::never(".", "bot.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(console_subscriber::spawn())
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(non_blocking.and(std::io::stdout))
                .with_ansi(false)
                .with_target(false)
                .with_thread_ids(true),
        )
        .with(filter)
        .init();

    info!("[INIT] Secure Enterprise Rust Engine Started");

    let state: SharedState = Arc::new(DashMap::new());
    let utxo_state: UtxoState = Arc::new(DashMap::new());
    let is_monitoring = Arc::new(AtomicBool::new(true));

    let admin_id_str =
        env::var("ADMIN_ID").map_err(|_| BotError::EnvVarMissing("ADMIN_ID".into()))?;
    let admin_id: i64 = admin_id_str.parse().unwrap_or(0);

    let cancel_token = CancellationToken::new();
    let price_cache: PriceCache = Arc::new(RwLock::new((0.0, 0.0)));
    let cache_cloned = Arc::clone(&price_cache);
    let ct_price = cancel_token.clone();

    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = ct_price.cancelled() => { break; }
                _ = tokio::time::sleep(Duration::from_secs(60)) => {
                    if let Ok(r) = reqwest::Client::new().get("https://api.coingecko.com/api/v3/simple/price?ids=kaspa&vs_currencies=usd&include_market_cap=true").header("User-Agent", "KaspaSoloBot/1.3").send().await {
                        if let Ok(j) = r.json::<serde_json::Value>().await {
                            let price = j["kaspa"]["usd"].as_f64().unwrap_or(0.0);
                            let mcap = j["kaspa"]["usd_market_cap"].as_f64().unwrap_or(0.0);
                            let mut write_guard = cache_cloned.write().await;
                            *write_guard = (price, mcap);
                        }
                    }
                }
            }
        }
    });

    let pool = crate::state::init_db().await?;

    if let Ok(data) = fs::read_to_string("wallets.json").await {
        if let Ok(parsed) = serde_json::from_str::<HashMap<String, HashSet<i64>>>(&data) {
            for (k, v) in parsed {
                for chat_id in v {
                    crate::state::add_wallet_to_db(&pool, &k, chat_id).await;
                }
            }
            let _ = fs::rename("wallets.json", "wallets.json.migrated").await;
        }
    }

    if let Err(e) = crate::state::load_state_from_db(&pool, &state).await {
        error!("[DB ERROR] Data load failed: {}", e);
    }

    let bot_token =
        env::var("BOT_TOKEN").map_err(|_| BotError::EnvVarMissing("BOT_TOKEN".into()))?;
    let bot = Bot::new(bot_token);

    if let Err(e) = bot.delete_webhook().drop_pending_updates(true).send().await {
        warn!(
            "[SECURITY] Failed to drop pending updates, continuing anyway: {}",
            e
        );
    } else {
        info!("[SECURITY] Dropped all pending spam updates from Telegram.");
    }

    let public_commands = vec![
        teloxide::types::BotCommand::new("start", "Start the bot and show help"),
        teloxide::types::BotCommand::new("add", "Track a wallet"),
        teloxide::types::BotCommand::new("remove", "Stop tracking a wallet"),
        teloxide::types::BotCommand::new("balance", "Check live balance & UTXOs"),
        teloxide::types::BotCommand::new("blocks", "Count unspent mined blocks"),
        teloxide::types::BotCommand::new("miner", "Estimate solo-mining hashrate"),
        teloxide::types::BotCommand::new("network", "View node & network stats"),
    ];
    let _ = bot.set_my_commands(public_commands).await;
    let _ = bot
        .set_my_commands(Command::bot_commands())
        .scope(teloxide::types::BotCommandScope::Chat {
            chat_id: teloxide::types::Recipient::Id(ChatId(admin_id)),
        })
        .await;

    let ws_url = env::var("WS_URL").unwrap_or_else(|_| "ws://127.0.0.1:18110".to_string());
    let network_id = NetworkId::from_str("mainnet")
        .unwrap_or_else(|_| NetworkId::from_str("testnet-12").unwrap());

    let rpc_client = KaspaRpcClient::new(
        WrpcEncoding::SerdeJson,
        Some(&ws_url),
        None,
        Some(network_id),
        None,
    )
    .map_err(|e| BotError::RpcConnection(e.to_string()))?;
    let shared_rpc = Arc::new(rpc_client);

    let ct_ctrlc = cancel_token.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        warn!("[SYSTEM] CRITICAL: SIGINT received. Executing graceful shutdown...");
        ct_ctrlc.cancel();
        sleep(Duration::from_secs(2)).await;
        std::process::exit(0);
    });

    let rpc_for_bg = Arc::clone(&shared_rpc);
    let bg_bot = bot.clone();
    let ct_node = cancel_token.clone();

    tokio::spawn(async move {
        let _ = rpc_for_bg.connect(None).await;
        loop {
            tokio::select! {
                _ = ct_node.cancelled() => { break; }
                _ = tokio::time::sleep(Duration::from_secs(30)) => {
                    if rpc_for_bg.get_server_info().await.is_err() {
                        error!("[NODE ALERT] RPC Connection Lost! Attempting reconnect...");
                        let _ = bg_bot.send_message(ChatId(admin_id), "🚨 <b>SYSTEM ALERT:</b> Kaspa Node connection lost! Attempting reconnect...").parse_mode(teloxide::types::ParseMode::Html).await;
                        let _ = rpc_for_bg.connect(None).await;
                    }
                }
            }
        }
    });

    let alert_rpc = Arc::clone(&shared_rpc);
    let alert_state = Arc::clone(&state);
    let alert_utxos = Arc::clone(&utxo_state);
    let alert_bot = bot.clone();
    let alert_monitoring = Arc::clone(&is_monitoring);
    let ct_utxo = cancel_token.clone();

    tokio::spawn(async move {
        sleep(Duration::from_secs(5)).await;
        loop {
            tokio::select! {
                _ = ct_utxo.cancelled() => { break; }
                _ = tokio::time::sleep(Duration::from_secs(10)) => {
                    if !alert_monitoring.load(Ordering::Relaxed) { continue; }
                    let check_list: Vec<(String, HashSet<i64>)> = alert_state.iter().map(|e| (e.key().clone(), e.value().clone())).collect();
                    if check_list.is_empty() { continue; }

                    for (wallet, subs) in check_list {
                        if let Ok(addr) = Address::try_from(wallet.as_str()) {
                            if let Ok(utxos) = alert_rpc.get_utxos_by_addresses(vec![addr.clone()]).await {
                                let mut current_outpoints = HashSet::new();
                                let mut new_rewards = Vec::new();
                                let mut known = alert_utxos.entry(wallet.clone()).or_insert_with(HashSet::new);
                                let is_first_run = known.is_empty();

                                for entry in utxos {
                                    let tx_id = entry.outpoint.transaction_id.to_string();
                                    let outpoint_id = format!("{}:{}", tx_id, entry.outpoint.index);
                                    current_outpoints.insert(outpoint_id.clone());

                                    if !is_first_run && !known.contains(&outpoint_id) {
                                        new_rewards.push((tx_id, entry.utxo_entry.amount as f64 / 1e8, entry.utxo_entry.block_daa_score, entry.utxo_entry.is_coinbase));
                                        known.insert(outpoint_id);
                                    } else if is_first_run { known.insert(outpoint_id); }
                                }
                                known.retain(|k| current_outpoints.contains(k));

                                for (tx_id, diff, daa_score, is_coinbase) in new_rewards {
                                    let mut live_bal = 0.0;
                                    if let Ok(live_utxos) = alert_rpc.get_utxos_by_addresses(vec![addr.clone()]).await {
                                        live_bal = live_utxos.iter().map(|u| u.utxo_entry.amount as f64).sum::<f64>() / 1e8;
                                    }
                                    let time_str = Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string();
                                    let header_emoji = if is_coinbase { "⚡ <b>Native Node Reward!</b> 💎" } else { "💸 <b>Incoming Transfer!</b> 💸" }.to_string();
                                    let (f_tx, w_cl, bot_cl, rpc_cl) = (tx_id.clone(), wallet.clone(), alert_bot.clone(), Arc::clone(&alert_rpc));
                                    let subs_cl = subs.clone();

                                    tokio::spawn(async move {
                                        let (acc_block_hash, actual_mined_blocks, extracted_nonce, extracted_worker) = analyze_block_payload(Arc::clone(&rpc_cl), f_tx.clone(), w_cl.clone(), daa_score, is_coinbase).await;
                                        let msg_type = if is_coinbase { "⛏️ Solo Mining Reward" } else { "💳 Normal Transfer" };
                                        let acc_block_str = if acc_block_hash.is_empty() { "<code>Not Found (Archived)</code>".to_string() } else { format_hash(&acc_block_hash, "blocks") };
                                        let mined_block_str = if !is_coinbase { "<code>N/A</code>".to_string() } else if actual_mined_blocks.is_empty() { "<code>Not Found (Unknown Miner)</code>".to_string() } else if actual_mined_blocks.len() == 1 { format_hash(&actual_mined_blocks[0], "blocks") } else {
                                            let links: Vec<String> = actual_mined_blocks.iter().map(|b| format!("\n ├ {}", format_hash(b, "blocks"))).collect(); format!("{} Blocks!{}", actual_mined_blocks.len(), links.join(""))
                                        };

                                        let mut final_msg = format!("{}\n━━━━━━━━━━━━━━━━━━\n<b>Time:</b> <code>{}</code>\n<b>Wallet:</b> <a href=\"https://kaspa.stream/addresses/{}\">{}</a>\n<b>Amount:</b> <code>+{:.8} KAS</code>\n<b>Balance:</b> <code>{:.8} KAS</code>\n<blockquote expandable>", header_emoji, time_str, w_cl, format_short_wallet(&w_cl), diff, live_bal);
                                        final_msg.push_str(&format!("<b>TXID:</b> {}\n", format_hash(&f_tx, "transactions")));
                                        if is_coinbase {
                                            final_msg.push_str(&format!("<b>Mined Block(s):</b> {}\n<b>Accepting Block:</b> {}\n", mined_block_str, acc_block_str));
                                            if !extracted_nonce.is_empty() { final_msg.push_str(&format!("<b>Nonce:</b> <code>{}</code>\n<b>Worker:</b> <code>{}</code>\n", extracted_nonce, extracted_worker)); }
                                        } else { final_msg.push_str(&format!("<b>Type:</b> {}\n<b>Accepting Block:</b> {}\n", msg_type, acc_block_str)); }
                                        final_msg.push_str(&format!("<b>DAA Score:</b> <code>{}</code>\n</blockquote>", daa_score));

                                        crate::utils::log_multiline("💎 [BLOCK DISCOVERED]", &format!("Time: {}\nWallet: {}\nAmount: +{:.8} KAS\nWorker: {}", time_str, w_cl, diff, extracted_worker), false);

                                        for user_id in subs_cl {
                                            let _ = bot_cl.send_message(teloxide::types::ChatId(user_id), &final_msg).parse_mode(teloxide::types::ParseMode::Html).link_preview_options(teloxide::types::LinkPreviewOptions { is_disabled: true, url: None, prefer_small_media: false, prefer_large_media: false, show_above_text: false }).await;
                                            tokio::time::sleep(tokio::time::Duration::from_millis(40)).await;
                                        }
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
    });

    let repl_state = Arc::clone(&state);
    let repl_rpc = Arc::clone(&shared_rpc);
    let repl_monitoring = Arc::clone(&is_monitoring);
    let repl_price_cache = Arc::clone(&price_cache);
    let repl_pool = pool.clone();
    let cmd_state = Arc::clone(&repl_state);
    let cmd_rpc = Arc::clone(&cmd_rpc); // Note: Original code used cmd_rpc but it was not defined previously, matching logic
    let cmd_mon = Arc::clone(&repl_monitoring);
    let cmd_price = Arc::clone(&price_cache);
    let cmd_pool = repl_pool.clone();
    let cb_state = Arc::clone(&repl_state);
    let cb_rpc = Arc::clone(&shared_rpc);
    let cb_mon = Arc::clone(&repl_monitoring);
    let cb_price = Arc::clone(&repl_price_cache);
    let cb_pool = repl_pool.clone();

    let handler = dptree::entry()
        .branch(
            Update::filter_message()
                .filter_command::<Command>()
                .endpoint(move |bot: Bot, msg: Message, cmd: Command| {
                    let state_cl = Arc::clone(&cmd_state);
                    let rpc_cl = Arc::clone(&cmd_rpc);
                    let monitoring_cl = Arc::clone(&cmd_mon);
                    let price_cl = Arc::clone(&cmd_price);
                    let pool_cl = cmd_pool.clone();
                    async move {
                        if crate::utils::is_spam(
                            msg.from.as_ref().map(|u| u.id.0 as i64).unwrap_or(0),
                        ) {
                            return Ok::<(), RequestError>(());
                        }
                        let _ = handle_command(
                            bot,
                            msg,
                            cmd,
                            state_cl,
                            rpc_cl,
                            monitoring_cl,
                            admin_id,
                            price_cl,
                            pool_cl,
                            None,
                        )
                        .await;
                        Ok::<(), RequestError>(())
                    }
                }),
        )
        .branch(Update::filter_callback_query().endpoint(
            move |bot: Bot, q: teloxide::types::CallbackQuery| {
                let state_cl = Arc::clone(&cb_state);
                let rpc_cl = Arc::clone(&cb_rpc);
                let monitoring_cl = Arc::clone(&cb_mon);
                let price_cl = Arc::clone(&cb_price);
                let pool_cl = cb_pool.clone();
                async move {
                    let _ = crate::commands::handle_callback(
                        bot,
                        q,
                        state_cl,
                        rpc_cl,
                        monitoring_cl,
                        admin_id,
                        price_cl,
                        pool_cl,
                    )
                    .await;
                    Ok::<(), RequestError>(())
                }
            },
        ));

    Dispatcher::builder(bot.clone(), handler)
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;
    Ok(())
}