use std::ffi::c_char;
use std::sync::Arc;
use tej_db::{DistanceMetric, VectorId, VectorIndex};

use crate::error::{FfiError, TEJ_OK, catch_ffi};
use crate::handle::{DbInstance, TejDbHandle, c_str_to_str, check_not_null, check_not_null_mut};

fn parse_metric(metric: i32) -> Result<DistanceMetric, FfiError> {
    match metric {
        0 => Ok(DistanceMetric::L2),
        1 => Ok(DistanceMetric::Cosine),
        2 => Ok(DistanceMetric::DotProduct),
        other => Err(FfiError::InvalidArg(format!(
            "Unknown distance metric code: {other}"
        ))),
    }
}

/// Inserts a dense vector into the table's ACID delta buffer.
///
/// # Safety
/// All pointers must be valid and non-null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tej_db_vector_insert(
    db: *mut TejDbHandle,
    table: *const c_char,
    vector_id: u64,
    vec_ptr: *const f32,
    dim: usize,
) -> i32 {
    catch_ffi(|| {
        check_not_null(db, "db")?;
        check_not_null(vec_ptr, "vec_ptr")?;
        if (vec_ptr as usize) % std::mem::align_of::<f32>() != 0 {
            return Err(FfiError::InvalidArg(
                "Pointer vec_ptr is not aligned for f32".into(),
            ));
        }
        if dim == 0 {
            return Err(FfiError::InvalidArg("Vector dimension cannot be 0".into()));
        }

        // SAFETY: Pointer is non-null and valid for dim f32 floats.
        let table_str = unsafe { c_str_to_str(table, "table")? };
        let vector = unsafe { std::slice::from_raw_parts(vec_ptr, dim) };
        let handle = unsafe { &*db };

        match &handle.0 {
            #[cfg(not(target_arch = "wasm32"))]
            DbInstance::File { db, .. } => {
                let v_idx = VectorIndex::new(
                    table_str,
                    dim,
                    DistanceMetric::L2,
                    Arc::clone(db.tx_manager()),
                );
                let mut tx = db.begin_write()?;
                v_idx.insert(&mut tx, VectorId(vector_id), vector)?;
                tx.commit()?;
            }
            DbInstance::Mem { db, .. } => {
                let v_idx = VectorIndex::new(
                    table_str,
                    dim,
                    DistanceMetric::L2,
                    Arc::clone(db.tx_manager()),
                );
                let mut tx = db.begin_write()?;
                v_idx.insert(&mut tx, VectorId(vector_id), vector)?;
                tx.commit()?;
            }
        }

        Ok(TEJ_OK)
    })
}

/// Compacts pending vector delta mutations into the persistent Vamana graph.
///
/// # Safety
/// `db` and `table` must be valid non-null pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tej_db_vector_compact(
    db: *mut TejDbHandle,
    table: *const c_char,
    dim: usize,
    metric: i32,
) -> i32 {
    catch_ffi(|| {
        check_not_null(db, "db")?;
        if dim == 0 {
            return Err(FfiError::InvalidArg("Vector dimension cannot be 0".into()));
        }

        let metric_enum = parse_metric(metric)?;
        // SAFETY: table is validated non-null.
        let table_str = unsafe { c_str_to_str(table, "table")? };
        let handle = unsafe { &*db };

        match &handle.0 {
            #[cfg(not(target_arch = "wasm32"))]
            DbInstance::File { db, .. } => {
                let v_idx =
                    VectorIndex::new(table_str, dim, metric_enum, Arc::clone(db.tx_manager()));
                v_idx.compact()?;
            }
            DbInstance::Mem { db, .. } => {
                let v_idx =
                    VectorIndex::new(table_str, dim, metric_enum, Arc::clone(db.tx_manager()));
                v_idx.compact()?;
            }
        }

        Ok(TEJ_OK)
    })
}

/// Searches the vector index using beam search, writing results into caller-allocated buffers.
///
/// # Safety
/// All pointers must be valid and non-null. Caller must ensure `out_ids` and `out_dists`
/// have capacity for at least `top_k` elements.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tej_db_vector_search(
    db: *mut TejDbHandle,
    table: *const c_char,
    dim: usize,
    metric: i32,
    query_ptr: *const f32,
    top_k: usize,
    out_ids: *mut u64,
    out_dists: *mut f32,
    out_count: *mut usize,
) -> i32 {
    catch_ffi(|| {
        check_not_null(db, "db")?;
        check_not_null(query_ptr, "query_ptr")?;
        check_not_null_mut(out_ids, "out_ids")?;
        check_not_null_mut(out_dists, "out_dists")?;
        check_not_null_mut(out_count, "out_count")?;

        if (query_ptr as usize) % std::mem::align_of::<f32>() != 0 {
            return Err(FfiError::InvalidArg(
                "Pointer query_ptr is not aligned for f32".into(),
            ));
        }
        if (out_ids as usize) % std::mem::align_of::<u64>() != 0 {
            return Err(FfiError::InvalidArg(
                "Pointer out_ids is not aligned for u64".into(),
            ));
        }
        if (out_dists as usize) % std::mem::align_of::<f32>() != 0 {
            return Err(FfiError::InvalidArg(
                "Pointer out_dists is not aligned for f32".into(),
            ));
        }
        if (out_count as usize) % std::mem::align_of::<usize>() != 0 {
            return Err(FfiError::InvalidArg(
                "Pointer out_count is not aligned for usize".into(),
            ));
        }

        if dim == 0 || top_k == 0 {
            return Err(FfiError::InvalidArg(
                "dim and top_k must be greater than 0".into(),
            ));
        }

        let metric_enum = parse_metric(metric)?;
        // SAFETY: Pointer is non-null and valid for dim f32 floats.
        let table_str = unsafe { c_str_to_str(table, "table")? };
        let query = unsafe { std::slice::from_raw_parts(query_ptr, dim) };
        let handle = unsafe { &*db };

        let beam_width = 16.max(top_k * 2);
        let candidates = match &handle.0 {
            #[cfg(not(target_arch = "wasm32"))]
            DbInstance::File { db, .. } => {
                let v_idx =
                    VectorIndex::new(table_str, dim, metric_enum, Arc::clone(db.tx_manager()));
                let rx = db.begin_read()?;
                v_idx.search(&rx, query, beam_width, top_k, None)?
            }
            DbInstance::Mem { db, .. } => {
                let v_idx =
                    VectorIndex::new(table_str, dim, metric_enum, Arc::clone(db.tx_manager()));
                let rx = db.begin_read()?;
                v_idx.search(&rx, query, beam_width, top_k, None)?
            }
        };

        let result_len = candidates.len().min(top_k);
        // SAFETY: Output pointers are validated non-null and guaranteed by caller to have capacity >= top_k.
        unsafe {
            for (i, cand) in candidates.iter().take(result_len).enumerate() {
                *out_ids.add(i) = cand.vector_id.0;
                *out_dists.add(i) = cand.distance;
            }
            *out_count = result_len;
        }

        Ok(TEJ_OK)
    })
}
