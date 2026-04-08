use crate::state::{SharedState, UtxoState};
use kaspa_wrpc_client::KaspaRpcClient;
use sqlx::sqlite::SqlitePool;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::RwLock;

pub type PriceCache = Arc<RwLock<(f64, f64)>>;

#[derive(Clone)]
pub struct AppContext {
    pub rpc: Arc<KaspaRpcClient>,
    pub pool: SqlitePool,
    pub state: SharedState,
    pub utxo_state: UtxoState,
    pub monitoring: Arc<AtomicBool>,
    pub price_cache: PriceCache,
    pub admin_id: i64,
}
