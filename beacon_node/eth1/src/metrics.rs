pub use lighthouse_metrics::*;

use lazy_static::lazy_static;

lazy_static! {
    /*
     * Eth1 blocks
     */
    pub static ref BLOCK_CACHE_LEN: Result<IntGauge> =
        try_create_int_gauge("eth1_block_cache_len", "Count of eth1 blocks in cache");
    pub static ref LATEST_CACHED_BLOCK_TIMESTAMP: Result<IntGauge> =
        try_create_int_gauge("eth1_latest_cached_block_timestamp", "Timestamp of latest block in eth1 cache");

    /*
     * Eth1 deposits
     */
    pub static ref DEPOSIT_CACHE_LEN: Result<IntGauge> =
        try_create_int_gauge("eth1_deposit_cache_len", "Number of deposits in the eth1 cache");
    pub static ref HIGHEST_PROCESSED_DEPOSIT_BLOCK: Result<IntGauge> =
        try_create_int_gauge("eth1_highest_processed_deposit_block", "Number of the last block checked for deposits");

    /*
     * Eth1 rpc connection
     */

    pub static ref ETH1_CONNECTED: Result<IntGauge> = try_create_int_gauge(
        "sync_eth1_connected", "Set to 1 if connected to an eth1 node, otherwise set to 0"
    );

}
