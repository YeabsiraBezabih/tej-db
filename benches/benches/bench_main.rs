#![allow(clippy::unwrap_used)]

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use std::ops::Bound;
use tej_db::{Database, DurabilityMode, KeyRange, WriteBatch};
use tej_db_pager::{CompressedStorage, EncryptedStorage, FileStorage};
use tempfile::tempdir;

fn bench_mem_insert(c: &mut Criterion) {
    c.bench_function("mem_storage_insert_1000", |b| {
        b.iter(|| {
            let db = Database::open_memory().unwrap();
            let mut tx = db.begin_write().unwrap();
            for i in 0..1000u32 {
                let k = i.to_le_bytes();
                let v = (i * 2).to_le_bytes();
                tx.put(&k, &v).unwrap();
            }
            tx.commit().unwrap();
        });
    });
}

fn bench_mem_point_lookup(c: &mut Criterion) {
    let db = Database::open_memory().unwrap();
    let mut tx = db.begin_write().unwrap();
    for i in 0..1000u32 {
        let k = i.to_le_bytes();
        let v = (i * 2).to_le_bytes();
        tx.put(&k, &v).unwrap();
    }
    tx.commit().unwrap();

    let rx = db.begin_read().unwrap();
    c.bench_function("mem_storage_point_lookup", |b| {
        b.iter(|| {
            for i in 0..1000u32 {
                let k = black_box(i.to_le_bytes());
                let val = rx.get(&k).unwrap();
                black_box(val);
            }
        });
    });
}

fn bench_file_and_mmap_storage(c: &mut Criterion) {
    let mut group = c.benchmark_group("storage_backends");

    group.bench_function("file_posix_insert_500", |b| {
        b.iter(|| {
            let temp_dir = tempdir().unwrap();
            let db_path = temp_dir.path().join("bench_file.db");
            let db = Database::open_file(&db_path).unwrap();
            db.set_durability(DurabilityMode::Normal);
            let mut tx = db.begin_write().unwrap();
            for i in 0..500u32 {
                tx.put(&i.to_be_bytes(), &(i * 3).to_be_bytes()).unwrap();
            }
            tx.commit().unwrap();
        });
    });

    #[cfg(unix)]
    group.bench_function("file_mmap_insert_500", |b| {
        b.iter(|| {
            let temp_dir = tempdir().unwrap();
            let db_path = temp_dir.path().join("bench_mmap.db");
            let db = Database::open_mmap(&db_path).unwrap();
            db.set_durability(DurabilityMode::Normal);
            let mut tx = db.begin_write().unwrap();
            for i in 0..500u32 {
                tx.put(&i.to_be_bytes(), &(i * 3).to_be_bytes()).unwrap();
            }
            tx.commit().unwrap();
        });
    });

    group.finish();
}

fn bench_batch_writes(c: &mut Criterion) {
    let mut group = c.benchmark_group("batch_writes");

    let count = 500u32;
    let items: Vec<(Vec<u8>, Vec<u8>)> = (0..count)
        .map(|i| (i.to_be_bytes().to_vec(), (i * 2).to_be_bytes().to_vec()))
        .collect();

    group.bench_function("individual_put_loop_500", |b| {
        b.iter(|| {
            let db = Database::open_memory().unwrap();
            let mut tx = db.begin_write().unwrap();
            for (k, v) in &items {
                tx.put(k, v).unwrap();
            }
            tx.commit().unwrap();
        });
    });

    group.bench_function("write_batch_api_500", |b| {
        b.iter(|| {
            let db = Database::open_memory().unwrap();
            let mut batch = WriteBatch::with_capacity(count as usize);
            for (k, v) in &items {
                batch.put(k.clone(), v.clone());
            }
            db.write_batch(batch).unwrap();
        });
    });

    group.finish();
}

fn bench_range_scans(c: &mut Criterion) {
    let mut group = c.benchmark_group("range_scans");

    let db = Database::open_memory().unwrap();
    let mut tx = db.begin_write().unwrap();
    for i in 0..2000u32 {
        tx.put(&i.to_be_bytes(), &(i * 2).to_be_bytes()).unwrap();
    }
    tx.commit().unwrap();

    let rx = db.begin_read().unwrap();

    for size in [10u32, 100u32, 1000u32] {
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &sz| {
            let start = 100u32.to_be_bytes();
            let end = (100u32 + sz).to_be_bytes();
            let range = KeyRange {
                start: Bound::Included(start.to_vec()),
                end: Bound::Excluded(end.to_vec()),
            };

            b.iter(|| {
                let items = rx.scan_range(black_box(&range)).unwrap();
                black_box(items);
            });
        });
    }

    group.finish();
}

fn bench_durability_levels(c: &mut Criterion) {
    let mut group = c.benchmark_group("durability_levels");
    group.sample_size(10); // fsync is slow

    group.bench_function("durability_immediate_fsync", |b| {
        b.iter(|| {
            let temp_dir = tempdir().unwrap();
            let db_path = temp_dir.path().join("imm.db");
            let db = Database::open_file(&db_path).unwrap();
            db.set_durability(DurabilityMode::Immediate);
            let mut tx = db.begin_write().unwrap();
            tx.put(b"k", b"v").unwrap();
            tx.commit().unwrap();
        });
    });

    group.bench_function("durability_normal_flush", |b| {
        b.iter(|| {
            let temp_dir = tempdir().unwrap();
            let db_path = temp_dir.path().join("norm.db");
            let db = Database::open_file(&db_path).unwrap();
            db.set_durability(DurabilityMode::Normal);
            let mut tx = db.begin_write().unwrap();
            tx.put(b"k", b"v").unwrap();
            tx.commit().unwrap();
        });
    });

    group.bench_function("durability_eventual_flusher", |b| {
        b.iter(|| {
            let temp_dir = tempdir().unwrap();
            let db_path = temp_dir.path().join("eve.db");
            let db = Database::open_file(&db_path).unwrap();
            db.set_durability(DurabilityMode::Eventual { interval_ms: 20 });
            let mut tx = db.begin_write().unwrap();
            tx.put(b"k", b"v").unwrap();
            tx.commit().unwrap();
        });
    });

    group.bench_function("durability_none_inmemory", |b| {
        b.iter(|| {
            let temp_dir = tempdir().unwrap();
            let db_path = temp_dir.path().join("none.db");
            let db = Database::open_file(&db_path).unwrap();
            db.set_durability(DurabilityMode::None);
            let mut tx = db.begin_write().unwrap();
            tx.put(b"k", b"v").unwrap();
            tx.commit().unwrap();
        });
    });

    group.finish();
}

fn bench_storage_transforms(c: &mut Criterion) {
    let mut group = c.benchmark_group("storage_transforms");

    let count = 400u32;
    let payload = vec![0x77u8; 128];

    group.bench_function("plain_posix_storage", |b| {
        b.iter(|| {
            let temp_dir = tempdir().unwrap();
            let db_path = temp_dir.path().join("plain.db");
            let db = Database::open_file(&db_path).unwrap();
            db.set_durability(DurabilityMode::Normal);
            let mut tx = db.begin_write().unwrap();
            for i in 0..count {
                tx.put(&i.to_be_bytes(), &payload).unwrap();
            }
            tx.commit().unwrap();
        });
    });

    group.bench_function("lz4_compressed_storage", |b| {
        b.iter(|| {
            let temp_dir = tempdir().unwrap();
            let db_path = temp_dir.path().join("lz4.db");
            let raw = FileStorage::open(&db_path).unwrap();
            let comp = CompressedStorage::new(raw);
            let db = Database::open_custom(comp, 200).unwrap();
            db.set_durability(DurabilityMode::Normal);
            let mut tx = db.begin_write().unwrap();
            for i in 0..count {
                tx.put(&i.to_be_bytes(), &payload).unwrap();
            }
            tx.commit().unwrap();
        });
    });

    group.bench_function("chacha20_encrypted_storage", |b| {
        b.iter(|| {
            let temp_dir = tempdir().unwrap();
            let db_path = temp_dir.path().join("crypto.db");
            let raw = FileStorage::open(&db_path).unwrap();
            let enc = EncryptedStorage::new(raw, [0x5Au8; 32]);
            let db = Database::open_custom(enc, 200).unwrap();
            db.set_durability(DurabilityMode::Normal);
            let mut tx = db.begin_write().unwrap();
            for i in 0..count {
                tx.put(&i.to_be_bytes(), &payload).unwrap();
            }
            tx.commit().unwrap();
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_mem_insert,
    bench_mem_point_lookup,
    bench_file_and_mmap_storage,
    bench_batch_writes,
    bench_range_scans,
    bench_durability_levels,
    bench_storage_transforms
);
criterion_main!(benches);
