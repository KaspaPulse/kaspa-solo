use crate::state::{SharedState, UtxoState};
use dashmap::DashMap;
use governor::{clock::DefaultClock, state::keyed::DefaultKeyedStateStore, Quota, RateLimiter};
use kaspa_wrpc_client::KaspaRpcClient;
use sqlx::sqlite::SqlitePool;
use std::collections::VecDeque;
use std::num::NonZeroU32;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::RwLock;

pub type PriceCache = Arc<RwLock<(f64, f64)>>;
// Memory: Map User ID to a Queue of JSON Messages (Max 10)
pub type ContextMemory = Arc<DashMap<i64, VecDeque<serde_json::Value>>>;
pub type SpamLimiter = Arc<RateLimiter<i64, DefaultKeyedStateStore<i64>, DefaultClock>>;

#[derive(Clone)]
pub struct AppContext {
    pub rpc: Arc<KaspaRpcClient>,
    pub pool: SqlitePool,
    pub state: SharedState,
    pub utxo_state: UtxoState,
    pub monitoring: Arc<AtomicBool>,
    pub price_cache: PriceCache,
    pub admin_id: i64,
    pub memory: ContextMemory,
    #[allow(dead_code)]
    pub rate_limiter: SpamLimiter,
}

impl AppContext {
    pub fn new_rate_limiter() -> SpamLimiter {
        // Strict Policy: Max 2 messages per second per user. Bursts up to 3.
        Arc::new(RateLimiter::keyed(
            Quota::per_second(NonZeroU32::new(2).unwrap()).allow_burst(NonZeroU32::new(3).unwrap()),
        ))
    }
}
