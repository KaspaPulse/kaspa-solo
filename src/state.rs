use dashmap::DashMap;
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use std::collections::HashSet;
use std::sync::Arc;
use tracing::{error, info};

pub type SharedState = Arc<DashMap<String, HashSet<i64>>>;
pub type UtxoState = Arc<DashMap<String, HashSet<String>>>;

pub async fn init_db() -> Result<SqlitePool, sqlx::Error> {
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect("sqlite://enterprise.db?mode=rwc")
        .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS user_wallets (
            wallet TEXT NOT NULL,
            chat_id INTEGER NOT NULL,
            PRIMARY KEY (wallet, chat_id)
        )",
    )
    .execute(&pool)
    .await?;

    Ok(pool)
}

pub async fn load_state_from_db(pool: &SqlitePool, state: &SharedState) -> Result<(), sqlx::Error> {
    let rows: Vec<(String, i64)> = sqlx::query_as("SELECT wallet, chat_id FROM user_wallets")
        .fetch_all(pool)
        .await?;

    for (wallet, chat_id) in rows {
        state
            .entry(wallet)
            .or_insert_with(HashSet::new)
            .insert(chat_id);
    }
    info!(
        "[DB] Loaded {} active wallets from Async SQLite.",
        state.len()
    );
    Ok(())
}

pub async fn add_wallet_to_db(pool: &SqlitePool, wallet: &str, chat_id: i64) {
    if let Err(e) =
        sqlx::query("INSERT OR IGNORE INTO user_wallets (wallet, chat_id) VALUES (?1, ?2)")
            .bind(wallet)
            .bind(chat_id)
            .execute(pool)
            .await
    {
        error!("[DB ERROR] Failed to add wallet: {}", e);
    }
}

pub async fn remove_wallet_from_db(pool: &SqlitePool, wallet: &str, chat_id: i64) {
    if let Err(e) = sqlx::query("DELETE FROM user_wallets WHERE wallet = ?1 AND chat_id = ?2")
        .bind(wallet)
        .bind(chat_id)
        .execute(pool)
        .await
    {
        error!("[DB ERROR] Failed to remove wallet: {}", e);
    }
}

// 🧹 Enterprise Cleanup Function - Deletes all user data instantly when they block the bot
pub async fn remove_all_user_data(pool: &SqlitePool, state: &SharedState, chat_id: i64) {
    // 1. Remove from SQLite DB
    if let Err(e) = sqlx::query("DELETE FROM user_wallets WHERE chat_id = ?1")
        .bind(chat_id)
        .execute(pool)
        .await
    {
        error!(
            "[DB ERROR] Failed to remove all data for user {}: {}",
            chat_id, e
        );
    }

    // 2. Remove from Active Memory (DashMap)
    let mut empty_wallets = Vec::new();
    for mut entry in state.iter_mut() {
        entry.value_mut().remove(&chat_id);
        if entry.value().is_empty() {
            empty_wallets.push(entry.key().clone());
        }
    }

    // 3. Drop unmonitored wallets from memory entirely
    for wallet in empty_wallets {
        state.remove(&wallet);
    }

    info!(
        "[SYSTEM OPTIMIZATION] Wiped all tracking data for blocked/left User ID: {}",
        chat_id
    );
}
