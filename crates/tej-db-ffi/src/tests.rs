#![allow(
    clippy::borrow_as_ptr,
    clippy::cast_precision_loss,
    clippy::useless_vec
)]

use std::ffi::{CStr, CString};
use std::ptr;
use tempfile::NamedTempFile;

use crate::*;

#[test]
fn test_ffi_open_memory_crud_lifecycle() {
    unsafe {
        let mut db: *mut TejDbHandle = ptr::null_mut();
        let rc = tej_db_open_memory(&mut db);
        assert_eq!(rc, TEJ_OK);
        assert!(!db.is_null());

        let table = CString::new("kv_table").expect("cstring");
        let key = b"user_key";
        let val = b"user_value_12345";

        // 1. Put
        let rc = tej_db_put(
            db,
            table.as_ptr(),
            key.as_ptr(),
            key.len(),
            val.as_ptr(),
            val.len(),
        );
        assert_eq!(rc, TEJ_OK);

        // 2. Get
        let mut val_out: *mut u8 = ptr::null_mut();
        let mut val_len: usize = 0;
        let rc = tej_db_get(
            db,
            table.as_ptr(),
            key.as_ptr(),
            key.len(),
            &mut val_out,
            &mut val_len,
        );
        assert_eq!(rc, TEJ_OK);
        assert_eq!(val_len, val.len());
        let slice = std::slice::from_raw_parts(val_out, val_len);
        assert_eq!(slice, val);

        // 3. Free buffer
        let rc = tej_db_free_buffer(val_out, val_len);
        assert_eq!(rc, TEJ_OK);

        // 4. Delete
        let rc = tej_db_delete(db, table.as_ptr(), key.as_ptr(), key.len());
        assert_eq!(rc, TEJ_OK);

        // 5. Get deleted key -> NotFound
        let mut val_out2: *mut u8 = ptr::null_mut();
        let mut val_len2: usize = 0;
        let rc = tej_db_get(
            db,
            table.as_ptr(),
            key.as_ptr(),
            key.len(),
            &mut val_out2,
            &mut val_len2,
        );
        assert_eq!(rc, TEJ_ERR_NOT_FOUND);

        // 6. Close
        let rc = tej_db_close(db);
        assert_eq!(rc, TEJ_OK);
    }
}

#[test]
fn test_ffi_file_backed_persistence() {
    let tmp = NamedTempFile::new().expect("temp file");
    let path_str = tmp.path().to_str().expect("valid utf8");
    let c_path = CString::new(path_str).expect("cstring");
    let table = CString::new("persisted_table").expect("cstring");

    unsafe {
        // Open, write, close
        {
            let mut db: *mut TejDbHandle = ptr::null_mut();
            let rc = tej_db_open(c_path.as_ptr(), &mut db);
            assert_eq!(rc, TEJ_OK);

            let rc = tej_db_put(
                db,
                table.as_ptr(),
                b"persisted_key".as_ptr(),
                13,
                b"persisted_val".as_ptr(),
                13,
            );
            assert_eq!(rc, TEJ_OK);

            let rc = tej_db_close(db);
            assert_eq!(rc, TEJ_OK);
        }

        // Reopen and read
        {
            let mut db: *mut TejDbHandle = ptr::null_mut();
            let rc = tej_db_open(c_path.as_ptr(), &mut db);
            assert_eq!(rc, TEJ_OK);

            let mut val_out: *mut u8 = ptr::null_mut();
            let mut val_len: usize = 0;
            let rc = tej_db_get(
                db,
                table.as_ptr(),
                b"persisted_key".as_ptr(),
                13,
                &mut val_out,
                &mut val_len,
            );
            assert_eq!(rc, TEJ_OK);
            assert_eq!(val_len, 13);
            let slice = std::slice::from_raw_parts(val_out, val_len);
            assert_eq!(slice, b"persisted_val");

            tej_db_free_buffer(val_out, val_len);
            tej_db_close(db);
        }
    }
}

#[test]
fn test_ffi_transactions() {
    unsafe {
        let mut db: *mut TejDbHandle = ptr::null_mut();
        tej_db_open_memory(&mut db);
        let table = CString::new("tx_table").expect("cstring");

        // Write transaction commit
        let mut wtx: *mut TejWriteTxHandle = ptr::null_mut();
        let rc = tej_db_begin_write(db, &mut wtx);
        assert_eq!(rc, TEJ_OK);

        tej_db_write_tx_put(wtx, table.as_ptr(), b"k1".as_ptr(), 2, b"v1".as_ptr(), 2);
        let rc = tej_db_write_tx_commit(wtx);
        assert_eq!(rc, TEJ_OK);

        // Read transaction
        let mut rtx: *mut TejReadTxHandle = ptr::null_mut();
        let rc = tej_db_begin_read(db, &mut rtx);
        assert_eq!(rc, TEJ_OK);

        let mut val_out: *mut u8 = ptr::null_mut();
        let mut val_len: usize = 0;
        let rc = tej_db_read_tx_get(
            rtx,
            table.as_ptr(),
            b"k1".as_ptr(),
            2,
            &mut val_out,
            &mut val_len,
        );
        assert_eq!(rc, TEJ_OK);
        assert_eq!(std::slice::from_raw_parts(val_out, val_len), b"v1");
        tej_db_free_buffer(val_out, val_len);
        tej_db_read_tx_close(rtx);

        // Write transaction abort
        let mut wtx2: *mut TejWriteTxHandle = ptr::null_mut();
        tej_db_begin_write(db, &mut wtx2);
        tej_db_write_tx_put(wtx2, table.as_ptr(), b"k2".as_ptr(), 2, b"v2".as_ptr(), 2);
        tej_db_write_tx_abort(wtx2);

        // Verify aborted key is absent
        let mut rtx2: *mut TejReadTxHandle = ptr::null_mut();
        tej_db_begin_read(db, &mut rtx2);
        let rc = tej_db_read_tx_get(
            rtx2,
            table.as_ptr(),
            b"k2".as_ptr(),
            2,
            &mut val_out,
            &mut val_len,
        );
        assert_eq!(rc, TEJ_ERR_NOT_FOUND);
        tej_db_read_tx_close(rtx2);

        tej_db_close(db);
    }
}

#[test]
fn test_ffi_sql_query() {
    unsafe {
        let mut db: *mut TejDbHandle = ptr::null_mut();
        tej_db_open_memory(&mut db);

        let ddl =
            CString::new("CREATE TABLE items (id INT PRIMARY KEY, name TEXT);").expect("cstring");
        let mut json_out: *mut std::ffi::c_char = ptr::null_mut();
        let rc = tej_db_sql_execute(db, ddl.as_ptr(), &mut json_out);
        assert_eq!(rc, TEJ_OK);
        tej_db_free_string(json_out);

        let insert = CString::new("INSERT INTO items VALUES (1, 'WidgetA'), (2, 'GadgetB');")
            .expect("cstring");
        let rc = tej_db_sql_execute(db, insert.as_ptr(), &mut json_out);
        assert_eq!(rc, TEJ_OK);
        tej_db_free_string(json_out);

        let query = CString::new("SELECT * FROM items;").expect("cstring");
        let rc = tej_db_sql_execute(db, query.as_ptr(), &mut json_out);
        assert_eq!(rc, TEJ_OK);
        let c_str = CStr::from_ptr(json_out);
        let json_text = c_str.to_str().expect("valid utf8");
        assert!(json_text.contains("WidgetA"));
        assert!(json_text.contains("GadgetB"));
        tej_db_free_string(json_out);

        tej_db_close(db);
    }
}

#[test]
fn test_ffi_vector_search() {
    unsafe {
        let mut db: *mut TejDbHandle = ptr::null_mut();
        tej_db_open_memory(&mut db);

        let table = CString::new("vectors").expect("cstring");
        for i in 0..10u64 {
            let mut vec = vec![0.0f32; 16];
            vec[0] = i as f32;
            vec[1] = (i * 2) as f32;
            let rc = tej_db_vector_insert(db, table.as_ptr(), i, vec.as_ptr(), 16);
            assert_eq!(rc, TEJ_OK);
        }

        let query = vec![0.0f32; 16];
        let mut out_ids = [0u64; 3];
        let mut out_dists = [0.0f32; 3];
        let mut out_count: usize = 0;

        let rc = tej_db_vector_search(
            db,
            table.as_ptr(),
            16,
            0, // L2
            query.as_ptr(),
            3,
            out_ids.as_mut_ptr(),
            out_dists.as_mut_ptr(),
            &mut out_count,
        );
        assert_eq!(rc, TEJ_OK);
        assert_eq!(out_count, 3);
        assert_eq!(out_ids[0], 0);

        let rc = tej_db_vector_compact(db, table.as_ptr(), 16, 0);
        assert_eq!(rc, TEJ_OK);

        tej_db_close(db);
    }
}

#[test]
fn test_ffi_error_handling_and_panic_safety() {
    unsafe {
        // Null pointer handling
        let mut db: *mut TejDbHandle = ptr::null_mut();
        let rc = tej_db_open(ptr::null(), &mut db);
        assert_eq!(rc, TEJ_ERR_NULL_PTR);

        let mut err_buf = [0i8; 128];
        let len = tej_db_last_error(err_buf.as_mut_ptr(), err_buf.len());
        assert!(len > 0);
        let err_str = CStr::from_ptr(err_buf.as_ptr())
            .to_str()
            .expect("valid utf8");
        assert!(err_str.contains("Null pointer"));

        tej_db_clear_last_error();
        let len2 = tej_db_last_error(err_buf.as_mut_ptr(), err_buf.len());
        assert_eq!(len2, 0);

        // Panic catching
        let panic_rc = catch_ffi(|| {
            panic!("Intentional test panic inside FFI");
        });
        assert_eq!(panic_rc, TEJ_ERR_PANIC);

        let len3 = tej_db_last_error(err_buf.as_mut_ptr(), err_buf.len());
        assert!(len3 > 0);
        let panic_msg = CStr::from_ptr(err_buf.as_ptr())
            .to_str()
            .expect("valid utf8");
        assert!(panic_msg.contains("Intentional test panic"));
    }
}
