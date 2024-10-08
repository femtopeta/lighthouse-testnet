//! Provides generic behaviour for multiple execution engines, specifically fallback behaviour.

use crate::engine_api::{
    EngineCapabilities, Error as EngineApiError, ForkchoiceUpdatedResponse, PayloadAttributes,
    PayloadId,
};
use crate::{ClientVersionV1, HttpJsonRpc};
use lru::LruCache;
use slog::{debug, error, info, warn, Logger};
use std::future::Future;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;
use task_executor::TaskExecutor;
use tokio::sync::{watch, Mutex, RwLock};
use tokio_stream::wrappers::WatchStream;
use types::non_zero_usize::new_non_zero_usize;
use types::ExecutionBlockHash;

/// The number of payload IDs that will be stored for each `Engine`.
///
/// Since the size of each value is small (~800 bytes) a large number is used for safety.
const PAYLOAD_ID_LRU_CACHE_SIZE: NonZeroUsize = new_non_zero_usize(512);
const CACHED_RESPONSE_AGE_LIMIT: Duration = Duration::from_secs(900); // 15 minutes

/// Stores the remembered state of a engine.
#[derive(Copy, Clone, PartialEq, Debug, Eq, Default)]
enum EngineStateInternal {
    Synced,
    #[default]
    Offline,
    Syncing,
    AuthFailed,
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
enum ResponseCacheAction {
    #[default]
    None,
    Update, // Update cached responses
    Clear,  // Clear cached responses
}

/// A subset of the engine state to inform other services if the engine is online or offline.
#[derive(Debug, Clone, PartialEq, Eq, Copy)]
pub enum EngineState {
    Online,
    Offline,
}

impl From<EngineStateInternal> for EngineState {
    fn from(state: EngineStateInternal) -> Self {
        match state {
            EngineStateInternal::Synced | EngineStateInternal::Syncing => EngineState::Online,
            EngineStateInternal::Offline | EngineStateInternal::AuthFailed => EngineState::Offline,
        }
    }
}

/// Wrapper structure that ensures changes to the engine state are correctly reported to watchers.
struct State {
    /// The actual engine state.
    state: EngineStateInternal,
    /// Notifier to watch the engine state.
    notifier: watch::Sender<EngineState>,
}

impl std::ops::Deref for State {
    type Target = EngineStateInternal;

    fn deref(&self) -> &Self::Target {
        &self.state
    }
}

impl Default for State {
    fn default() -> Self {
        let state = EngineStateInternal::default();
        let (notifier, _receiver) = watch::channel(state.into());
        State { state, notifier }
    }
}

impl State {
    // Updates the state and notifies all watchers if the state has changed.
    pub fn update(&mut self, new_state: EngineStateInternal) {
        self.state = new_state;
        self.notifier.send_if_modified(|last_state| {
            let changed = *last_state != new_state.into(); // notify conditionally
            *last_state = new_state.into(); // update the state unconditionally
            changed
        });
    }

    /// Gives access to a channel containing whether the last state is online.
    ///
    /// This can be called several times.
    pub fn watch(&self) -> WatchStream<EngineState> {
        self.notifier.subscribe().into()
    }
}

#[derive(Copy, Clone, PartialEq, Debug)]
pub struct ForkchoiceState {
    pub head_block_hash: ExecutionBlockHash,
    pub safe_block_hash: ExecutionBlockHash,
    pub finalized_block_hash: ExecutionBlockHash,
}

#[derive(Hash, PartialEq, std::cmp::Eq)]
struct PayloadIdCacheKey {
    pub head_block_hash: ExecutionBlockHash,
    pub payload_attributes: PayloadAttributes,
}

#[derive(Debug)]
pub enum EngineError {
    Offline,
    Api { error: EngineApiError },
    BuilderApi { error: EngineApiError },
    Auth,
}

/// An execution engine.
pub struct Engine {
    pub api: HttpJsonRpc,
    payload_id_cache: Mutex<LruCache<PayloadIdCacheKey, PayloadId>>,
    state: RwLock<State>,
    latest_forkchoice_state: RwLock<Option<ForkchoiceState>>,
    executor: TaskExecutor,
    log: Logger,
}

impl Engine {
    /// Creates a new, offline engine.
    pub fn new(api: HttpJsonRpc, executor: TaskExecutor, log: &Logger) -> Self {
        Self {
            api,
            payload_id_cache: Mutex::new(LruCache::new(PAYLOAD_ID_LRU_CACHE_SIZE)),
            state: Default::default(),
            latest_forkchoice_state: Default::default(),
            executor,
            log: log.clone(),
        }
    }

    /// Gives access to a channel containing the last engine state.
    ///
    /// This can be called several times.
    pub async fn watch_state(&self) -> WatchStream<EngineState> {
        self.state.read().await.watch()
    }

    pub async fn get_payload_id(
        &self,
        head_block_hash: &ExecutionBlockHash,
        payload_attributes: &PayloadAttributes,
    ) -> Option<PayloadId> {
        self.payload_id_cache
            .lock()
            .await
            .get(&PayloadIdCacheKey::new(head_block_hash, payload_attributes))
            .cloned()
    }

    pub async fn notify_forkchoice_updated(
        &self,
        forkchoice_state: ForkchoiceState,
        payload_attributes: Option<PayloadAttributes>,
        log: &Logger,
    ) -> Result<ForkchoiceUpdatedResponse, EngineApiError> {
        let response = self
            .api
            .forkchoice_updated(forkchoice_state, payload_attributes.clone())
            .await?;

        if let Some(payload_id) = response.payload_id {
            if let Some(key) = payload_attributes
                .map(|pa| PayloadIdCacheKey::new(&forkchoice_state.head_block_hash, &pa))
            {
                self.payload_id_cache.lock().await.put(key, payload_id);
            } else {
                debug!(
                    log,
                    "Engine returned unexpected payload_id";
                    "payload_id" => ?payload_id
                );
            }
        }

        Ok(response)
    }

    async fn get_latest_forkchoice_state(&self) -> Option<ForkchoiceState> {
        *self.latest_forkchoice_state.read().await
    }

    pub async fn set_latest_forkchoice_state(&self, state: ForkchoiceState) {
        *self.latest_forkchoice_state.write().await = Some(state);
    }

    async fn send_latest_forkchoice_state(&self) {
        let latest_forkchoice_state = self.get_latest_forkchoice_state().await;

        if let Some(forkchoice_state) = latest_forkchoice_state {
            if forkchoice_state.head_block_hash == ExecutionBlockHash::zero() {
                debug!(
                    self.log,
                    "No need to call forkchoiceUpdated";
                    "msg" => "head does not have execution enabled",
                );
                return;
            }

            info!(
                self.log,
                "Issuing forkchoiceUpdated";
                "forkchoice_state" => ?forkchoice_state,
            );

            // For simplicity, payload attributes are never included in this call. It may be
            // reasonable to include them in the future.
            if let Err(e) = self.api.forkchoice_updated(forkchoice_state, None).await {
                debug!(
                    self.log,
                    "Failed to issue latest head to engine";
                    "error" => ?e,
                );
            }
        } else {
            debug!(
                self.log,
                "No head, not sending to engine";
            );
        }
    }

    /// Returns `true` if the engine has a "synced" status.
    pub async fn is_synced(&self) -> bool {
        **self.state.read().await == EngineStateInternal::Synced
    }

    /// Returns `true` if the engine has a status other than synced or syncing.
    pub async fn is_offline(&self) -> bool {
        EngineState::from(**self.state.read().await) == EngineState::Offline
    }

    /// Run the `EngineApi::upcheck` function if the node's last known state is not synced. This
    /// might be used to recover the node if offline.
    pub async fn upcheck(&self) {
        let (state, cache_action) = match self.api.upcheck().await {
            Ok(()) => {
                let mut state = self.state.write().await;
                if **state != EngineStateInternal::Synced {
                    info!(
                        self.log,
                        "Execution engine online";
                    );

                    // Send the node our latest forkchoice_state.
                    self.send_latest_forkchoice_state().await;
                } else {
                    debug!(
                        self.log,
                        "Execution engine online";
                    );
                }
                state.update(EngineStateInternal::Synced);
                (**state, ResponseCacheAction::Update)
            }
            Err(EngineApiError::IsSyncing) => {
                let mut state = self.state.write().await;
                state.update(EngineStateInternal::Syncing);
                (**state, ResponseCacheAction::Update)
            }
            Err(EngineApiError::Auth(err)) => {
                error!(
                    self.log,
                    "Failed jwt authorization";
                    "error" => ?err,
                );

                let mut state = self.state.write().await;
                state.update(EngineStateInternal::AuthFailed);
                (**state, ResponseCacheAction::Clear)
            }
            Err(e) => {
                error!(
                    self.log,
                    "Error during execution engine upcheck";
                    "error" => ?e,
                );

                let mut state = self.state.write().await;
                state.update(EngineStateInternal::Offline);
                // need to clear cached responses if we detect the execution engine
                // is offline as it is likely the engine is being updated to a newer
                // version which might also have new capabilities
                (**state, ResponseCacheAction::Clear)
            }
        };

        // do this after dropping state lock guard to avoid holding two locks at once
        match cache_action {
            ResponseCacheAction::None => {}
            ResponseCacheAction::Update => {
                if let Err(e) = self
                    .get_engine_capabilities(Some(CACHED_RESPONSE_AGE_LIMIT))
                    .await
                {
                    warn!(self.log,
                        "Error during exchange capabilities";
                        "error" => ?e,
                    )
                } else {
                    // no point in running this if there was an error fetching the capabilities
                    // as it will just result in an error again
                    let _ = self
                        .get_engine_version(Some(CACHED_RESPONSE_AGE_LIMIT))
                        .await;
                }
            }
            ResponseCacheAction::Clear => {
                self.api.clear_exchange_capabilties_cache().await;
                self.api.clear_engine_version_cache().await;
            }
        }

        debug!(
            self.log,
            "Execution engine upcheck complete";
            "state" => ?state,
        );
    }

    /// Returns the execution engine capabilities resulting from a call to
    /// engine_exchangeCapabilities. If the capabilities cache is not populated,
    /// or if it is populated with a cached result of age >= `age_limit`, this
    /// method will fetch the result from the execution engine and populate the
    /// cache before returning it. Otherwise it will return a cached result from
    /// a previous call.
    ///
    /// Set `age_limit` to `None` to always return the cached result
    /// Set `age_limit` to `Some(Duration::ZERO)` to force fetching from EE
    pub async fn get_engine_capabilities(
        &self,
        age_limit: Option<Duration>,
    ) -> Result<EngineCapabilities, EngineApiError> {
        self.api.get_engine_capabilities(age_limit).await
    }

    /// Returns the execution engine version resulting from a call to
    /// engine_clientVersionV1. If the version cache is not populated, or if it
    /// is populated with a cached result of age >= `age_limit`, this method will
    /// fetch the result from the execution engine and populate the cache before
    /// returning it. Otherwise it will return the cached result from an earlier
    /// call.
    ///
    /// Set `age_limit` to `None` to always return the cached result
    /// Set `age_limit` to `Some(Duration::ZERO)` to force fetching from EE
    pub async fn get_engine_version(
        &self,
        age_limit: Option<Duration>,
    ) -> Result<Vec<ClientVersionV1>, EngineApiError> {
        self.api.get_engine_version(age_limit).await
    }

    /// Run `func` on the node regardless of the node's current state.
    ///
    /// ## Note
    ///
    /// This function takes locks on `self.state`, holding a conflicting lock might cause a
    /// deadlock.
    pub async fn request<'a, F, G, H>(self: &'a Arc<Self>, func: F) -> Result<H, EngineError>
    where
        F: FnOnce(&'a Engine) -> G,
        G: Future<Output = Result<H, EngineApiError>>,
    {
        match func(self).await {
            Ok(result) => {
                // Take a clone *without* holding the read-lock since the `upcheck` function will
                // take a write-lock.
                let state: EngineStateInternal = **self.state.read().await;

                // Keep an up to date engine state.
                if state != EngineStateInternal::Synced {
                    // Spawn the upcheck in another task to avoid slowing down this request.
                    let inner_self = self.clone();
                    self.executor.spawn(
                        async move { inner_self.upcheck().await },
                        "upcheck_after_success",
                    );
                }

                Ok(result)
            }
            Err(error) => {
                warn!(
                    self.log,
                    "Execution engine call failed";
                    "error" => ?error,
                );

                // The node just returned an error, run an upcheck so we can update the endpoint
                // state.
                //
                // Spawn the upcheck in another task to avoid slowing down this request.
                let inner_self = self.clone();
                self.executor.spawn(
                    async move { inner_self.upcheck().await },
                    "upcheck_after_error",
                );

                Err(EngineError::Api { error })
            }
        }
    }
}

impl PayloadIdCacheKey {
    fn new(head_block_hash: &ExecutionBlockHash, attributes: &PayloadAttributes) -> Self {
        Self {
            head_block_hash: *head_block_hash,
            payload_attributes: attributes.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_stream::StreamExt;

    #[tokio::test]
    async fn test_state_notifier() {
        let mut state = State::default();
        let initial_state: EngineState = state.state.into();
        assert_eq!(initial_state, EngineState::Offline);
        state.update(EngineStateInternal::Synced);

        // a watcher that arrives after the first update.
        let mut watcher = state.watch();
        let new_state = watcher.next().await.expect("Last state is always present");
        assert_eq!(new_state, EngineState::Online);
    }
}
