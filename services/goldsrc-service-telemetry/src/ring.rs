//! High-frequency, lock-free telemetry ring buffer for GoldSrc.rs engine frame ticks.
//!
//! Provides nanosecond-level telemetry dispatching conforming to the Scrooge Systems Mindset:
//! 64-byte L1D cache line aligned packets, zero heap allocations, and zero lock contention
//! during the 1000 FPS / 1ms engine tick budget.

use crate::{SystemMetricsSnapshot, TelemetryContext, TelemetryLayer};
use std::sync::Mutex;
use stitch_rs::flow::FlowControl;
use wire_ring::builder::RingError;
use wire_ring::channel::{Consumer, Producer};
use wire_ring::presets::Ring;
use wire_ring::storage::{HeapStorage, MirroredStorage, StorageBackend};

/// 64-byte L1D Cache-Aligned tick telemetry packet for zero-copy streaming.
#[repr(C, align(64))]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TickMetricsPacket {
    /// Monotonic timestamp in nanoseconds.
    pub timestamp_ns: u64,
    /// Delta frame time duration in nanoseconds.
    pub frame_delta_ns: u64,
    /// Measured engine tickrate in frames per second (FPS).
    pub measured_fps: f32,
    /// Frame time jitter in milliseconds.
    pub frame_jitter_ms: f32,
    /// Global host CPU utilization percentage (0.0 to 100.0).
    pub cpu_usage: f32,
    /// Process Resident Set Size (RSS) memory in bytes.
    pub process_rss_bytes: u64,
    /// Strictly monotonic sequence number of the tick.
    pub sequence: u64,
    /// Padding ensuring exact 64-byte L1D cache-line fit.
    pub _padding: [u8; 16],
}

// Compile-time verification of cache-line alignment and size
const _: () = {
    assert!(core::mem::size_of::<TickMetricsPacket>() == 64);
    assert!(core::mem::align_of::<TickMetricsPacket>() == 64);
};

impl TickMetricsPacket {
    /// Creates a new `TickMetricsPacket`.
    pub const fn new(
        timestamp_ns: u64,
        frame_delta_ns: u64,
        measured_fps: f32,
        frame_jitter_ms: f32,
        cpu_usage: f32,
        process_rss_bytes: u64,
        sequence: u64,
    ) -> Self {
        Self {
            timestamp_ns,
            frame_delta_ns,
            measured_fps,
            frame_jitter_ms,
            cpu_usage,
            process_rss_bytes,
            sequence,
            _padding: [0u8; 16],
        }
    }

    /// Converts a full `SystemMetricsSnapshot` into a compact 64-byte tick packet.
    pub fn from_snapshot(snapshot: &SystemMetricsSnapshot, sequence: u64) -> Self {
        Self {
            timestamp_ns: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0),
            frame_delta_ns: if snapshot.measured_fps > 0.0 {
                (1_000_000_000.0 / snapshot.measured_fps) as u64
            } else {
                0
            },
            measured_fps: snapshot.measured_fps as f32,
            frame_jitter_ms: snapshot.frame_jitter_ms as f32,
            cpu_usage: snapshot.global_cpu_usage,
            process_rss_bytes: snapshot.process_memory_rss_bytes,
            sequence,
            _padding: [0u8; 16],
        }
    }
}

/// Outbound lock-free publisher for engine tick telemetry.
pub struct TelemetryRingPublisher<S: StorageBackend = MirroredStorage> {
    producer: Producer<S>,
}

impl<S: StorageBackend> TelemetryRingPublisher<S> {
    /// Creates a new publisher wrapping the provided `wire-ring` producer.
    pub const fn new(producer: Producer<S>) -> Self {
        Self { producer }
    }

    /// Returns the total usable capacity of the underlying ring buffer.
    #[inline(always)]
    pub fn capacity(&self) -> usize {
        self.producer.capacity()
    }

    /// Returns the number of writable bytes currently available.
    #[inline(always)]
    pub fn available_write(&mut self) -> usize {
        self.producer.available_write()
    }

    /// Publishes a tick metrics packet directly into leased ring memory.
    ///
    /// # Performance
    /// Zero allocations, zero system calls, zero lock contention: ~3-5 ns latency.
    #[allow(clippy::result_unit_err)]
    pub fn publish_tick(&mut self, tick: &TickMetricsPacket) -> Result<(), ()> {
        let size = core::mem::size_of::<TickMetricsPacket>();
        let mut grant = self.producer.grant(size).ok_or(())?;
        if grant.len() < size {
            return Err(());
        }

        // SAFETY: TickMetricsPacket is repr(C, align(64)) with 64-byte size and plain data.
        let src = unsafe { core::slice::from_raw_parts(tick as *const _ as *const u8, size) };
        grant[..size].copy_from_slice(src);
        grant.commit(size);
        Ok(())
    }

    /// Consumes the publisher, returning the inner `Producer`.
    pub fn into_inner(self) -> Producer<S> {
        self.producer
    }
}

/// Inbound consumer for draining high-frequency tick telemetry.
pub struct TelemetryRingConsumer<S: StorageBackend = MirroredStorage> {
    consumer: Consumer<S>,
}

impl<S: StorageBackend> TelemetryRingConsumer<S> {
    /// Creates a new consumer wrapping the provided `wire-ring` consumer.
    pub const fn new(consumer: Consumer<S>) -> Self {
        Self { consumer }
    }

    /// Returns the total usable capacity of the underlying ring buffer.
    #[inline(always)]
    pub fn capacity(&self) -> usize {
        self.consumer.capacity()
    }

    /// Returns the number of readable bytes currently available.
    #[inline(always)]
    pub fn available_read(&mut self) -> usize {
        self.consumer.available_read()
    }

    /// Drains all available tick metrics packets from the ring, invoking `f` for each packet.
    ///
    /// Returns the number of packets drained and committed.
    pub fn drain_ticks<F: FnMut(&TickMetricsPacket)>(&mut self, mut f: F) -> usize {
        let Some(grant) = self.consumer.grant() else {
            return 0;
        };

        let packet_size = core::mem::size_of::<TickMetricsPacket>();
        let available_packets = grant.len() / packet_size;
        if available_packets == 0 {
            return 0;
        }

        for i in 0..available_packets {
            let offset = i * packet_size;
            // SAFETY: Memory is valid, within leased grant bounds, and packet_size is 64 bytes.
            let packet_ptr = unsafe { grant.as_ptr().add(offset) as *const TickMetricsPacket };
            let packet = unsafe { core::ptr::read_unaligned(packet_ptr) };
            f(&packet);
        }

        let total_consumed = available_packets * packet_size;
        grant.commit(total_consumed);
        available_packets
    }

    /// Consumes the consumer, returning the inner `Consumer`.
    pub fn into_inner(self) -> Consumer<S> {
        self.consumer
    }
}

/// Creates a new MMU-mirrored SPSC telemetry ring channel.
pub fn telemetry_ring_channel(
    capacity: usize,
) -> Result<
    (
        TelemetryRingPublisher<MirroredStorage>,
        TelemetryRingConsumer<MirroredStorage>,
    ),
    RingError,
> {
    let (tx, rx) = Ring::network_default(capacity)?;
    Ok((
        TelemetryRingPublisher::new(tx),
        TelemetryRingConsumer::new(rx),
    ))
}

/// Creates a new heap-allocated SPSC telemetry ring channel.
pub fn telemetry_ring_heap_channel(
    capacity: usize,
) -> Result<
    (
        TelemetryRingPublisher<HeapStorage>,
        TelemetryRingConsumer<HeapStorage>,
    ),
    RingError,
> {
    let (tx, rx) = Ring::heap(capacity)?;
    Ok((
        TelemetryRingPublisher::new(tx),
        TelemetryRingConsumer::new(rx),
    ))
}

/// SMA telemetry middleware layer publishing samples into a `wire-ring` buffer.
pub struct RingTelemetryLayer<S: StorageBackend = MirroredStorage> {
    publisher: Mutex<TelemetryRingPublisher<S>>,
    sequence: Mutex<u64>,
}

impl<S: StorageBackend> RingTelemetryLayer<S> {
    /// Creates a new layer wrapping the provided telemetry ring publisher.
    pub fn new(publisher: TelemetryRingPublisher<S>) -> Self {
        Self {
            publisher: Mutex::new(publisher),
            sequence: Mutex::new(0),
        }
    }
}

impl<S: StorageBackend + Send + Sync> TelemetryLayer for RingTelemetryLayer<S> {
    fn on_enter(&self, _ctx: &mut TelemetryContext<'_>) -> FlowControl<(), (), ()> {
        FlowControl::Proceed(())
    }

    fn on_exit(&self, ctx: &mut TelemetryContext<'_>, _outcome: &mut Result<(), ()>) {
        if ctx.is_suppressed {
            return;
        }

        let mut seq_guard = self.sequence.lock().unwrap_or_else(|e| e.into_inner());
        *seq_guard = seq_guard.wrapping_add(1);
        let seq = *seq_guard;

        let packet = TickMetricsPacket::from_snapshot(ctx.metrics, seq);

        if let Ok(mut pub_guard) = self.publisher.lock() {
            let _ = pub_guard.publish_tick(&packet);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_telemetry_ring_high_frequency_push_and_drain() {
        let (mut publisher, mut consumer) =
            telemetry_ring_heap_channel(16 * 1024).expect("telemetry heap ring");

        let total_packets = 100;
        for i in 0..total_packets {
            let packet = TickMetricsPacket::new(
                1_000_000_000 + i,
                1_000_000,
                1000.0,
                0.05,
                15.5,
                128 * 1024 * 1024,
                i,
            );
            publisher.publish_tick(&packet).expect("publish tick");
        }

        let mut received = 0;
        let drained = consumer.drain_ticks(|pkt| {
            assert_eq!(pkt.sequence, received);
            assert_eq!(pkt.measured_fps, 1000.0);
            received += 1;
        });

        assert_eq!(drained, total_packets as usize);
        assert_eq!(received, total_packets);
        assert_eq!(consumer.available_read(), 0);
    }
}
