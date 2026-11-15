use super::*;

#[test]
fn test_bitpacking_roundtrip_basic() {
    let original = vec![1000, 1005, 1002, 1010, 1001, 1007, 1000];
    let compressed = BitPackingCodec::encode(&original);
    assert!(compressed.len() < original.len() * 8);

    let decompressed = BitPackingCodec::decode(&compressed).unwrap();
    assert_eq!(decompressed, original);
}

#[test]
fn test_bitpacking_constant_values() {
    let original = vec![42; 100];
    let compressed = BitPackingCodec::encode(&original);
    let decompressed = BitPackingCodec::decode(&compressed).unwrap();
    assert_eq!(decompressed, original);
}

#[test]
fn test_bitpacking_negative_and_spans() {
    let original = vec![-100, -50, 0, 50, 100, -80, 20];
    let compressed = BitPackingCodec::encode(&original);
    let decompressed = BitPackingCodec::decode(&compressed).unwrap();
    assert_eq!(decompressed, original);
}

#[test]
#[allow(clippy::similar_names)]
fn test_bitpacking_specialized_widths() {
    // Width 1: span 1 (0 or 1)
    let w1: Vec<i64> = (0..65).map(|i| i % 2).collect();
    let enc1 = BitPackingCodec::encode(&w1);
    assert_eq!(BitPackingCodec::decode(&enc1).unwrap(), w1);

    // Width 4: span 10 (0..10)
    let w4: Vec<i64> = (0..105).map(|i| i % 10).collect();
    let enc4 = BitPackingCodec::encode(&w4);
    assert_eq!(BitPackingCodec::decode(&enc4).unwrap(), w4);

    // Width 6: span 50 (0..50)
    let w6: Vec<i64> = (0..205).map(|i| i % 50).collect();
    let enc6 = BitPackingCodec::encode(&w6);
    assert_eq!(BitPackingCodec::decode(&enc6).unwrap(), w6);

    // Width 8: span 200 (0..200)
    let w8: Vec<i64> = (0..500).map(|i| i % 200).collect();
    let enc8 = BitPackingCodec::encode(&w8);
    assert_eq!(BitPackingCodec::decode(&enc8).unwrap(), w8);

    // Width 10: span 730 (0..730)
    let w10: Vec<i64> = (0..305).map(|i| i % 730).collect();
    let enc10 = BitPackingCodec::encode(&w10);
    assert_eq!(BitPackingCodec::decode(&enc10).unwrap(), w10);

    // Width 16: span 40000 (0..40000)
    let w16: Vec<i64> = (0..500).map(|i| i * 70).collect();
    let enc16 = BitPackingCodec::encode(&w16);
    assert_eq!(BitPackingCodec::decode(&enc16).unwrap(), w16);
}

#[test]
fn test_rle_roundtrip() {
    let mut original = Vec::new();
    original.extend(std::iter::repeat_n(10, 50));
    original.extend(std::iter::repeat_n(20, 100));
    original.extend(std::iter::repeat_n(30, 200));

    let compressed = RleCodec::encode(&original);
    // 3 runs * 12 bytes + 4 bytes header = 40 bytes (vs 350 * 8 = 2800 bytes!)
    assert_eq!(compressed.len(), 40);

    let decompressed = RleCodec::decode(&compressed).unwrap();
    assert_eq!(decompressed, original);
}

#[test]
fn test_dictionary_roundtrip() {
    let items = vec![
        "apple", "banana", "apple", "cherry", "banana", "apple", "date", "cherry",
    ];
    let compressed = DictionaryCodec::encode(&items);
    let decompressed = DictionaryCodec::decode(&compressed).unwrap();
    let decompressed_refs: Vec<&str> = decompressed.iter().map(String::as_str).collect();
    assert_eq!(decompressed_refs, items);
}
