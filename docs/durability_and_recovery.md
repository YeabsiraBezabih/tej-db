# Durability Modes & Crash Recovery

TejDb provides configurable durability tiers balancing ACID persistence guarantees against raw write performance. This document details the 4-tier durability spectrum, the Dual Ping-Pong Superblock architecture, Leader-Follower Group Commit, the torn-write repair protocol, and power-loss recovery mechanics.

---

## 1. The Durability Spectrum

Durability is parameterized via the `DurabilityMode` enum:

```rust
pub enum DurabilityMode {
    Immediate,
    Normal,
    Eventual { interval_ms: u64 },
    None,
}
```

```mermaid
flowchart TD
    subgraph DurabilityLevels ["Durability Spectrum"]
        direction LR
        D1["DurabilityMode::Immediate<br/>Leader-Follower Group fsync()<br/>Guaranteed Power-Loss Durability"]
        D2["DurabilityMode::Normal<br/>pwrite64() to OS page cache<br/>Crash-Safe against Process Kill"]
        D3["DurabilityMode::Eventual { interval_ms }<br/>pwrite64() + Background sync()<br/>Bounded Data Loss Window"]
        D4["DurabilityMode::None<br/>Dirty pages kept in PageCache<br/>Ephemeral In-Memory Speed"]
    end

    style D1 fill:#e8f5e9,stroke:#2e7d32,stroke-width:2px;
    style D2 fill:#e1f5fe,stroke:#0288d1,stroke-width:2px;
    style D3 fill:#fff3e0,stroke:#f57c00,stroke-width:2px;
    style D4 fill:#fce4ec,stroke:#c2185b,stroke-width:2px;
```

### Operational Characteristics

| Durability Tier | Commit Flush Behavior | Survival Guarantee | Target Workload |
| :--- | :--- | :--- | :--- |
| **`Immediate`** | Leader-Follower Group Commit: Coalesces concurrent writers into shared `fdatasync()` / barrier. | Tolerates sudden OS kernel panic, power outage, or hardware failure. | Financial ledgers, high-consequence transaction systems. |
| **`Normal`** | Writes dirty pages to storage file (`pwrite64`), returns immediately without synchronous `fsync()`. | Tolerates application crash or `SIGKILL`. Tolerates power outage up to OS sync interval. | General database applications, high-throughput microservices. |
| **`Eventual { interval_ms }`** | Writes dirty pages to storage file; dedicated `tej-db-flusher` background thread calls `sync()` at set intervals. | Tolerates application crash. Upper bound on power-loss data loss is `interval_ms`. | Metrics telemetry, time-series, log ingestion. |
| **`None`** | No storage writes on commit; dirty pages stay in RAM buffer pool until evicted or shutdown. | Process-lifetime only unless explicit `db.flush()` is called. | Unit testing, ephemeral caching, analytical scratchpads. |

---

## 2. Dual Ping-Pong Superblock Architecture (Page 0 & Page 1)

In shadow-paging and Copy-on-Write engines (such as LMDB and Redb), overwriting a single root meta page in-place introduces a catastrophic torn-write window: if power is interrupted mid-write, the root checksum fails, leading to unrecoverable database loss.

To eliminate this vulnerability, TejDb reserves **Page 0 (`SUPERBLOCK_A`)** and **Page 1 (`SUPERBLOCK_B`)** as alternating ping-pong superblocks.

```mermaid
flowchart LR
    subgraph PingPong ["Alternating Superblock Commitment"]
        direction TB
        SB0["Page 0: SUPERBLOCK_A<br/>tx_id = 42 (Committed)"]
        SB1["Page 1: SUPERBLOCK_B<br/>tx_id = 41 (Previous Valid)"]
    end

    NewTx["Commit Tx 43"] -->|Target Older Slot| SB1New["Write to Page 1<br/>tx_id = 43 + fdatasync()"]
    SB1New --> Swap["Active Root Swapped to Page 1"]

    style SB0 fill:#e8f5e9,stroke:#2e7d32,stroke-width:2px;
    style SB1 fill:#e1f5fe,stroke:#0288d1,stroke-width:2px;
    style SB1New fill:#fff3e0,stroke:#f57c00,stroke-width:2px;
```

### Binary Layout (4,096 Bytes)

```
+-------------------------------------------------------------------------------+
|                       Slotted Page Header (64 Bytes)                          |
|  PageType::Meta (0x04) | Checksum (CRC32C) | LSN | Free Space Offset          |
+-------------------------------------------------------------------------------+
| Bytes 0..4   : Magic Bytes ("TEJ_DB")                                           |
| Bytes 4..8   : Version (CURRENT_DB_VERSION = 2)                               |
| Bytes 8..16  : Active Root PageId (Little-Endian u64)                         |
| Bytes 16..24 : Catalog Root PageId (Dynamic B+ Tree Catalog Root)             |
| Bytes 24..32 : Next Transaction ID (Monotonic Little-Endian u64)              |
| Bytes 32..34 : Table Directory Inline Count (Reserved for Fast Path)          |
+-------------------------------------------------------------------------------+
| Inline Table Directory Cache (Up to Header Limit)                             |
| Dynamic Catalog B+ Tree manages infinite tables beyond the 4 KiB header limit  |
+-------------------------------------------------------------------------------+
| Unused Reserved Payload (Padded to 4,096 Bytes)                               |
+-------------------------------------------------------------------------------+
```

### Ping-Pong Slot Selection & Recovery Protocol
1. **Commit Slot Selection**: Every write transaction inspects the current active superblock slot. It writes the newly committed transaction metadata (`active_root`, `catalog_root`, `next_tx_id + 1`) to the *alternate* superblock slot, issues `fdatasync()`, and only then publishes the active slot index via an `AtomicU64`.
2. **Crash & Power-Loss Recovery**:
   - Both `SUPERBLOCK_A` (Page 0) and `SUPERBLOCK_B` (Page 1) are read and validated for CRC32C checksum and magic header bytes.
   - If a torn write occurred while updating one slot during sudden power loss, that slot's CRC32 checksum fails. TejDb seamlessly adopts the other valid slot containing the previous consistent database state.
   - If both slots are valid, the engine selects the slot with the higher `next_tx_id`.
   - Never falls back to initializing a clean database if existing data is present.

---

## 3. Dynamic Catalog Root B+ Tree Migration

In early versions, the table catalog was constrained inside the 4 KiB payload of Page 0, imposing a hard ceiling on table count and sharded partitions.

TejDb decouples table definitions from superblock limits by storing the table catalog inside a dedicated Copy-on-Write B+ Tree (`catalog_root`):
- **Unlimited Tables & Shards**: System tables, user tables, and intra-table partitions are stored as dynamic entries in the catalog B+ Tree.
- **Fast-Path Inline Caching**: The most frequently accessed table roots remain cached in the superblock header for instant bootstrap without extra I/O hops.
- **ACID DDL**: `CREATE TABLE` and `DROP TABLE` modify the catalog tree within the same atomic commit boundary as user data mutations.

---

## 4. Leader-Follower Group Commit (`DurabilityMode::Immediate`)

Synchronous `fsync()` / `fdatasync()` calls incur severe latency penalties under heavy concurrent write loads (dropping throughput on virtualized and Docker filesystems).

TejDb addresses this with **Leader-Follower Group Commit**:
```mermaid
sequenceDiagram
    autonumber
    participant W1 as Writer 1 (Leader)
    participant W2 as Writer 2 (Follower)
    participant W3 as Writer 3 (Follower)
    participant Coord as GroupCommitCoordinator
    participant Disk as Storage Hardware

    W1->>Coord: Register commit request (tx_id = 101)
    Note over W1: Elected Leader
    W2->>Coord: Register commit request (tx_id = 102)
    Note over W2: Registered as Follower
    W3->>Coord: Register commit request (tx_id = 103)
    Note over W3: Registered as Follower

    W1->>Disk: Single fdatasync() flush (coalesced 101..103)
    Disk-->>W1: Sync complete
    W1->>Coord: Notify followers up to tx_id = 103
    Coord-->>W2: Wakeup: Committed
    Coord-->>W3: Wakeup: Committed
```

Concurrent writers entering the commit phase register their pending transaction ID into a lock-free queue. The first writer becomes the group leader and executes a single storage sync barrier (`fdatasync`). All waiting followers are notified and return success in a single round-trip, amortizing disk barrier costs over dozens of concurrent transactions.

---

## 5. Crash Recovery & Torn-Write Healing

A torn write occurs when power is severed midway through writing a sector, leaving trailing partial bytes at the file boundary.

### Recovery Workflow (`Database::recover_file`)

```mermaid
flowchart TD
    Start["1. Open File & Inspect Metadata"] --> InspectLen["2. Check Length: file_len % 4096 == 0?"]
    
    InspectLen -- "Aligned (Clean)" --> LoadSuperblocks["4. Inspect Dual Superblocks (Pages 0 & 1)"]
    InspectLen -- "Unaligned (Torn Write Detected)" --> TrimFile["3. Truncate Trailing Bytes<br/>set_len(len - unaligned)"]
    
    TrimFile --> LoadSuperblocks
    LoadSuperblocks --> CheckValid{"Any Valid Superblock?"}
    
    CheckValid -- "Both Valid" --> SelectHighest["Adopt Superblock with Highest tx_id"]
    CheckValid -- "One Valid (Torn Superblock)" --> AdoptValid["Adopt Valid Superblock<br/>Heals Power-Loss Torn Write"]
    CheckValid -- "Neither Valid" --> CheckEmpty{"Total Pages == 0 or Zeroed?"}
    
    CheckEmpty -- "Empty File" --> InitClean["Initialize Clean Database"]
    CheckEmpty -- "Existing Data" --> FatalCorrupt["Report Fatal Corruption<br/>Zero Risk of Data Wipe"]
    
    SelectHighest --> Reconstruct["5. Load Catalog Tree & Restore Table Roots"]
    AdoptValid --> Reconstruct
    Reconstruct --> Ready["Database Ready for Transactions"]

    style Start fill:#e1f5fe,stroke:#0288d1,stroke-width:2px;
    style TrimFile fill:#ffebee,stroke:#c62828,stroke-width:2px;
    style Ready fill:#e8f5e9,stroke:#2e7d32,stroke-width:2px;
    style FatalCorrupt fill:#ffcdd2,stroke:#b71c1c,stroke-width:2px;
```

---

## 6. Background Flusher Mechanics (`tej-db-flusher`)

When running in `DurabilityMode::Eventual { interval_ms }`, a background daemon thread maintains periodic disk synchronization without blocking worker threads:

```mermaid
sequenceDiagram
    autonumber
    participant App as Application Writers
    participant Cache as PageCache (RAM)
    participant Disk as Persistent Disk
    participant Flusher as tej-db-flusher Daemon Thread

    loop Worker Commits
        App->>Cache: Commit mutations (pwrite64)
        Cache-->>App: Return Ok(()) immediately
    end

    loop Periodic Background Flush
        Flusher->>Flusher: sleep(interval_ms)
        Flusher->>Cache: cache.sync()
        Cache->>Disk: Flush all dirty buffers to OS / hardware
        Disk-->>Cache: Disk write barrier satisfied
    end
```

The flusher thread uses an atomic stop flag (`AtomicBool`) to cleanly finish any in-flight disk sync operations upon database shutdown or mode reconfiguration.
