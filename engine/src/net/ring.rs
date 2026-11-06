//! High-performance outbound network message ring buffer.
//!
//! Conforms to the "Scrooge" Systems Mindset:
//! - Backed by `wire-ring` virtual mirrored storage or heap ring buffer.
//! - Eliminates memory reallocations and memcpy shuffling on hot packet queues.
//! - Contiguous `WriteGrant` / `ReadGrant` for zero-copy message staging.

use std::sync::Mutex;
use wire_ring::builder::RingError;
use wire_ring::channel::{Consumer, Producer};
use wire_ring::presets::Ring;
use wire_ring::storage::{HeapStorage, StorageBackend};

/// Fixed header for an outbound GoldSrc network user message in the ring queue.
#[repr(C, align(8))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueuedNetMessageHeader {
    /// Target player index (1..=32) or 0 for broadcast (MessageDest::All).
    pub target_slot: i32,
    /// GoldSrc registered user message ID (e.g. TextMsg, SayText).
    pub msg_id: i32,
    /// Destination type (MessageDest enum).
    pub msg_dest: i32,
    /// Length of the following raw payload bytes in the ring buffer.
    pub payload_len: u32,
}

impl QueuedNetMessageHeader {
    pub const SIZE: usize = core::mem::size_of::<Self>();
}

/// Thread-safe lock-free SPSC outbound network packet staging channel.
pub struct OutboundNetRingQueue<S: StorageBackend = HeapStorage> {
    producer: Mutex<Producer<S>>,
    consumer: Mutex<Consumer<S>>,
}

impl OutboundNetRingQueue<HeapStorage> {
    /// Creates a new heap-backed outbound network queue with power-of-two capacity.
    pub fn new_heap(capacity: usize) -> Result<Self, RingError> {
        let (producer, consumer) = Ring::heap(capacity)?;
        Ok(Self {
            producer: Mutex::new(producer),
            consumer: Mutex::new(consumer),
        })
    }
}

impl<S: StorageBackend> OutboundNetRingQueue<S> {
    /// Enqueues an outbound network user message into the contiguous ring buffer without reallocation.
    pub fn enqueue_message(
        &self,
        target_slot: i32,
        msg_id: i32,
        msg_dest: i32,
        payload: &[u8],
    ) -> bool {
        let mut producer = self.producer.lock().unwrap();
        let total_size = QueuedNetMessageHeader::SIZE + payload.len();

        let mut grant = match producer.grant(total_size) {
            Some(g) => g,
            None => return false,
        };

        let header = QueuedNetMessageHeader {
            target_slot,
            msg_id,
            msg_dest,
            payload_len: payload.len() as u32,
        };

        // Write header
        let header_slice = unsafe {
            core::slice::from_raw_parts(
                &header as *const QueuedNetMessageHeader as *const u8,
                QueuedNetMessageHeader::SIZE,
            )
        };
        grant[..QueuedNetMessageHeader::SIZE].copy_from_slice(header_slice);

        // Write payload
        grant[QueuedNetMessageHeader::SIZE..total_size].copy_from_slice(payload);

        grant.commit(total_size);
        true
    }

    /// Drains and flushes all queued network messages directly to the GoldSrc engine port.
    pub fn flush_to_engine(&self, engine: &dyn goldsrc_spi::engine::EnginePort) -> usize {
        let mut consumer = self.consumer.lock().unwrap();
        let mut flushed_count = 0;

        loop {
            let available = consumer.available_read();
            if available < QueuedNetMessageHeader::SIZE {
                break;
            }

            let grant = match consumer.grant() {
                Some(g) => g,
                None => break,
            };
            let slice = &grant[..];

            let header = unsafe { *(slice.as_ptr() as *const QueuedNetMessageHeader) };
            let total_size = QueuedNetMessageHeader::SIZE + (header.payload_len as usize);

            if slice.len() < total_size {
                break;
            }

            let payload = &slice[QueuedNetMessageHeader::SIZE..total_size];

            // Send via GoldSrc engine port
            let target_opt = if header.target_slot > 0 {
                Some(header.target_slot)
            } else {
                None
            };

            engine.message_begin(header.msg_dest, header.msg_id, None, target_opt);
            for &byte in payload {
                engine.write_byte(byte as i32);
            }
            engine.message_end();

            grant.commit(total_size);
            flushed_count += 1;
        }

        flushed_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_queued_net_message_header_alignment() {
        assert_eq!(core::mem::size_of::<QueuedNetMessageHeader>(), 16);
        assert_eq!(core::mem::align_of::<QueuedNetMessageHeader>(), 8);
    }

    #[test]
    fn test_outbound_ring_enqueue_and_drain() {
        let queue = OutboundNetRingQueue::new_heap(4096).unwrap();

        let payload = b"Hello from wire-ring network buffer!";
        let enqueued = queue.enqueue_message(1, 100, 2, payload);
        assert!(enqueued);

        // Verify consumer sees data
        let mut consumer = queue.consumer.lock().unwrap();
        assert!(consumer.available_read() >= QueuedNetMessageHeader::SIZE + payload.len());

        let grant = consumer.grant().unwrap();
        let header = unsafe { *(grant.as_ptr() as *const QueuedNetMessageHeader) };
        assert_eq!(header.target_slot, 1);
        assert_eq!(header.msg_id, 100);
        assert_eq!(header.msg_dest, 2);
        assert_eq!(header.payload_len, payload.len() as u32);

        let data =
            &grant[QueuedNetMessageHeader::SIZE..QueuedNetMessageHeader::SIZE + payload.len()];
        assert_eq!(data, payload);
    }
}
