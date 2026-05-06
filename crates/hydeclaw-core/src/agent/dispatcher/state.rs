//! Per-session state for the tool dispatcher: describe cache, call counts,
//! promotion set. RwLock-protected because parallel tool batches in
//! `pipeline/parallel.rs` can read/mutate concurrently within one session.

use dashmap::DashMap;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

/// Map of session UUID → per-session tool dispatcher state.
pub type SessionToolStateMap = Arc<DashMap<Uuid, Arc<SessionToolState>>>;

/// Per-session bookkeeping for the dispatcher. All fields use
/// `tokio::sync::RwLock` because handlers run inside async contexts.
#[derive(Default)]
pub struct SessionToolState {
    /// Cached `describe()` rendered output, keyed by tool name.
    pub describe_cache: RwLock<HashMap<String, String>>,
    /// Number of successful calls per extension tool name in this session.
    // allow(dead_code): consumed by a later dispatcher task (call-count
    // tracking and promotion threshold). Landed early alongside the rest of
    // the per-session state.
    #[allow(dead_code)]
    pub call_counts: RwLock<HashMap<String, u32>>,
    /// System extension tools promoted to per-session core after threshold.
    pub promoted: RwLock<HashSet<String>>,
}

impl SessionToolState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
}
