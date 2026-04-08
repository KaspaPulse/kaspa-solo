#![allow(
    clippy::too_many_arguments,
    clippy::unnecessary_cast,
    clippy::redundant_pattern_matching
)]

use chrono::Utc;
use kaspa_addresses::Address;
use kaspa_rpc_core::api::rpc::RpcApi;
use rev_lines::RevLines;
use std::collections::HashSet;
use std::io::BufReader;
use std::sync::atomic::Ordering;
use sysinfo::System;
use teloxide::{prelude::*, types::ChatId};
// ✅ FIXED: Removed unused sleep and Duration imports to resolve CI warnings
use tokio::time::Instant;

use crate::commands::Command;
use crate::context::AppContext;
use crate::utils::{f_num, format_short_wallet};

pub async fn handle_command(
    bot: Bot,
    msg: Message,
    cmd: Command,
    ctx: AppContext,
) -> anyhow::Result<()> {
    let chat_id = msg.chat.id;
    let user_id = msg.from.as_ref().map(|u| u.id.0 as i64).unwrap_or(0);
    execute_command(bot, chat_id, user_id, cmd, ctx, None).await
}

pub async fn handle_callback(
    bot: Bot,
    q: teloxide::types::CallbackQuery,
    ctx: AppContext,
) -> anyhow::Result<()> {
    let user_id = q.from.id.0 as i64;

    if crate::utils::is_spam(user_id) {
        tracing::warn!("[UX] Rate limited button click from User: {}", user_id);
        let _ = bot
            .answer_callback_query(q.id.clone())
            .text("⚠️ Processing... Please wait a moment!")
            .show_alert(false)
            .await;
        return Ok(());
    }

    if let Some(data) = q.data.clone() {
        if let Some(msg) = q.regular_message() {
            let (cmd, is_refresh) = match data.as_str() {
                "cmd_balance" => (Some(Command::Balance), false),
                "refresh_balance" => (Some(Command::Balance), true),
                "cmd_miner" => (Some(Command::Miner), false),
                "refresh_miner" => (Some(Command::Miner), true),
                "cmd_blocks" => (Some(Command::Blocks), false),
                "refresh_blocks" => (Some(Command::Blocks), true),
                "cmd_list" => (Some(Command::List), false),
                "refresh_list" => (Some(Command::List), true),
                "cmd_price" => (Some(Command::Price), false),
                "refresh_price" => (Some(Command::Price), true),
                "cmd_market" => (Some(Command::Market), false),
                "refresh_market" => (Some(Command::Market), true),
                "cmd_network" => (Some(Command::Network), false),
                "refresh_network" => (Some(Command::Network), true),
                "cmd_fees" => (Some(Command::Fees), false),
                "refresh_fees" => (Some(Command::Fees), true),
                "cmd_supply" => (Some(Command::Supply), false),
                "refresh_supply" => (Some(Command::Supply), true),
                "cmd_dag" => (Some(Command::Dag), false),
                "refresh_dag" => (Some(Command::Dag), true),
                "cmd_stats" => (Some(Command::Stats), false),
                "refresh_stats" => (Some(Command::Stats), true),
                "cmd_sys" => (Some(Command::Sys), false),
                "refresh_sys" => (Some(Command::Sys), true),
                "cmd_donate" => (Some(Command::Donate), false),
                _ => (None, false),
            };

            if let Some(c) = cmd {
                let edit_msg_id = if is_refresh { Some(msg.id) } else { None };
                let _ =
                    execute_command(bot.clone(), msg.chat.id, user_id, c, ctx, edit_msg_id).await;
            }
        }
    }
    let _ = bot.answer_callback_query(q.id).await;
    Ok(())
}

async fn execute_command(
    bot: Bot,
    chat_id: ChatId,
    user_id: i64,
    cmd: Command,
    ctx: AppContext,
    edit_msg_id: Option<teloxide::types::MessageId>,
) -> anyhow::Result<()> {
    let is_admin = user_id == ctx.admin_id;
    let timer = Instant::now();

    crate::utils::log_multiline(
        &format!(
            "📥 [CMD IN] User: {} | Chat: {} | Command: {:?}",
            user_id, chat_id.0, cmd
        ),
        "",
        false,
    );
    let current_utc_time = Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string();

    match cmd {
        Command::Start => {
            let help_text = "🤖 <b>Kaspa Enterprise AI Engine</b>\n━━━━━━━━━━━━━━━━━━\nWelcome to the next generation of Kaspa monitoring. I am an autonomous AI Agent connected directly to your node.\n\n✨ <b>What's New?</b>\n🎙️ <b>Voice Notes:</b> Send me audio in English or Arabic.\n🧠 <b>Ask Anything:</b> Chat naturally about Kaspa algorithms, DAG, or live stats.\n⚡ <b>Smart Track:</b> Just paste any Kaspa address to track it.\n\nSelect an option below or simply start talking to me!";
            let _ = bot
                .send_message(chat_id, help_text)
                .parse_mode(teloxide::types::ParseMode::Html)
                .reply_markup(crate::kaspa_features::main_menu_markup())
                .await;
        }
        Command::Help => {
            let help_text = "📚 <b>Ultimate AI & Node Guide</b>\n━━━━━━━━━━━━━━━━━━\n<b>1. 🧠 AI & Voice Agent</b>\n• 🎙️ <i>Voice Notes:</i> Hold the mic and say \"What is my balance?\" or \"كم رصيدي؟\"\n• 💬 <i>Natural Chat:</i> Ask technical questions (e.g., \"Explain kHeavyHash\"). I will search the Kaspa knowledge base.\n• 🧠 <i>Context Memory:</i> I remember the context of our recent conversation.\n\n<b>2. ⚡ Smart Features</b>\n• 📋 <i>Auto-Add:</i> Paste a <code>kaspa:...</code> address to track it instantly.\n• 🌐 <i>Web Agent:</i> I can search the internet for live Kaspa updates.\n\n<b>3. 📌 Core Commands</b>\n• /balance - Live UTXO balances\n• /blocks - Unspent mined blocks\n• /network - Node health & Global stats\n\n<i>💡 Pro Tip: You don't need commands anymore. Just talk to me naturally!</i>";
            let _ = bot
                .send_message(chat_id, help_text)
                .parse_mode(teloxide::types::ParseMode::Html)
                .await;
        }
        Command::Add(wallet) => {
            let wallet = wallet.trim().to_string();
            if wallet.is_empty() || !wallet.starts_with("kaspa:") {
                let _ = bot
                    .send_message(
                        chat_id,
                        "⚠️ <b>Invalid Format.</b>\nPlease use: <code>/add kaspa:q...</code>",
                    )
                    .parse_mode(teloxide::types::ParseMode::Html)
                    .await;
                return Ok(());
            }
            crate::state::add_wallet_to_db(&ctx.pool, &wallet, chat_id.0).await;
            ctx.state
                .entry(wallet.clone())
                .or_insert_with(std::collections::HashSet::new)
                .insert(chat_id.0);
            let _ = bot
                .send_message(
                    chat_id,
                    format!(
                        "✅ <b>Wallet Added!</b>\n<code>{}</code> is now being monitored.",
                        wallet
                    ),
                )
                .parse_mode(teloxide::types::ParseMode::Html)
                .await;
        }
        Command::Remove(wallet) => {
            let wallet = wallet.trim().to_string();
            crate::state::remove_wallet_from_db(&ctx.pool, &wallet, chat_id.0).await;
            if let Some(mut users) = ctx.state.get_mut(&wallet) {
                users.remove(&chat_id.0);
            }
            let _ = bot.send_message(chat_id, "🗑️ <b>Wallet Removed.</b>\nYou will no longer receive notifications for this wallet.").parse_mode(teloxide::types::ParseMode::Html).await;
        }
        Command::List => {
            let mut tracked = String::new();
            for e in ctx.state.iter().filter(|e| e.value().contains(&chat_id.0)) {
                tracked.push_str(&format!("• <code>{}</code>\n", e.key()));
            }
            let text = if tracked.is_empty() {
                "📂 <b>You are not tracking any wallets yet.</b>\nUse <code>/add kaspa:...</code> to add one.".to_string()
            } else {
                format!("📂 <b>Your Tracked Wallets:</b>\n{}", tracked)
            };
            let _ = bot
                .send_message(chat_id, text)
                .parse_mode(teloxide::types::ParseMode::Html)
                .await;
        }
        Command::Balance => {
            let mut total = 0.0;
            let mut text = format!("💰 <b>Wallet Analysis & Live Balance</b>\n⏱️ <code>{}</code>\n━━━━━━━━━━━━━━━━━━\n", current_utc_time);

            let tracked_wallets: Vec<String> = ctx
                .state
                .iter()
                .filter(|e| e.value().contains(&chat_id.0))
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
                        text.push_str(&format!("⏱️ <code>{}</code>\n├ <b>Live Balance:</b> {:.8} KAS\n└ <b>UTXOs:</b> {}\n\n", format_short_wallet(&wallet_str), k, utxos.len()));
                    }
                }
            }
            text.push_str(&format!(
                "━━━━━━━━━━━━━━━━━━\n💎 <b>Total Holdings:</b> <code>{} KAS</code>",
                f_num(total)
            ));
            let _ = crate::utils::send_or_edit_log(
                &bot,
                chat_id,
                edit_msg_id,
                text,
                Some(crate::utils::refresh_markup("refresh_balance")),
            )
            .await;
        }
        Command::Blocks => {
            let tracked: Vec<String> = ctx
                .state
                .iter()
                .filter(|e| e.value().contains(&chat_id.0))
                .map(|e| e.key().clone())
                .collect();
            if tracked.is_empty() {
                let _ = crate::utils::send_or_edit_log(
                    &bot,
                    chat_id,
                    edit_msg_id,
                    "⚠️ <b>No wallets tracked.</b>",
                    None,
                )
                .await;
                return Ok(());
            }
            let mut text = format!("🧱 <b>Mined Blocks Tracker (Unspent)</b>\n⏱️ <code>{}</code>\n━━━━━━━━━━━━━━━━━━\n", current_utc_time);
            let (mut global_blocks, mut global_rewards) = (0, 0.0);
            for w in tracked {
                if let Ok(addr) = Address::try_from(w.as_str()) {
                    if let Ok(utxos) = ctx.rpc.get_utxos_by_addresses(vec![addr]).await {
                        let coinbase_utxos: Vec<_> = utxos
                            .into_iter()
                            .filter(|u| u.utxo_entry.is_coinbase)
                            .collect();
                        let total_blocks = coinbase_utxos.len();
                        let total_kas: f64 = coinbase_utxos
                            .iter()
                            .map(|u| u.utxo_entry.amount as f64)
                            .sum::<f64>()
                            / 1e8;
                        global_blocks += total_blocks;
                        global_rewards += total_kas;
                        text.push_str(&format!("⏱️ <code>{}</code>\n├ <b>Blocks Mined:</b> {}\n└ <b>Rewards Value:</b> {:.8} KAS\n\n", crate::utils::format_short_wallet(&w), total_blocks, total_kas));
                    }
                }
            }
            text.push_str(&format!("━━━━━━━━━━━━━━━━━━\n🏆 <b>Total Blocks:</b> {}\n💎 <b>Total Mined Value:</b> {:.8} KAS\n\n<i>*Note: Nodes only index unspent block rewards.</i>", global_blocks, global_rewards));
            let _ = crate::utils::send_or_edit_log(
                &bot,
                chat_id,
                edit_msg_id,
                text,
                Some(crate::utils::refresh_markup("refresh_blocks")),
            )
            .await;
        }
        Command::Miner => {
            let tracked: Vec<String> = ctx
                .state
                .iter()
                .filter(|e| e.value().contains(&chat_id.0))
                .map(|e| e.key().clone())
                .collect();
            if tracked.is_empty() {
                return Ok(());
            }
            let mut text = format!("⛏️ <b>Solo-Miner Hashrate Estimation</b>\n⏱️ <code>{}</code>\n━━━━━━━━━━━━━━━━━━\n", current_utc_time);
            if let Ok(dag_info) = ctx.rpc.get_block_dag_info().await {
                if let Ok(net_hashrate) =
                    ctx.rpc.estimate_network_hashes_per_second(1000, None).await
                {
                    let net_hashrate = net_hashrate as f64;
                    for w in tracked {
                        if let Ok(addr) = Address::try_from(w.as_str()) {
                            if let Ok(utxos) = ctx.rpc.get_utxos_by_addresses(vec![addr]).await {
                                let coinbase_utxos: Vec<_> = utxos
                                    .into_iter()
                                    .filter(|u| u.utxo_entry.is_coinbase)
                                    .collect();
                                let (mut b_1h, mut b_24h, mut b_7d) = (0, 0, 0);
                                for u in &coinbase_utxos {
                                    let age = dag_info
                                        .virtual_daa_score
                                        .saturating_sub(u.utxo_entry.block_daa_score);
                                    if age <= 3600 {
                                        b_1h += 1;
                                    }
                                    if age <= 86400 {
                                        b_24h += 1;
                                    }
                                    if age <= 604800 {
                                        b_7d += 1;
                                    }
                                }
                                text.push_str(&format!("⏱️ <code>{}</code>\n├ <b>1 Hour:</b> {} ({} Blks)\n├ <b>24 Hours:</b> {} ({} Blks)\n└ <b>7 Days:</b> {} ({} Blks)\n\n", crate::utils::format_short_wallet(&w), crate::kaspa_features::format_hashrate(net_hashrate * (b_1h as f64 / 3600.0)), b_1h, crate::kaspa_features::format_hashrate(net_hashrate * (b_24h as f64 / 86400.0)), b_24h, crate::kaspa_features::format_hashrate(net_hashrate * (b_7d as f64 / 604800.0)), b_7d));
                            }
                        }
                    }
                    text.push_str(&format!("━━━━━━━━━━━━━━━━━━\n🌐 <b>Network Hashrate:</b> {}\n<i>*Note: Based on unspent node rewards.</i>", crate::kaspa_features::format_hashrate(net_hashrate)));
                }
            }
            let _ = crate::utils::send_or_edit_log(
                &bot,
                chat_id,
                edit_msg_id,
                text,
                Some(crate::utils::refresh_markup("refresh_miner")),
            )
            .await;
        }
        Command::Network => {
            let mut text = String::from("🛠️ <b>Node Health & Network</b>\n");
            if let Ok(info) = ctx.rpc.get_server_info().await {
                text.push_str(&format!(
                    "├ <b>Version:</b> {} | <b>Net:</b> {}\n├ <b>UTXO Index:</b> {}\n",
                    info.server_version,
                    info.network_id,
                    if info.has_utxo_index {
                        "Enabled ✅"
                    } else {
                        "Disabled ❌"
                    }
                ));
            }
            if let Ok(peers) = ctx.rpc.get_connected_peer_info().await {
                text.push_str(&format!(
                    "├ <b>Connected Peers:</b> {}\n",
                    peers.peer_info.len()
                ));
            }
            if let Ok(sync) = ctx.rpc.get_sync_status().await {
                text.push_str(&format!(
                    "└ <b>Sync Status:</b> {}\n\n",
                    if sync {
                        "100% Synced ✅"
                    } else {
                        "Syncing ⚠️"
                    }
                ));
            }
            text.push_str("📊 <b>GHOSTDAG Consensus</b>\n");
            if let Ok(dag) = ctx.rpc.get_block_dag_info().await {
                text.push_str(&format!(
                    "├ <b>Total Blocks:</b> {}\n├ <b>DAA Score:</b> {}\n├ <b>Difficulty:</b> {}\n",
                    f_num(dag.block_count as f64),
                    dag.virtual_daa_score,
                    crate::kaspa_features::format_difficulty(dag.difficulty)
                ));
            }
            if let Ok(hashrate) = ctx.rpc.estimate_network_hashes_per_second(1000, None).await {
                text.push_str(&format!(
                    "├ <b>Hashrate:</b> {}\n",
                    crate::kaspa_features::format_hashrate(hashrate as f64)
                ));
            }
            if let Ok(supply) = ctx.rpc.get_coin_supply().await {
                let circ = supply.circulating_sompi as f64 / 1e8;
                let max = supply.max_sompi as f64 / 1e8;
                text.push_str(&format!(
                    "├ <b>Circulating:</b> {} KAS\n└ <b>Minted:</b> {:.2}%\n\n",
                    f_num(circ),
                    (circ / max) * 100.0
                ));
            }
            text.push_str(&format!("\n⏱️ <code>{}</code>", current_utc_time));
            let _ = crate::utils::send_or_edit_log(
                &bot,
                chat_id,
                edit_msg_id,
                text,
                Some(crate::utils::refresh_markup("refresh_network")),
            )
            .await;
        }
        Command::Dag => {
            if let Ok(info) = ctx.rpc.get_block_dag_info().await {
                let text = format!("📊 <b>BlockDAG Details:</b>\n🧱 <b>Blocks:</b> <code>{}</code>\n📜 <b>Headers:</b> <code>{}</code>\n\n⏱️ <code>{}</code>", f_num(info.block_count as f64), f_num(info.header_count as f64), current_utc_time);
                let _ = crate::utils::send_or_edit_log(
                    &bot,
                    chat_id,
                    edit_msg_id,
                    text,
                    Some(crate::utils::refresh_markup("refresh_dag")),
                )
                .await;
            }
        }
        Command::Price => {
            let price = ctx.price_cache.read().await.0;
            let text = if price > 0.0 {
                format!(
                    "💵 <b>Price:</b> <code>${:.4} USD</code> (CoinGecko)\n\n⏱️ <code>{}</code>",
                    price, current_utc_time
                )
            } else {
                format!(
                    "⚠️ <b>Price API Syncing...</b>\n\n⏱️ <code>{}</code>",
                    current_utc_time
                )
            };
            let _ = crate::utils::send_or_edit_log(
                &bot,
                chat_id,
                edit_msg_id,
                text,
                Some(crate::utils::refresh_markup("refresh_price")),
            )
            .await;
        }
        Command::Market => {
            let mcap = ctx.price_cache.read().await.1;
            let text = if mcap > 0.0 {
                format!(
                    "📈 <b>Market Cap:</b> <code>${} USD</code> (CoinGecko)\n\n⏱️ <code>{}</code>",
                    f_num(mcap),
                    current_utc_time
                )
            } else {
                format!(
                    "⚠️ <b>Market Cap API Syncing...</b>\n\n⏱️ <code>{}</code>",
                    current_utc_time
                )
            };
            let _ = crate::utils::send_or_edit_log(
                &bot,
                chat_id,
                edit_msg_id,
                text,
                Some(crate::utils::refresh_markup("refresh_market")),
            )
            .await;
        }
        Command::Supply => {
            if let Ok(supply) = ctx.rpc.get_coin_supply().await {
                let circ = supply.circulating_sompi as f64 / 1e8;
                let max = supply.max_sompi as f64 / 1e8;
                let text = format!("🪙 <b>Coin Supply:</b>\n├ <b>Circulating:</b> <code>{} KAS</code>\n├ <b>Max Supply:</b> <code>{} KAS</code>\n└ <b>Minted:</b> <code>{:.2}%</code>\n\n⏱️ <code>{}</code>", f_num(circ), f_num(max), (circ / max) * 100.0, current_utc_time);
                let _ = crate::utils::send_or_edit_log(
                    &bot,
                    chat_id,
                    edit_msg_id,
                    text,
                    Some(crate::utils::refresh_markup("refresh_supply")),
                )
                .await;
            }
        }
        Command::Fees => {
            if let Ok(r) = reqwest::get("https://api.kaspa.org/info/fee-estimate").await {
                if let Ok(j) = r.json::<serde_json::Value>().await {
                    let text = format!("⛽ <b>Fee Estimate:</b> <code>{:.2} sompi/gram</code>\n\n⏱️ <code>{}</code>", j["normalBuckets"][0]["feerate"].as_f64().unwrap_or(0.0), current_utc_time);
                    let _ = crate::utils::send_or_edit_log(
                        &bot,
                        chat_id,
                        edit_msg_id,
                        text,
                        Some(crate::utils::refresh_markup("refresh_fees")),
                    )
                    .await;
                }
            }
        }
        Command::Stats => {
            if is_admin {
                let total_users: HashSet<i64> =
                    ctx.state.iter().flat_map(|e| e.value().clone()).collect();
                let ping = tokio::time::Instant::now();
                let status = match ctx.rpc.get_server_info().await {
                    Ok(_) => format!("Online 🟢 ({}ms)", ping.elapsed().as_millis()),
                    Err(_) => "Offline 🔴".to_string(),
                };
                let text = format!("📊 <b>Enterprise Analytics</b>\n━━━━━━━━━━━━━━━━━━\n👥 <b>Active Users:</b> {}\n💼 <b>Tracked Wallets:</b> {}\n🌐 <b>Node Ping:</b> {}\n\n⏱️ <code>{}</code>", total_users.len(), ctx.state.len(), status, current_utc_time);
                let _ = crate::utils::send_or_edit_log(
                    &bot,
                    chat_id,
                    edit_msg_id,
                    text,
                    Some(crate::utils::refresh_markup("refresh_stats")),
                )
                .await;
            }
        }
        Command::Sys => {
            if is_admin {
                let monitoring = ctx.monitoring.load(Ordering::Relaxed);
                let current_time = current_utc_time.clone();

                let (used_mem, total_mem) = tokio::task::spawn_blocking(move || {
                    let mut s = System::new_all();
                    s.refresh_all();
                    (
                        s.used_memory() / 1024 / 1024,
                        s.total_memory() / 1024 / 1024,
                    )
                })
                .await
                .unwrap_or((0, 0));

                let text = format!("⚙️ <b>Server Node Diagnostics:</b>\n🧠 <b>RAM Used:</b> <code>{} MB</code>\n🧠 <b>RAM Total:</b> <code>{} MB</code>\n👀 <b>Monitor:</b> <code>{}</code>\n\n⏱️ <code>{}</code>", used_mem, total_mem, monitoring, current_time);
                let _ = crate::utils::send_or_edit_log(
                    &bot,
                    chat_id,
                    edit_msg_id,
                    text,
                    Some(crate::utils::refresh_markup("refresh_sys")),
                )
                .await;
            }
        }
        Command::Pause => {
            if is_admin {
                ctx.monitoring.store(false, Ordering::Relaxed);
                let _ = crate::utils::send_or_edit_log(
                    &bot,
                    chat_id,
                    None,
                    "⏸️ <b>Monitoring Paused.</b>".to_string(),
                    None,
                )
                .await;
            }
        }
        Command::Resume => {
            if is_admin {
                ctx.monitoring.store(true, Ordering::Relaxed);
                let _ = crate::utils::send_or_edit_log(
                    &bot,
                    chat_id,
                    None,
                    "▶️ <b>Monitoring Active.</b>".to_string(),
                    None,
                )
                .await;
            }
        }
        Command::Restart => {
            if is_admin {
                let _ = crate::utils::send_or_edit_log(
                    &bot,
                    chat_id,
                    None,
                    "🔄 <b>Restarting safely...</b>".to_string(),
                    None,
                )
                .await;
                std::process::exit(0);
            }
        }
        Command::Broadcast(ref m) => {
            if is_admin {
                let users: HashSet<i64> =
                    ctx.state.iter().flat_map(|e| e.value().clone()).collect();

                let count = users.len();
                for u in users {
                    let bot_clone = bot.clone();
                    let msg_text = format!("📢 <b>Admin Broadcast:</b>\n\n{}", m);

                    tokio::spawn(async move {
                        let _ = bot_clone
                            .send_message(ChatId(u), msg_text)
                            .parse_mode(teloxide::types::ParseMode::Html)
                            .await;
                    });
                }

                let _ = crate::utils::send_or_edit_log(
                    &bot,
                    chat_id,
                    None,
                    format!("✅ Broadcast sent to {} users.", count),
                    None,
                )
                .await;
            }
        }
        Command::Logs => {
            if is_admin {
                if let Ok(file) = std::fs::File::open("bot.log") {
                    let mut lines: Vec<String> = RevLines::new(BufReader::new(file))
                        .take(25)
                        .filter_map(Result::ok)
                        .collect();
                    lines.reverse();
                    let _ = crate::utils::send_or_edit_log(
                        &bot,
                        chat_id,
                        None,
                        format!(
                            "📜 <b>System Logs (Tail):</b>\n<pre>{}</pre>",
                            lines.join("\n")
                        ),
                        None,
                    )
                    .await;
                }
            }
        }
        Command::Donate => {
            let _ = crate::utils::send_or_edit_log(&bot, chat_id, None, "❤️ <b>Support & Donate</b>\nIf you find this bot valuable, consider supporting its development!\n\n<b>Kaspa (KAS) Address:</b>\n<code>kaspa:qz0yqq8z3twwgg7lq2mjzg6w4edqys45w2wslz7tym2tc6s84580vvx9zr44g</code>".to_string(), None).await;
        }
    };
    tracing::info!(
        "[TIME] Request processed in {}ms | ChatID: {}",
        timer.elapsed().as_millis(),
        chat_id.0
    );
    Ok(())
}

pub async fn handle_block_user(
    update: teloxide::types::ChatMemberUpdated,
    ctx: AppContext,
) -> anyhow::Result<()> {
    if update.new_chat_member.is_banned() || update.new_chat_member.is_left() {
        crate::state::remove_all_user_data(&ctx.pool, &ctx.state, update.chat.id.0).await;
    }
    Ok(())
}

#[allow(dead_code)]
pub async fn handle_media(bot: Bot, msg: Message, ctx: AppContext) -> anyhow::Result<()> {
    if msg.voice().is_some() {
        return crate::ai::process_voice_message(bot, msg, ctx).await;
    }

    let text = if msg.audio().is_some() || msg.video_note().is_some() {
        "🎙️ <b>System Notice:</b> Please send voice notes directly, not audio files or video notes."
    } else if msg.photo().is_some() || msg.video().is_some() {
        "📸 <b>Media Detected:</b> I cannot analyze images or videos visually. Please use text."
    } else {
        "⚠️ <b>Format Error:</b> Unsupported file type. Please use text commands."
    };
    let _ = bot
        .send_message(msg.chat.id, text)
        .parse_mode(teloxide::types::ParseMode::Html)
        .await;
    Ok(())
}

#[allow(dead_code)]
pub async fn handle_text_router(bot: Bot, msg: Message, ctx: AppContext) -> anyhow::Result<()> {
    let raw_text = msg.text().unwrap_or("").trim();
    let lower_text = raw_text.to_lowercase();

    if raw_text.starts_with('/')
        || lower_text.starts_with("kaspa:")
        || (lower_text.starts_with('q') && lower_text.len() >= 60)
    {
        return fallback_heuristic_text(bot, msg.chat.id, raw_text, ctx).await;
    }

    let chat_id = msg.chat.id;
    let user_id = msg.from.as_ref().map(|u| u.id.0 as i64).unwrap_or(0);
    crate::ai::process_conversational_intent(bot, chat_id, user_id, raw_text.to_string(), ctx).await
}

pub async fn fallback_heuristic_text(
    bot: Bot,
    chat_id: teloxide::types::ChatId,
    raw_text: &str,
    ctx: AppContext,
) -> anyhow::Result<()> {
    let lower_text = raw_text.to_lowercase();

    if lower_text.starts_with("kaspa:") || (lower_text.starts_with('q') && lower_text.len() >= 60) {
        let clean_address = if lower_text.starts_with("kaspa:") {
            raw_text.to_string()
        } else {
            format!("kaspa:{}", raw_text)
        };
        if kaspa_addresses::Address::try_from(clean_address.as_str()).is_ok() {
            ctx.state
                .entry(clean_address.clone())
                .or_insert_with(std::collections::HashSet::new)
                .insert(chat_id.0);
            crate::state::add_wallet_to_db(&ctx.pool, &clean_address, chat_id.0).await;
            let _ = bot
                .send_message(
                    chat_id,
                    format!(
                        "⚡ <b>Smart Auto-Add Activated!</b>\n✅ Now tracking:\n<code>{}</code>",
                        clean_address
                    ),
                )
                .parse_mode(teloxide::types::ParseMode::Html)
                .await;
        }
        return Ok(());
    }

    if raw_text.starts_with('/') {
        let known_commands = vec![
            "/start", "/help", "/add", "/remove", "/list", "/balance", "/blocks", "/miner",
            "/network", "/dag", "/price", "/market", "/supply", "/fees", "/donate",
        ];
        for cmd in known_commands {
            if strsim::levenshtein(&lower_text, cmd) <= 2 && lower_text.len() > 2 {
                let _ = bot
                    .send_message(
                        chat_id,
                        format!("🤖 <b>Command not found.</b>\nDid you mean {} ?", cmd),
                    )
                    .parse_mode(teloxide::types::ParseMode::Html)
                    .await;
                return Ok(());
            }
        }
    }

    let response = if lower_text.contains("balance") || lower_text.contains("funds") {
        "💰 Tap /balance to view your live node data."
    } else if lower_text.contains("hashrate") || lower_text.contains("speed") {
        "⛏️ Tap /miner to estimate your solo hashrate."
    } else if lower_text.contains("block") || lower_text.contains("mined") {
        "🧱 Tap /blocks to view mined blocks."
    } else {
        "🤖 <b>Unrecognized Input.</b> Press /start for the menu."
    };

    let _ = bot
        .send_message(chat_id, response)
        .parse_mode(teloxide::types::ParseMode::Html)
        .await;
    Ok(())
}

pub async fn handle_raw_message_v2(
    bot: teloxide::prelude::Bot,
    msg: teloxide::prelude::Message,
    ctx: crate::context::AppContext,
) -> anyhow::Result<()> {
    let user_id = msg.from.as_ref().map(|u| u.id.0 as i64).unwrap_or(0);

    if user_id != ctx.admin_id && ctx.rate_limiter.check_key(&user_id).is_err() {
        tracing::warn!("[SECURITY] Spam blocked for User: {}", user_id);
        let _ = bot
            .send_message(
                msg.chat.id,
                "🛑 <b>Rate Limit Exceeded!</b>\nYou are sending messages too fast.",
            )
            .parse_mode(teloxide::types::ParseMode::Html)
            .await;
        return Ok(());
    }

    if msg.voice().is_some() {
        return crate::ai::process_voice_message(bot, msg, ctx).await;
    }

    if let Some(text) = msg.text() {
        let raw_text = text.trim();
        let lower_text = raw_text.to_lowercase();

        if raw_text.starts_with('/')
            || lower_text.starts_with("kaspa:")
            || (lower_text.starts_with('q') && lower_text.len() >= 60)
        {
            return crate::handlers::fallback_heuristic_text(bot, msg.chat.id, raw_text, ctx).await;
        }

        return crate::ai::process_conversational_intent(
            bot,
            msg.chat.id,
            user_id,
            raw_text.to_string(),
            ctx,
        )
        .await;
    }

    if msg.photo().is_some() || msg.video().is_some() || msg.document().is_some() {
        let _ = bot.send_message(msg.chat.id, "📸 <b>Media Detected:</b> I cannot analyze visual media. Please use text or voice.").parse_mode(teloxide::types::ParseMode::Html).await;
    }
    Ok(())
}
