//! Unified SQLite WAL Engine & Async MPSC Batch Worker.
//!
//! Re-exported from [`goldsrc_service_storage`].

pub use goldsrc_service_storage::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_storage_reexport() {
        let temp_dir = std::env::temp_dir();
        let db_file = temp_dir.join(format!(
            "goldsrc_test_storage_reexport_{}.db",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let engine = SqliteStorageEngine::open(&db_file).unwrap();
        let bucket = Bucket::<String>::new(engine, "test_reexport");
        bucket.set("greeting", &"hello world".to_string()).unwrap();
        let val = bucket.get("greeting").unwrap();
        assert_eq!(val, Some("hello world".to_string()));

        let _ = std::fs::remove_file(&db_file);
    }
}
