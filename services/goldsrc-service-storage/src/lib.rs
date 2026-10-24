//! Unified SQLite WAL Engine & Async MPSC Batch Worker for GoldSrc.rs.
//!
//! Provides zero-latency main-thread Key-Value and SQL operations for GoldSrc plugins:
//! - All writes go to a non-blocking `mpsc` queue (0 frame cost).
//! - Dedicated background thread batches writes to disk every 500ms using SQLite in WAL mode.
//! - Reads check the in-memory cache first, falling back to SQLite WAL queries.
//! - Synchronous transactional flush on `client_disconnect` and `ServerDeactivate`.

use goldsrc_api::consts::log_targets;
pub use goldsrc_spi::storage::{StorageError, StoragePort};
use std::marker::PhantomData;
use std::sync::Arc;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, unbounded};
use rusqlite::{Connection, params};

/// Storage operation dispatched over the non-blocking MPSC channel.
#[derive(Debug)]
enum StorageOp {
    Set {
        bucket: String,
        key: String,
        value: Vec<u8>,
    },
    Delete {
        bucket: String,
        key: String,
    },
    Flush(Sender<()>),
}

type StorageMemoryCache = HashMap<String, HashMap<String, Option<Vec<u8>>>>;

/// High-performance Key-Value & Relational Storage Engine backed by SQLite WAL.
pub struct SqliteStorageEngine {
    db_path: PathBuf,
    /// Fast in-memory cache for dirty/hot keys: bucket -> (key -> value)
    memory_cache: RwLock<StorageMemoryCache>,
    /// Direct read connection protected by a mutex for queries on cache misses
    read_conn: Mutex<Connection>,
    /// Channel sender for non-blocking main-thread writes
    tx: Sender<StorageOp>,
    /// Background worker thread handle
    #[allow(dead_code)]
    worker_handle: Mutex<Option<JoinHandle<()>>>,
}

impl SqliteStorageEngine {
    /// Returns the database file path.
    pub fn path(&self) -> &Path {
        &self.db_path
    }

    /// Opens or creates the SQLite database in WAL mode and starts the background worker.
    pub fn open(db_path: impl AsRef<Path>) -> Result<Arc<Self>, StorageError> {
        let path = db_path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        // Initialize schema and configure WAL mode
        {
            let init_conn = Connection::open(&path)
                .map_err(|e| StorageError::Backend(format!("Failed to open DB for init: {e}")))?;
            init_conn
                .execute_batch(
                    "
                    PRAGMA journal_mode = WAL;
                    PRAGMA synchronous = NORMAL;
                    PRAGMA busy_timeout = 5000;
                    CREATE TABLE IF NOT EXISTS goldsrc_kv (
                        bucket TEXT NOT NULL,
                        key TEXT NOT NULL,
                        val BLOB NOT NULL,
                        updated_at INTEGER NOT NULL,
                        PRIMARY KEY (bucket, key)
                    );
                    CREATE INDEX IF NOT EXISTS idx_goldsrc_kv_bucket ON goldsrc_kv(bucket);
                    ",
                )
                .map_err(|e| StorageError::Backend(format!("Failed to init schema: {e}")))?;
        }

        let read_conn = Connection::open(&path)
            .map_err(|e| StorageError::Backend(format!("Failed to open read DB: {e}")))?;
        read_conn
            .execute_batch(
                "
                PRAGMA journal_mode = WAL;
                PRAGMA synchronous = NORMAL;
                PRAGMA busy_timeout = 5000;
                ",
            )
            .map_err(|e| StorageError::Backend(format!("Failed to configure read DB: {e}")))?;

        let (tx, rx) = unbounded();
        let worker_path = path.clone();
        let worker_handle = thread::Builder::new()
            .name("goldsrc-storage-worker".to_string())
            .spawn(move || {
                Self::worker_loop(worker_path, rx);
            })
            .map_err(|e| StorageError::Backend(format!("Failed to spawn storage thread: {e}")))?;

        Ok(Arc::new(Self {
            db_path: path,
            memory_cache: RwLock::new(HashMap::new()),
            read_conn: Mutex::new(read_conn),
            tx,
            worker_handle: Mutex::new(Some(worker_handle)),
        }))
    }

    /// Background worker routine executing batched SQLite transactions.
    fn worker_loop(db_path: PathBuf, rx: Receiver<StorageOp>) {
        let mut conn = match Connection::open(&db_path) {
            Ok(c) => c,
            Err(e) => {
                log::error!(target: log_targets::STORAGE, "Storage worker failed to open SQLite: {e}");
                return;
            }
        };

        let _ = conn.execute_batch(
            "
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;
            PRAGMA busy_timeout = 5000;
            ",
        );

        let mut pending_batch = Vec::new();
        let mut last_flush = Instant::now();

        loop {
            // Wait up to 100ms for incoming ops, or process batch if interval elapsed
            let timeout = Duration::from_millis(100);
            match rx.recv_timeout(timeout) {
                Ok(StorageOp::Set { bucket, key, value }) => {
                    pending_batch.push(StorageOp::Set { bucket, key, value });
                }
                Ok(StorageOp::Delete { bucket, key }) => {
                    pending_batch.push(StorageOp::Delete { bucket, key });
                }
                Ok(StorageOp::Flush(ack_tx)) => {
                    if !pending_batch.is_empty() {
                        Self::commit_batch(&mut conn, &mut pending_batch);
                    }
                    let _ = ack_tx.send(());
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                    if !pending_batch.is_empty()
                        && last_flush.elapsed() >= Duration::from_millis(500)
                    {
                        Self::commit_batch(&mut conn, &mut pending_batch);
                        last_flush = Instant::now();
                    }
                }
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                    // Drain remaining ops on shutdown
                    while let Ok(op) = rx.try_recv() {
                        match op {
                            StorageOp::Set { bucket, key, value } => {
                                pending_batch.push(StorageOp::Set { bucket, key, value });
                            }
                            StorageOp::Delete { bucket, key } => {
                                pending_batch.push(StorageOp::Delete { bucket, key });
                            }
                            StorageOp::Flush(ack_tx) => {
                                let _ = ack_tx.send(());
                            }
                        }
                    }
                    if !pending_batch.is_empty() {
                        Self::commit_batch(&mut conn, &mut pending_batch);
                    }
                    break;
                }
            }

            if pending_batch.len() >= 500
                || (!pending_batch.is_empty() && last_flush.elapsed() >= Duration::from_millis(500))
            {
                Self::commit_batch(&mut conn, &mut pending_batch);
                last_flush = Instant::now();
            }
        }
    }

    /// Commits a batch of operations inside a single SQLite transaction.
    fn commit_batch(conn: &mut Connection, batch: &mut Vec<StorageOp>) {
        if batch.is_empty() {
            return;
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        let tx_result = conn.transaction();
        match tx_result {
            Ok(tx) => {
                {
                    let mut insert_stmt = match tx.prepare_cached(
                        "INSERT OR REPLACE INTO goldsrc_kv (bucket, key, val, updated_at) VALUES (?1, ?2, ?3, ?4)"
                    ) {
                        Ok(s) => s,
                        Err(e) => {
                            log::error!(target: log_targets::STORAGE, "Failed to prepare insert stmt: {e}");
                            batch.clear();
                            return;
                        }
                    };

                    let mut delete_stmt = match tx
                        .prepare_cached("DELETE FROM goldsrc_kv WHERE bucket = ?1 AND key = ?2")
                    {
                        Ok(s) => s,
                        Err(e) => {
                            log::error!(target: log_targets::STORAGE, "Failed to prepare delete stmt: {e}");
                            batch.clear();
                            return;
                        }
                    };

                    for op in batch.drain(..) {
                        match op {
                            StorageOp::Set { bucket, key, value } => {
                                let _ = insert_stmt.execute(params![bucket, key, value, now]);
                            }
                            StorageOp::Delete { bucket, key } => {
                                let _ = delete_stmt.execute(params![bucket, key]);
                            }
                            _ => {}
                        }
                    }
                }
                if let Err(e) = tx.commit() {
                    log::error!(target: log_targets::STORAGE, "Failed to commit storage batch transaction: {e}");
                }
            }
            Err(e) => {
                log::error!(target: log_targets::STORAGE, "Failed to begin storage transaction: {e}");
                batch.clear();
            }
        }
    }

    /// Asynchronously requests a flush of pending in-flight writes to disk without blocking the caller.
    pub fn try_flush_async(&self) -> Result<(), StorageError> {
        let (ack_tx, _) = crossbeam_channel::bounded(1);
        self.tx
            .try_send(StorageOp::Flush(ack_tx))
            .map_err(|e| StorageError::Backend(format!("Async flush send failed: {e}")))?;
        Ok(())
    }

    /// Synchronously flushes all pending in-flight writes to disk.
    /// Reserved for server shutdown and map transitions (`ServerDeactivate`).
    pub fn flush(&self) -> Result<(), StorageError> {
        let (ack_tx, ack_rx) = crossbeam_channel::bounded(1);
        self.tx
            .send(StorageOp::Flush(ack_tx))
            .map_err(|e| StorageError::Backend(format!("Flush send failed: {e}")))?;

        ack_rx
            .recv_timeout(Duration::from_secs(2))
            .map_err(|e| StorageError::Backend(format!("Flush timeout or error: {e}")))?;
        Ok(())
    }
}

impl StoragePort for SqliteStorageEngine {
    fn get(&self, bucket: &str, key: &str) -> Result<Option<Vec<u8>>, StorageError> {
        // 1. Check in-memory cache first (Zero-alloc: borrows &str keys directly)
        {
            let cache = self.memory_cache.read().unwrap_or_else(|e| e.into_inner());
            if let Some(bucket_map) = cache.get(bucket)
                && let Some(entry) = bucket_map.get(key)
            {
                return Ok(entry.clone());
            }
        }

        // 2. Query SQLite read connection
        let loaded_bytes = {
            let conn = self.read_conn.lock().unwrap_or_else(|e| e.into_inner());
            let mut stmt = conn
                .prepare_cached("SELECT val FROM goldsrc_kv WHERE bucket = ?1 AND key = ?2")
                .map_err(|e| StorageError::Backend(e.to_string()))?;

            let mut rows = stmt
                .query(params![bucket, key])
                .map_err(|e| StorageError::Backend(e.to_string()))?;

            if let Some(row) = rows
                .next()
                .map_err(|e| StorageError::Backend(e.to_string()))?
            {
                let bytes: Vec<u8> = row
                    .get(0)
                    .map_err(|e| StorageError::Backend(e.to_string()))?;
                Some(bytes)
            } else {
                None
            }
        };

        // 3. Populate read cache without holding read_conn mutex (prevents AB-BA deadlock)
        if let Some(bytes) = loaded_bytes {
            self.memory_cache
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .entry(bucket.to_string())
                .or_default()
                .insert(key.to_string(), Some(bytes.clone()));
            Ok(Some(bytes))
        } else {
            Ok(None)
        }
    }

    fn set(&self, bucket: &str, key: &str, value: &[u8]) -> Result<(), StorageError> {
        let val_vec = value.to_vec();
        // 1. Update memory cache immediately so subsequent reads in same frame see new value
        self.memory_cache
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .entry(bucket.to_string())
            .or_default()
            .insert(key.to_string(), Some(val_vec.clone()));

        // 2. Dispatch non-blocking write to background worker
        self.tx
            .send(StorageOp::Set {
                bucket: bucket.to_string(),
                key: key.to_string(),
                value: val_vec,
            })
            .map_err(|e| StorageError::Backend(format!("Failed to enqueue storage write: {e}")))?;

        Ok(())
    }

    fn delete(&self, bucket: &str, key: &str) -> Result<bool, StorageError> {
        // 1. Update memory cache immediately
        self.memory_cache
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .entry(bucket.to_string())
            .or_default()
            .insert(key.to_string(), None);

        // 2. Dispatch delete
        self.tx
            .send(StorageOp::Delete {
                bucket: bucket.to_string(),
                key: key.to_string(),
            })
            .map_err(|e| StorageError::Backend(format!("Failed to enqueue storage delete: {e}")))?;

        Ok(true)
    }

    fn fetch_add(&self, bucket: &str, key: &str, delta: i64) -> Result<i64, StorageError> {
        // 1. Check if the value is in memory cache first
        let cached_opt = {
            let cache = self.memory_cache.read().unwrap_or_else(|e| e.into_inner());
            cache.get(bucket).and_then(|m| m.get(key)).cloned()
        };

        // 2. If not cached, load from SQLite read_conn WITHOUT holding memory_cache lock (prevents AB-BA deadlock)
        let base_val = match cached_opt {
            Some(Some(bytes)) => {
                let arr: [u8; 8] = bytes.as_slice().try_into().map_err(|_| {
                    StorageError::Backend(format!(
                        "Corrupt 64-bit integer blob for key '{key}' in bucket '{bucket}' (expected 8 bytes, found {})",
                        bytes.len()
                    ))
                })?;
                i64::from_le_bytes(arr)
            }
            Some(None) => 0,
            None => {
                let conn = self.read_conn.lock().unwrap_or_else(|e| e.into_inner());
                let mut stmt = conn
                    .prepare_cached("SELECT val FROM goldsrc_kv WHERE bucket = ?1 AND key = ?2")
                    .map_err(|e| StorageError::Backend(e.to_string()))?;
                let mut rows = stmt
                    .query(params![bucket, key])
                    .map_err(|e| StorageError::Backend(e.to_string()))?;
                if let Some(row) = rows
                    .next()
                    .map_err(|e| StorageError::Backend(e.to_string()))?
                {
                    let bytes: Vec<u8> = row
                        .get(0)
                        .map_err(|e| StorageError::Backend(e.to_string()))?;
                    let arr: [u8; 8] = bytes.as_slice().try_into().map_err(|_| {
                        StorageError::Backend(format!(
                            "Corrupt 64-bit integer blob for key '{key}' in bucket '{bucket}' (expected 8 bytes, found {})",
                            bytes.len()
                        ))
                    })?;
                    i64::from_le_bytes(arr)
                } else {
                    0
                }
            }
        };

        // 3. Re-acquire memory cache write lock to perform atomic update
        let mut cache = self.memory_cache.write().unwrap_or_else(|e| e.into_inner());
        let current_val = match cache.get(bucket).and_then(|m| m.get(key)) {
            Some(Some(bytes)) => {
                let arr: [u8; 8] = bytes.as_slice().try_into().map_err(|_| {
                    StorageError::Backend(format!(
                        "Corrupt 64-bit integer blob for key '{key}' in bucket '{bucket}' (expected 8 bytes, found {})",
                        bytes.len()
                    ))
                })?;
                i64::from_le_bytes(arr)
            }
            Some(None) => 0,
            None => base_val,
        };

        let new_val = current_val.checked_add(delta).ok_or_else(|| {
            StorageError::Backend(format!(
                "Integer overflow on fetch_add: current {current_val} + delta {delta}"
            ))
        })?;
        let new_bytes = new_val.to_le_bytes().to_vec();

        cache
            .entry(bucket.to_string())
            .or_default()
            .insert(key.to_string(), Some(new_bytes.clone()));

        self.tx
            .send(StorageOp::Set {
                bucket: bucket.to_string(),
                key: key.to_string(),
                value: new_bytes,
            })
            .map_err(|e| StorageError::Backend(format!("Failed to enqueue atomic write: {e}")))?;

        Ok(new_val)
    }
}

/// Policy trait for serializing and deserializing bucket data.
pub trait StorageFormat {
    /// Serializes a value into byte representation.
    fn encode<T: serde::Serialize + ?Sized>(value: &T) -> Result<Vec<u8>, StorageError>;
    /// Deserializes a value from byte representation.
    fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, StorageError>;
}

/// Standard human-readable JSON serialization format (default policy).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JsonFormat;

impl StorageFormat for JsonFormat {
    fn encode<T: serde::Serialize + ?Sized>(value: &T) -> Result<Vec<u8>, StorageError> {
        serde_json::to_vec(value).map_err(|e| StorageError::Serialization(e.to_string()))
    }

    fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, StorageError> {
        serde_json::from_slice(bytes).map_err(|e| StorageError::Serialization(e.to_string()))
    }
}

/// Strongly-typed Key-Value Bucket facade for plugins (framework layer).
#[derive(Clone)]
pub struct Bucket<T, F: StorageFormat = JsonFormat> {
    provider: Arc<dyn StoragePort>,
    name: String,
    _marker: PhantomData<(T, F)>,
}

impl<T, F: StorageFormat> Bucket<T, F> {
    /// Creates a new typed bucket handle with the specified format policy.
    pub fn new<S: Into<String>>(provider: Arc<dyn StoragePort>, name: S) -> Self {
        Self {
            provider,
            name: name.into(),
            _marker: PhantomData,
        }
    }

    /// Returns the bucket name.
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl<T: serde::de::DeserializeOwned, F: StorageFormat> Bucket<T, F> {
    /// Retrieves and deserializes a value by key using the configured [`StorageFormat`].
    pub fn get(&self, key: &str) -> Result<Option<T>, StorageError> {
        let raw = self.provider.get(&self.name, key)?;
        match raw {
            Some(bytes) => {
                let val: T = F::decode(&bytes)?;
                Ok(Some(val))
            }
            None => Ok(None),
        }
    }
}

impl<T: serde::Serialize, F: StorageFormat> Bucket<T, F> {
    /// Serializes and stores a value by key using the configured [`StorageFormat`].
    pub fn set(&self, key: &str, value: &T) -> Result<(), StorageError> {
        let bytes = F::encode(value)?;
        self.provider.set(&self.name, key, &bytes)
    }

    /// Deletes a key from the bucket.
    pub fn delete(&self, key: &str) -> Result<bool, StorageError> {
        self.provider.delete(&self.name, key)
    }
}

impl<F: StorageFormat> Bucket<i64, F> {
    /// Atomically increments/decrements an integer balance and returns the new value.
    pub fn fetch_add(&self, key: &str, delta: i64) -> Result<i64, StorageError> {
        self.provider.fetch_add(&self.name, key, delta)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sqlite_storage_engine_crud() {
        let unique_id = format!(
            "goldsrc_test_db_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let temp_dir = std::env::temp_dir().join(unique_id);
        let db_file = temp_dir.join("test.db");

        let engine = SqliteStorageEngine::open(&db_file).unwrap();

        // 1. Set
        engine.set("vip_menu", "STEAM_0:0:1", b"vip_gold").unwrap();

        // 2. Immediate read from cache
        let val = engine.get("vip_menu", "STEAM_0:0:1").unwrap();
        assert_eq!(val, Some(b"vip_gold".to_vec()));

        // 3. Flush to disk
        engine.flush().unwrap();

        // 4. Atomic fetch_add
        let new_bal = engine.fetch_add("bank", "STEAM_0:0:1", 1000).unwrap();
        assert_eq!(new_bal, 1000);

        let new_bal2 = engine.fetch_add("bank", "STEAM_0:0:1", -300).unwrap();
        assert_eq!(new_bal2, 700);

        // 5. Delete
        engine.delete("vip_menu", "STEAM_0:0:1").unwrap();
        assert_eq!(engine.get("vip_menu", "STEAM_0:0:1").unwrap(), None);

        // 6. Test Bucket with custom format policy
        #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
        struct PlayerProfile {
            name: String,
            level: u32,
        }

        let bucket = Bucket::<PlayerProfile, JsonFormat>::new(engine, "profiles");
        let profile = PlayerProfile {
            name: "Player1".to_string(),
            level: 42,
        };

        bucket.set("STEAM_0:0:1", &profile).unwrap();
        let loaded = bucket.get("STEAM_0:0:1").unwrap();
        assert_eq!(loaded, Some(profile));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_concurrent_storage_operations_no_deadlock() {
        use std::sync::Arc;
        let unique_id = format!(
            "goldsrc_test_storage_concurrent_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let temp_dir = std::env::temp_dir().join(unique_id);
        let db_file = temp_dir.join("concurrent.db");

        let engine = Arc::new(SqliteStorageEngine::open(&db_file).unwrap());

        // Spawn 8 worker threads racing get and fetch_add across shared keys
        let mut handles = Vec::new();
        for t in 0..8 {
            let eng = Arc::clone(&engine);
            let handle = std::thread::spawn(move || {
                for i in 0..100 {
                    let key = format!("user_{}", (t + i) % 5);
                    let _ = eng.fetch_add("wallet", &key, 10);
                    let _ = eng.get("wallet", &key);
                }
            });
            handles.push(handle);
        }

        for h in handles {
            h.join().expect("Worker thread panicked or deadlocked");
        }

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_storage_poison_recovery_and_corrupt_blob_rejection() {
        use std::sync::Arc;
        let unique_id = format!(
            "goldsrc_test_storage_poison_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let temp_dir = std::env::temp_dir().join(unique_id);
        let db_file = temp_dir.join("poison.db");

        let engine = Arc::new(SqliteStorageEngine::open(&db_file).unwrap());

        // 1. Deliberately poison the memory_cache RwLock in a child thread
        let eng_poison = Arc::clone(&engine);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut _lock = eng_poison.memory_cache.write().unwrap();
            panic!("Intentional thread panic to poison RwLock");
        }));

        // Assert memory_cache is poisoned
        assert!(engine.memory_cache.read().is_err());

        // 2. Verify get() and set() survive and operate normally despite poisoned lock
        assert!(engine.set("poison_test", "key1", b"val1").is_ok());
        let res = engine.get("poison_test", "key1");
        assert!(res.is_ok());
        assert_eq!(res.unwrap(), Some(b"val1".to_vec()));

        // 3. Test corrupt BLOB rejection on fetch_add (4 bytes instead of 8 bytes)
        assert!(engine.set("counters", "corrupt_key", &[1, 2, 3, 4]).is_ok());
        let fetch_res = engine.fetch_add("counters", "corrupt_key", 10);
        assert!(
            fetch_res.is_err(),
            "Expected fetch_add on corrupt BLOB to return Err"
        );

        // 4. Test checked_add overflow protection
        assert!(
            engine
                .set("counters", "overflow_key", &i64::MAX.to_le_bytes())
                .is_ok()
        );
        let overflow_res = engine.fetch_add("counters", "overflow_key", 1);
        assert!(
            overflow_res.is_err(),
            "Expected integer overflow to return Err"
        );

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
