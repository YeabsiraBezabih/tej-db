use super::*;
use std::ops::Bound;

#[test]
fn test_page_header_roundtrip() {
    let page_id = PageId(42);
    let mut page = Page::new(page_id, PageType::BTreeLeaf);

    let header = page.read_header().expect("Valid header");
    assert_eq!(header.page_id, page_id);
    assert_eq!(header.page_type, PageType::BTreeLeaf);
    assert!(page.verify_checksum().is_ok());

    // Modify payload and ensure checksum verification catches it
    page.payload_mut()[0] ^= 0xFF;
    assert!(page.verify_checksum().is_err());
}

#[test]
fn test_slotted_leaf_operations() {
    let mut leaf = Page::new_leaf(PageId(1), PageId::INVALID);
    assert_eq!(leaf.item_count(), 0);

    // Insert items out of order to verify sorted maintenance
    leaf = leaf
        .leaf_create_with_insert(PageId(1), b"k3", b"val3")
        .unwrap();
    leaf = leaf
        .leaf_create_with_insert(PageId(1), b"k1", b"val1")
        .unwrap();
    leaf = leaf
        .leaf_create_with_insert(PageId(1), b"k2", b"val2")
        .unwrap();

    assert_eq!(leaf.item_count(), 3);
    assert_eq!(leaf.leaf_get_key(0).unwrap(), b"k1");
    assert_eq!(leaf.leaf_get_val(0).unwrap(), b"val1");
    assert_eq!(leaf.leaf_get_key(1).unwrap(), b"k2");
    assert_eq!(leaf.leaf_get_val(1).unwrap(), b"val2");
    assert_eq!(leaf.leaf_get_key(2).unwrap(), b"k3");
    assert_eq!(leaf.leaf_get_val(2).unwrap(), b"val3");

    // Binary search
    assert_eq!(leaf.leaf_search(b"k2").unwrap(), Ok(1));
    assert_eq!(leaf.leaf_search(b"k0").unwrap(), Err(0));
    assert_eq!(leaf.leaf_search(b"k15").unwrap(), Err(1));
    assert_eq!(leaf.leaf_search(b"k4").unwrap(), Err(3));

    // Split
    let (left, right, mid_key) = leaf.leaf_split_into(PageId(2), PageId(3)).unwrap();
    assert_eq!(mid_key, b"k2");
    assert_eq!(left.item_count(), 1);
    assert_eq!(right.item_count(), 2);
    assert_eq!(left.leaf_get_key(0).unwrap(), b"k1");
    assert_eq!(right.leaf_get_key(0).unwrap(), b"k2");
    assert_eq!(right.leaf_get_key(1).unwrap(), b"k3");
    assert_eq!(left.aux_ptr(), PageId(3));
}

#[test]
fn test_slotted_internal_operations() {
    let mut node = Page::new_internal(PageId(10), PageId(100)); // first_child = 100
    node = node
        .internal_create_with_insert(PageId(10), b"m", PageId(200))
        .unwrap();
    node = node
        .internal_create_with_insert(PageId(10), b"b", PageId(150))
        .unwrap();
    node = node
        .internal_create_with_insert(PageId(10), b"t", PageId(250))
        .unwrap();

    assert_eq!(node.item_count(), 3);
    assert_eq!(node.internal_get_key(0).unwrap(), b"b");
    assert_eq!(node.internal_get_child(0).unwrap(), PageId(150));
    assert_eq!(node.internal_get_key(1).unwrap(), b"m");
    assert_eq!(node.internal_get_child(1).unwrap(), PageId(200));
    assert_eq!(node.internal_get_key(2).unwrap(), b"t");
    assert_eq!(node.internal_get_child(2).unwrap(), PageId(250));

    // Binary search routing
    assert_eq!(node.internal_search(b"a").unwrap(), PageId(100)); // < "b" -> first_child
    assert_eq!(node.internal_search(b"b").unwrap(), PageId(150));
    assert_eq!(node.internal_search(b"c").unwrap(), PageId(150));
    assert_eq!(node.internal_search(b"m").unwrap(), PageId(200));
    assert_eq!(node.internal_search(b"p").unwrap(), PageId(200));
    assert_eq!(node.internal_search(b"t").unwrap(), PageId(250));
    assert_eq!(node.internal_search(b"z").unwrap(), PageId(250));
}

#[test]
fn test_slotted_inplace_operations() {
    let mut leaf = Page::new_leaf(PageId(1), PageId::INVALID);
    assert!(leaf.leaf_insert_inplace(b"k3", b"val3").unwrap());
    assert!(leaf.leaf_insert_inplace(b"k1", b"val1").unwrap());
    assert!(leaf.leaf_insert_inplace(b"k2", b"val2").unwrap());

    assert_eq!(leaf.item_count(), 3);
    assert_eq!(leaf.leaf_get_key(0).unwrap(), b"k1");
    assert_eq!(leaf.leaf_get_val(0).unwrap(), b"val1");
    assert_eq!(leaf.leaf_get_key(1).unwrap(), b"k2");
    assert_eq!(leaf.leaf_get_val(1).unwrap(), b"val2");
    assert_eq!(leaf.leaf_get_key(2).unwrap(), b"k3");
    assert_eq!(leaf.leaf_get_val(2).unwrap(), b"val3");
    assert!(leaf.verify_checksum().is_ok());

    // In-place replacement of existing key
    assert!(leaf.leaf_insert_inplace(b"k2", b"valX").unwrap());
    assert_eq!(leaf.item_count(), 3);
    assert_eq!(leaf.leaf_get_val(1).unwrap(), b"valX");
    assert!(leaf.verify_checksum().is_ok());

    // Internal in-place insert and replace child
    let mut internal = Page::new_internal(PageId(10), PageId(100));
    assert!(internal.internal_insert_inplace(b"m", PageId(200)).unwrap());
    assert!(internal.internal_insert_inplace(b"b", PageId(150)).unwrap());
    assert!(internal.internal_insert_inplace(b"t", PageId(250)).unwrap());

    assert_eq!(internal.item_count(), 3);
    assert_eq!(internal.internal_get_key(0).unwrap(), b"b");
    assert_eq!(internal.internal_get_child(0).unwrap(), PageId(150));
    assert_eq!(internal.internal_get_key(1).unwrap(), b"m");
    assert_eq!(internal.internal_get_child(1).unwrap(), PageId(200));
    assert_eq!(internal.internal_get_key(2).unwrap(), b"t");
    assert_eq!(internal.internal_get_child(2).unwrap(), PageId(250));
    assert!(internal.verify_checksum().is_ok());

    // In-place replace child
    assert!(
        internal
            .internal_replace_child(PageId(200), PageId(201))
            .unwrap()
    );
    assert_eq!(internal.internal_get_child(1).unwrap(), PageId(201));
    assert!(internal.verify_checksum().is_ok());
}

#[test]
fn test_key_range_and_write_batch() {
    let range = KeyRange {
        start: Bound::Included(b"a".to_vec()),
        end: Bound::Excluded(b"z".to_vec()),
    };
    assert!(matches!(range.start, Bound::Included(ref v) if v == b"a"));
    assert!(matches!(range.end, Bound::Excluded(ref v) if v == b"z"));

    let mut batch = WriteBatch::new();
    assert!(batch.is_empty());
    assert_eq!(batch.len(), 0);

    batch.put(b"k1", b"v1");
    batch.put(b"k2", b"v2");
    batch.delete(b"k3");

    assert!(!batch.is_empty());
    assert_eq!(batch.len(), 3);

    batch.clear();
    assert!(batch.is_empty());
    assert_eq!(batch.len(), 0);
}

#[test]
fn test_page_free_and_corrupt_checksum() {
    let page = Page::new(PageId(7), PageType::Free);
    let header = page.read_header().unwrap();
    assert_eq!(header.page_id, PageId(7));
    assert_eq!(header.page_type, PageType::Free);
    assert!(page.verify_checksum().is_ok());

    let mut corrupt = page;
    corrupt.as_mut_slice()[10] ^= 0xAA;
    assert!(corrupt.verify_checksum().is_err());
}
