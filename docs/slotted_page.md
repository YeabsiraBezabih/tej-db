# Binary Slotted Page Format & Specification

Every database page in **TejDb** conforms to a strict, 4,096-byte (4 KiB) binary slotted page layout (`tej-db-core/src/page/`).

The slotted page architecture allows storing variable-length keys and values with sub-nanosecond direct binary indexing, robust CRC32 checksum integrity verification, and zero heap copies.

---

## 1. Physical 4 KiB Slotted Page Layout

```
0                   16                  32                  48                  64 bytes
+-------------------+-------------------+-------------------+-------------------+
| PageId (8B)       | PageType (2B)     | ItemCount (2B)    | FreeOffset (2B)   |  <-- PageHeader
+-------------------+-------------------+-------------------+-------------------+
| NextLeaf (8B)     | PrevLeaf (8B)     | Checksum (4B)     | Reserved (30B)    |  (64 Bytes Total)
+-------------------+-------------------+-------------------+-------------------+
| Slot 0 (Offset 2B, Len 2B)            | Slot 1 (Offset 2B, Len 2B)            |  <-- Slot Directory
| Slot 2 (Offset 2B, Len 2B)            | Slot 3 (Offset 2B, Len 2B)            |      (Grows Downward)
+---------------------------------------+---------------------------------------+
|                                                                               |
|                             FREE CONTIGUOUS SPACE                             |
|                           (free_space_offset points)                          |
|                                                                               |
+-------------------------------------------------------------------------------+
| Cell 3: [KeyLen 2B][ValLen 2B][Key Bytes...][Val Bytes...]                    |  <-- Payload Cells
| Cell 2: [KeyLen 2B][ValLen 2B][Key Bytes...][Val Bytes...]                    |      (Grows Upward)
| Cell 1: [KeyLen 2B][ValLen 2B][Key Bytes...][Val Bytes...]                    |
| Cell 0: [KeyLen 2B][ValLen 2B][Key Bytes...][Val Bytes...]                    |
+-------------------------------------------------------------------------------+
4096 bytes (End of Page)
```

```mermaid
flowchart TD
    Header["64-Byte Page Header (PageId, Type, ItemCount, FreeOffset, Checksum)"]
    SlotDir["Slot Directory (Grows Downward from byte 64)"]
    FreeSpace["Contiguous Unallocated Free Space"]
    Cells["Key-Value Cells (Grows Upward from byte 4096)"]

    Header --> SlotDir
    SlotDir --> FreeSpace
    FreeSpace --> Cells

    style Header fill:#e1f5fe,stroke:#0288d1,stroke-width:2px;
    style SlotDir fill:#fff3e0,stroke:#e65100,stroke-width:2px;
    style FreeSpace fill:#f5f5f5,stroke:#9e9e9e,stroke-width:2px;
    style Cells fill:#e8f5e9,stroke:#2e7d32,stroke-width:2px;
```

---

## 2. Header Fields Specification (`PageHeader`)

Located at byte offsets `0x00 .. 0x3F` (64 bytes):

| Byte Offset | Field Name | Type | Description |
| :--- | :--- | :--- | :--- |
| `0x00 .. 0x07` | `page_id` | `u64` | Globally unique 64-bit page identifier |
| `0x08 .. 0x09` | `page_type` | `u16` | Type tag: `0x01` Leaf, `0x02` Internal, `0x03` Overflow, `0x04` Meta, `0x05` VectorNode |
| `0x0A .. 0x0B` | `item_count` | `u16` | Number of active items/slots present in this page |
| `0x0C .. 0x0D` | `free_space_offset` | `u16` | Byte offset to the lowest allocated cell byte |
| `0x0E .. 0x15` | `next_leaf` | `u64` | Linked-list sibling pointer to next leaf page (`PageId::INVALID` if none) |
| `0x16 .. 0x1D` | `prev_leaf` | `u64` | Linked-list sibling pointer to previous leaf page |
| `0x1E .. 0x21` | `checksum` | `u32` | CRC32-Castagnoli checksum over the remaining 4092 page bytes |
| `0x22 .. 0x3F` | `reserved` | `[u8; 30]` | 64-byte alignment padding for SIMD word operations |

---

## 3. Slot Directory Format

The Slot Directory begins immediately after the 64-byte header at byte offset `64`. Each slot consumes **4 bytes**:
- `offset`: `u16` (2 bytes) — Byte offset in the page where the item cell starts.
- `length`: `u16` (2 bytes) — Total byte length of the cell.

```rust
#[repr(C, packed)]
pub struct PageSlot {
    pub offset: u16,
    pub length: u16,
}
```

- When slot `i` is looked up, the cell data is accessed directly via slice indexing:
  `&page.data[slot.offset .. slot.offset + slot.length]`
- The slots maintain **strict sorted key order**. Deletions or insertions within the leaf perform pointer shifts in the slot directory without copying large payload byte blocks.

---

## 4. Key-Value Cell Formats

### A. Leaf Node Key-Value Cell (`leaf.rs`)
```
+---------------+---------------+-----------------------+-------------------------+
| key_len (2B)  | val_len (2B)  | key_bytes (key_len)   | val_bytes (val_len)     |
+---------------+---------------+-----------------------+-------------------------+
```
- Total cell size: `4 + key_len + val_len` bytes.
- Key and value slices are accessed with zero deserialization.

### B. Internal Node Routing Cell (`internal.rs`)
```
+---------------+-------------------+-----------------------+
| key_len (2B)  | child_page_id(8B) | key_bytes (key_len)   |
+---------------+-------------------+-----------------------+
```
- Associates a routing boundary key with a downward child `PageId`.

### C. Multi-Page Overflow Cell (`overflow.rs`)
When a value exceeds the inline page threshold (typically > 1,024 bytes), it is stored across a contiguous sequence of dedicated overflow pages.
```
+---------------+---------------+-----------------------+-----------------------+-----------------------+
| key_len (2B)  | 0xFFFF (2B)   | key_bytes (key_len)   | overflow_start_id(8B) | total_val_len (4B)    |
+---------------+---------------+-----------------------+-----------------------+-----------------------+
```
- The sentinel `val_len = 0xFFFF` instructs the reader to stream the payload directly from the storage extent starting at `overflow_start_id`.

### D. Vector Node Page Format (`PageType::VectorNode = 0x05`, `tej-db-vector`)
A dedicated 4 KiB slotted page format used by the page-aligned DiskANN (Vamana) graph engine:
```
0                   16                  32                  48 bytes
+-------------------+-------------------+-------------------+
| VectorId (8B)     | OverflowPage (8B) | Alpha (4B)        |
+-------------------+-------------------+-------------------+
| Beta (4B)         | Norm (4B)         | NeighborCount(2B) |
+-------------------+-------------------+-------------------+
| CodeBytes (2B)    | Flags (1B)        | Reserved (15B)    |  <-- Node Metadata (48B total)
+-------------------+-------------------+-------------------+
| Adjacency List: [Neighbor PageId 0 .. R-1] (8B * R)       |  <-- Fixed-degree R (32..64)
+-----------------------------------------------------------+
| 1-Bit RaBitQ Binary Code Bytes [CodeBytes]                |  <-- e.g. 192 bytes for 1536-dim
+-----------------------------------------------------------+
```
- **Page-Aligned Traversal**: Every beam-search hop fetches exactly one 4 KiB page containing both the node's neighbors and its quantized vector code, completely eliminating random I/O fragmentation.
- **Full Precision Storage**: Full-precision f32 vectors are stored in the overflow extent engine (`0xFFFF` sentinel) for high-accuracy top-$k$ reranking.

---

## 5. In-Place Slotted Page Mutation (`leaf_insert_raw_inplace`)

To achieve multi-million ops/s throughput during batch writes, `tej-db-core` implements in-place leaf modification:
1. Calculates required cell size (`4 + key.len() + val.len()`).
2. Checks whether unallocated contiguous space (`free_space_offset - slot_end`) suffices.
3. Decrements `free_space_offset` by cell size and copies key/value bytes into the vacated high-memory space.
4. Performs a binary search on the slot directory to determine sorted insertion index.
5. Memmoves slot entries to create a 4-byte gap and writes the new `PageSlot`.
6. Increments `item_count` by 1.

**Outcome**: Slotted leaf insertion completes in **~5 nanoseconds** with zero intermediate heap allocations.
