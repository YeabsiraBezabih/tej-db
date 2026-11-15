use super::*;
use std::str::FromStr;

#[test]
fn test_decimal_arithmetic_and_rescaling() {
    let d1 = Decimal::from_str("123.45").unwrap();
    let d2 = Decimal::from_str("0.55").unwrap();

    let sum = d1.checked_add(&d2).unwrap();
    assert_eq!(sum.to_string(), "124.00");

    let diff = d1.checked_sub(&d2).unwrap();
    assert_eq!(diff.to_string(), "122.90");

    let prod = d1.checked_mul(&d2).unwrap();
    assert_eq!(prod.to_string(), "67.8975");

    let d_div1 = Decimal::from_str("10.0").unwrap();
    let d_div2 = Decimal::from_str("4.0").unwrap();
    let div = d_div1.checked_div(&d_div2, 2).unwrap();
    assert_eq!(div.to_string(), "2.50");

    // Scale equality: 1.5 == 1.50
    let a = Decimal::from_str("1.5").unwrap();
    let b = Decimal::from_str("1.50").unwrap();
    assert_eq!(a, b);
    assert_eq!(a.cmp(&b), std::cmp::Ordering::Equal);
}

#[test]
fn test_uuid_parsing_and_roundtrip() {
    let s = "550e8400-e29b-41d4-a716-446655440000";
    let u = Uuid::from_str(s).unwrap();
    assert_eq!(u.to_string(), s);

    let nil = Uuid::nil();
    assert!(nil < u);
}

#[test]
fn test_timestamp_parsing_and_display() {
    let s = "2026-11-14 19:40:00.123456";
    let ts = Timestamp::from_str(s).unwrap();
    assert_eq!(ts.to_string(), s);

    let ts2 = Timestamp::from_secs(1_700_000_000);
    assert!(ts2 < ts);
}

#[test]
fn test_interval_and_timestamp_addition() {
    let ts = Timestamp::from_str("2026-11-14 00:00:00").unwrap();
    let interval = Interval::from_str("1 day 2 hours").unwrap();
    let new_ts = interval.add_to_timestamp(ts);
    assert_eq!(new_ts.to_string(), "2026-11-15 02:00:00");
}

#[test]
fn test_jsonb_path_extraction() {
    let json_str = r#"{"user": {"name": "Alice", "age": 30}, "tags": ["admin", "staff"]}"#;
    let j = JsonB::from_str(json_str).unwrap();

    let name = j.extract_path("user.name").unwrap();
    assert_eq!(name, "Alice");

    let tag0 = j.extract_path("tags.0").unwrap();
    assert_eq!(tag0, "admin");
}
