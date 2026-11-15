use crate::chunk::DataChunk;
use crate::row_group::RowGroup;
use crate::storage::RowGroupStorage;
use crate::vector::{ColumnVector, PhysicalType};
use crate::zonemap::ZoneMap;

#[test]
fn test_column_vector_crud_and_nulls() {
    let mut vec = ColumnVector::new(PhysicalType::Int64, 10);
    vec.push_i64(10).unwrap();
    vec.push_null().unwrap();
    vec.push_i64(30).unwrap();

    assert_eq!(vec.len(), 3);
    assert_eq!(vec.null_count(), 1);
    assert_eq!(vec.get_i64(0), Some(10));
    assert_eq!(vec.get_i64(1), None);
    assert!(vec.is_null(1));
    assert_eq!(vec.get_i64(2), Some(30));
}

#[test]
fn test_column_vector_strings() {
    let mut vec = ColumnVector::new(PhysicalType::String, 10);
    vec.push_str("hello").unwrap();
    vec.push_str("world").unwrap();
    vec.push_null().unwrap();

    assert_eq!(vec.len(), 3);
    assert_eq!(vec.get_str(0), Some("hello"));
    assert_eq!(vec.get_str(1), Some("world"));
    assert_eq!(vec.get_str(2), None);
    assert!(vec.is_null(2));
}

#[test]
fn test_zonemap_evaluation() {
    let mut vec = ColumnVector::new(PhysicalType::Int64, 10);
    for i in 100..200 {
        vec.push_i64(i).unwrap();
    }

    let zm = ZoneMap::build(&vec);
    assert!(zm.can_match_eq_i64(150));
    assert!(!zm.can_match_eq_i64(50));
    assert!(!zm.can_match_eq_i64(250));

    assert!(zm.can_match_range_i64(Some(120), Some(180)));
    assert!(!zm.can_match_range_i64(Some(210), Some(300)));
    assert!(!zm.can_match_range_i64(Some(0), Some(50)));

    assert!(!zm.can_match_null());
    assert!(zm.can_match_not_null());
}

#[test]
fn test_row_group_compression_and_roundtrip() {
    let types = vec![PhysicalType::Int64, PhysicalType::String];
    let mut chunk = DataChunk::new(&types, 100);

    let col0 = chunk.column_mut(0).unwrap();
    for i in 0..100 {
        // Repeated status integers (ideal for RLE / bitpacking)
        col0.push_i64(i / 10).unwrap();
    }

    let col1 = chunk.column_mut(1).unwrap();
    for i in 0..100 {
        // Repeated strings (ideal for dictionary)
        let s = if i % 2 == 0 { "EVEN" } else { "ODD" };
        col1.push_str(s).unwrap();
    }

    chunk.set_num_rows(100).unwrap();

    // 1. Compress to RowGroup
    let rg = RowGroup::from_chunk(&chunk).unwrap();
    assert_eq!(rg.metadata.num_rows, 100);

    // 2. Storage roundtrip
    let bytes = RowGroupStorage::serialize(&rg).unwrap();
    let loaded_rg = RowGroupStorage::deserialize(&bytes).unwrap();

    // 3. Decompress to DataChunk
    let decompressed = loaded_rg.decompress_to_chunk().unwrap();
    assert_eq!(decompressed.num_rows(), 100);

    let d_col0 = decompressed.column(0).unwrap();
    assert_eq!(d_col0.get_i64(0), Some(0));
    assert_eq!(d_col0.get_i64(99), Some(9));

    let d_col1 = decompressed.column(1).unwrap();
    assert_eq!(d_col1.get_str(0), Some("EVEN"));
    assert_eq!(d_col1.get_str(1), Some("ODD"));
}

#[test]
fn test_csv_streaming_parse() {
    use crate::external::{CsvHeader, CsvReader};

    let csv_content = "id,name,score,active\n\
                       1,Alice,95.5,true\n\
                       2,Bob,82.0,false\n\
                       3,Charlie,NULL,true\n";

    let types = vec![
        PhysicalType::Int64,
        PhysicalType::String,
        PhysicalType::Float64,
        PhysicalType::Boolean,
    ];

    let reader = CsvReader::new(b',', CsvHeader::Present, types);
    let chunk = reader.parse_str(csv_content, 10).unwrap();

    assert_eq!(chunk.num_rows(), 3);
    assert_eq!(chunk.column(0).unwrap().get_i64(0), Some(1));
    assert_eq!(chunk.column(1).unwrap().get_str(1), Some("Bob"));
    assert_eq!(chunk.column(2).unwrap().get_f64(0), Some(95.5));
    assert!(chunk.column(2).unwrap().is_null(2)); // Charlie NULL score
    assert_eq!(chunk.column(3).unwrap().get_bool(0), Some(true));
    assert_eq!(chunk.column(3).unwrap().get_bool(1), Some(false));
}

#[test]
fn test_arrow_array_view_export() {
    use crate::external::ArrowArrayView;

    let mut vec = ColumnVector::new(PhysicalType::Int64, 10);
    vec.push_i64(100).unwrap();
    vec.push_i64(200).unwrap();

    let arrow_view = ArrowArrayView::from_i64_vector(&vec);
    assert_eq!(arrow_view.length, 2);
    assert_eq!(arrow_view.null_count, 0);
    assert_eq!(arrow_view.n_buffers, 2);
    assert!(!arrow_view.buffers[1].is_null());
}
