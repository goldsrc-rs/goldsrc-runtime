//! Background telemetry collector archiving engine frame ticks into `.stitch` binary containers.
//!
//! Conforms to Scrooge Systems Mindset: batch reads from `wire-ring` ring buffers,
//! zero interference with the main server tick loop, and compact chunked persistence.

use crate::ring::{TelemetryRingConsumer, TickMetricsPacket};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;
use wire_ring::storage::HeapStorage;

/// Configuration for the background telemetry collector.
#[derive(Debug, Clone)]
pub struct CollectorConfig {
    /// Destination directory where `.stitch` container files are written.
    pub output_dir: PathBuf,
    /// Batch drain polling interval (typically 50-200ms).
    pub poll_interval: Duration,
    /// Maximum ticks per chunk before closing a chunk (e.g. 1000 ticks = 1 second at 1000 FPS).
    pub chunk_capacity: usize,
}

impl Default for CollectorConfig {
    fn default() -> Self {
        Self {
            output_dir: PathBuf::from("telemetry"),
            poll_interval: Duration::from_millis(100),
            chunk_capacity: 1000,
        }
    }
}

/// Simple CRC32 implementation (Castagnoli polynomial 0x82F63B78).
fn crc32_castagnoli(bytes: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in bytes {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if (crc & 1) != 0 {
                (crc >> 1) ^ 0x82F6_3B78
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// Background service worker collecting ticks and writing Stitch Data containers.
pub struct TelemetryCollectorService {
    is_running: Arc<AtomicBool>,
    worker_thread: Option<JoinHandle<usize>>,
}

impl TelemetryCollectorService {
    /// Spawns the background telemetry collector thread.
    pub fn spawn(
        mut consumer: TelemetryRingConsumer<HeapStorage>,
        config: CollectorConfig,
        file_path: impl AsRef<Path>,
    ) -> std::io::Result<Self> {
        let path = file_path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let is_running = Arc::new(AtomicBool::new(true));
        let running_flag = Arc::clone(&is_running);

        let handle = std::thread::Builder::new()
            .name("goldsrc-telemetry-collector".to_string())
            .spawn(move || {
                let mut total_persisted = 0;
                let file = match File::create(&path) {
                    Ok(f) => f,
                    Err(e) => {
                        log::error!("Failed to create telemetry container file: {e}");
                        return 0;
                    }
                };
                let mut writer = BufWriter::with_capacity(64 * 1024, file);

                // Write 16-byte ContainerHeader: "STDC", version 1, flags 0, schema_hash (0x474F4C4453524301 = GOLDSRC1)
                let magic = *b"STDC";
                let version: u16 = 1;
                let flags: u16 = 0;
                let schema_hash: u64 = 0x474F_4C44_5352_4301; // "GOLDSRC1"

                let mut header = [0u8; 16];
                header[0..4].copy_from_slice(&magic);
                header[4..6].copy_from_slice(&version.to_le_bytes());
                header[6..8].copy_from_slice(&flags.to_le_bytes());
                header[8..16].copy_from_slice(&schema_hash.to_le_bytes());
                if writer.write_all(&header).is_err() {
                    return 0;
                }

                struct ChunkMeta {
                    first_key: u64,
                    file_offset: u64,
                    compressed_len: u32,
                    row_count: u32,
                }

                let mut chunks: Vec<ChunkMeta> = Vec::new();
                let mut current_chunk_ticks: Vec<TickMetricsPacket> =
                    Vec::with_capacity(config.chunk_capacity);
                let mut current_offset: u64 = 16;

                while running_flag.load(Ordering::Relaxed) || consumer.available_read() >= 64 {
                    let drained = consumer.drain_ticks(|tick| {
                        current_chunk_ticks.push(*tick);
                    });

                    if drained == 0 {
                        std::thread::sleep(config.poll_interval);
                        continue;
                    }

                    if (current_chunk_ticks.len() >= config.chunk_capacity
                        || !running_flag.load(Ordering::Relaxed))
                        && let Some(first) = current_chunk_ticks.first()
                    {
                        let first_key = first.timestamp_ns;
                        let row_count = current_chunk_ticks.len() as u32;
                        let payload_size = (current_chunk_ticks.len()
                            * core::mem::size_of::<TickMetricsPacket>())
                            as u32;

                        let chunk_start = current_offset;
                        for tick in &current_chunk_ticks {
                            let bytes = unsafe {
                                core::slice::from_raw_parts(
                                    tick as *const _ as *const u8,
                                    core::mem::size_of::<TickMetricsPacket>(),
                                )
                            };
                            let _ = writer.write_all(bytes);
                        }

                        chunks.push(ChunkMeta {
                            first_key,
                            file_offset: chunk_start,
                            compressed_len: payload_size,
                            row_count,
                        });

                        current_offset += payload_size as u64;
                        total_persisted += current_chunk_ticks.len();
                        current_chunk_ticks.clear();
                    }
                }

                // Flush remaining ticks if any
                if !current_chunk_ticks.is_empty() {
                    let first_key = current_chunk_ticks[0].timestamp_ns;
                    let row_count = current_chunk_ticks.len() as u32;
                    let payload_size = (current_chunk_ticks.len()
                        * core::mem::size_of::<TickMetricsPacket>())
                        as u32;

                    let chunk_start = current_offset;
                    for tick in &current_chunk_ticks {
                        let bytes = unsafe {
                            core::slice::from_raw_parts(
                                tick as *const _ as *const u8,
                                core::mem::size_of::<TickMetricsPacket>(),
                            )
                        };
                        let _ = writer.write_all(bytes);
                    }

                    chunks.push(ChunkMeta {
                        first_key,
                        file_offset: chunk_start,
                        compressed_len: payload_size,
                        row_count,
                    });

                    current_offset += payload_size as u64;
                    total_persisted += current_chunk_ticks.len();
                }

                // Write Directory: array of 24-byte ChunkDescriptors
                let directory_offset = current_offset;
                let mut dir_bytes = Vec::with_capacity(chunks.len() * 24);
                for c in &chunks {
                    dir_bytes.extend_from_slice(&c.first_key.to_le_bytes());
                    dir_bytes.extend_from_slice(&c.file_offset.to_le_bytes());
                    dir_bytes.extend_from_slice(&c.compressed_len.to_le_bytes());
                    dir_bytes.extend_from_slice(&c.row_count.to_le_bytes());
                }
                let _ = writer.write_all(&dir_bytes);

                // Write 16-byte ContainerFooter: directory_offset (8), chunk_count (4), directory_crc32 (4)
                let dir_crc = crc32_castagnoli(&dir_bytes);
                let mut footer = [0u8; 16];
                footer[0..8].copy_from_slice(&directory_offset.to_le_bytes());
                footer[8..12].copy_from_slice(&(chunks.len() as u32).to_le_bytes());
                footer[12..16].copy_from_slice(&dir_crc.to_le_bytes());
                let _ = writer.write_all(&footer);

                let _ = writer.flush();
                total_persisted
            })?;

        Ok(Self {
            is_running,
            worker_thread: Some(handle),
        })
    }

    /// Stops the collector thread and waits for all buffered ticks to be flushed to disk.
    pub fn stop_and_join(mut self) -> usize {
        self.is_running.store(false, Ordering::Release);
        if let Some(handle) = self.worker_thread.take() {
            handle.join().unwrap_or(0)
        } else {
            0
        }
    }
}
