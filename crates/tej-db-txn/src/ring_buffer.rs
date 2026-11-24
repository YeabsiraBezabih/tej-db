use smallvec::SmallVec;
use std::fmt;
use std::sync::atomic::{AtomicU32, AtomicU64};
use tej_db_btree::RawBatchItem;

pub const RING_BUFFER_CAPACITY: usize = 4096;
pub const RING_BUFFER_MASK: usize = RING_BUFFER_CAPACITY - 1;

pub const SLOT_EMPTY: u32 = 0;
pub const SLOT_WRITING: u32 = 1;
pub const SLOT_READY: u32 = 2;
pub const SLOT_COMMITTED: u32 = 3;
pub const SLOT_CONFLICT: u32 = 4;
pub const SLOT_ABORTED: u32 = 5;

/// Cache-line padded slot (64 bytes aligned) eliminating hardware MESI false sharing.
#[repr(align(64))]
pub struct IngestSlot {
    pub status: AtomicU32,
    pub ticket: AtomicU64,
    pub snapshot_tx_id: AtomicU64,
    pub assigned_tx_id: AtomicU64,
    pub conflict_tx_id: AtomicU64,
    pub writes: parking_lot::Mutex<Vec<RawBatchItem>>,
    pub modified_tables: parking_lot::Mutex<SmallVec<[String; 2]>>,
    pub tx_pages: parking_lot::Mutex<tej_db_btree::TxPageSet>,
}

impl fmt::Debug for IngestSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IngestSlot")
            .field(
                "status",
                &self.status.load(std::sync::atomic::Ordering::Relaxed),
            )
            .field(
                "ticket",
                &self.ticket.load(std::sync::atomic::Ordering::Relaxed),
            )
            .finish_non_exhaustive()
    }
}

impl IngestSlot {
    pub fn new() -> Self {
        Self {
            status: AtomicU32::new(SLOT_EMPTY),
            ticket: AtomicU64::new(0),
            snapshot_tx_id: AtomicU64::new(0),
            assigned_tx_id: AtomicU64::new(0),
            conflict_tx_id: AtomicU64::new(0),
            writes: parking_lot::Mutex::new(Vec::with_capacity(32)),
            modified_tables: parking_lot::Mutex::new(SmallVec::new()),
            tx_pages: parking_lot::Mutex::new(tej_db_btree::TxPageSet::default()),
        }
    }
}

impl Default for IngestSlot {
    fn default() -> Self {
        Self::new()
    }
}

/// Zero-allocation, lock-free circular ring buffer (LMAX Disruptor Architecture).
pub struct DisruptorRingBuffer {
    pub head: AtomicU64,
    pub tail: AtomicU64,
    slots: Box<[IngestSlot]>,
}

impl fmt::Debug for DisruptorRingBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DisruptorRingBuffer")
            .field(
                "head",
                &self.head.load(std::sync::atomic::Ordering::Relaxed),
            )
            .field(
                "tail",
                &self.tail.load(std::sync::atomic::Ordering::Relaxed),
            )
            .field("capacity", &RING_BUFFER_CAPACITY)
            .finish_non_exhaustive()
    }
}

impl Default for DisruptorRingBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl DisruptorRingBuffer {
    pub fn new() -> Self {
        let mut slots_vec = Vec::with_capacity(RING_BUFFER_CAPACITY);
        for _ in 0..RING_BUFFER_CAPACITY {
            slots_vec.push(IngestSlot::new());
        }
        Self {
            head: AtomicU64::new(0),
            tail: AtomicU64::new(0),
            slots: slots_vec.into_boxed_slice(),
        }
    }

    #[inline]
    pub fn get_slot(&self, ticket: u64) -> &IngestSlot {
        let idx = (ticket as usize) & RING_BUFFER_MASK;
        &self.slots[idx]
    }
}
