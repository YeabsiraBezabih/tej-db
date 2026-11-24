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

#[test]
fn test_bitpacking_extreme_i64_min_max() {
    let original = vec![i64::MIN, 0, i64::MAX, -1, 1, i64::MIN + 1, i64::MAX - 1];
    let compressed = BitPackingCodec::encode(&original);
    let decompressed = BitPackingCodec::decode(&compressed).unwrap();
    assert_eq!(decompressed, original);
}

#[test]
fn test_codecs_malicious_header_rejection() {
    // 1. Bitpack with huge count (1_000_000_000) but only 13 bytes
    let mut bad_bitpack = vec![0; 13];
    bad_bitpack[0..4].copy_from_slice(&(1_000_000_000u32).to_le_bytes());
    bad_bitpack[12] = 8; // 8 bits per item
    assert!(BitPackingCodec::decode(&bad_bitpack).is_err());

    // 2. RLE with huge run_len exceeding total_count
    let mut bad_rle = vec![0; 16];
    bad_rle[0..4].copy_from_slice(&(10u32).to_le_bytes()); // total_count = 10
    bad_rle[4..8].copy_from_slice(&(1_000_000u32).to_le_bytes()); // run_len = 1,000,000
    assert!(RleCodec::decode(&bad_rle).is_err());

    // 3. Dictionary with huge dict_size (0xFFFF_FFFF) on small byte buffer
    let mut bad_dict = vec![0; 12];
    bad_dict[0..4].copy_from_slice(&(10u32).to_le_bytes());
    bad_dict[4..8].copy_from_slice(&(u32::MAX).to_le_bytes());
    assert!(DictionaryCodec::decode(&bad_dict).is_err());
}
