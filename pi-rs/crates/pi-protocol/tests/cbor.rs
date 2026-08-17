//! Port of `.upstream/packages/protocol/test/cbor/cbor.test.ts`.
//!
//! The `KNOWN_VECTORS` table is upstream's, hex strings included; the
//! `UPSTREAM_*` fixtures further down were produced by running upstream's own
//! `encodeCbor` (see the report accompanying this port) and pin the JavaScript
//! quirks that a general-purpose CBOR crate would not reproduce.

mod common;

use common::{from_hex, to_hex};
use pi_protocol::{
    decode_cbor, encode_cbor, CborError, CborMap, CborOptions, CborValue,
    DEFAULT_MAX_CBOR_BYTE_LENGTH, DEFAULT_MAX_CBOR_CONTAINER_LENGTH, DEFAULT_MAX_CBOR_DEPTH,
    MAX_SAFE_INTEGER, MIN_SAFE_INTEGER,
};

fn encode(value: &CborValue) -> Vec<u8> {
    encode_cbor(value, CborOptions::default()).expect("encodes")
}

fn decode(bytes: &[u8]) -> CborValue {
    decode_cbor(bytes, CborOptions::default()).expect("decodes")
}

fn map(entries: impl IntoIterator<Item = (&'static str, CborValue)>) -> CborValue {
    CborValue::Map(
        entries
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect::<CborMap>(),
    )
}

fn text(value: &str) -> CborValue {
    CborValue::Text(value.to_owned())
}

/// Upstream's `knownVectors`, verbatim.
fn known_vectors() -> Vec<(CborValue, &'static str)> {
    vec![
        (CborValue::Null, "f6"),
        (CborValue::Bool(false), "f4"),
        (CborValue::Bool(true), "f5"),
        (CborValue::Integer(0), "00"),
        (CborValue::Integer(1), "01"),
        (CborValue::Integer(10), "0a"),
        (CborValue::Integer(23), "17"),
        (CborValue::Integer(24), "1818"),
        (CborValue::Integer(25), "1819"),
        (CborValue::Integer(100), "1864"),
        (CborValue::Integer(1000), "1903e8"),
        (CborValue::Integer(1_000_000), "1a000f4240"),
        (CborValue::Integer(1_000_000_000_000), "1b000000e8d4a51000"),
        (CborValue::Integer(MAX_SAFE_INTEGER), "1b001fffffffffffff"),
        (CborValue::Integer(-1), "20"),
        (CborValue::Integer(-10), "29"),
        (CborValue::Integer(-24), "37"),
        (CborValue::Integer(-25), "3818"),
        (CborValue::Integer(-100), "3863"),
        (CborValue::Integer(-1000), "3903e7"),
        (CborValue::Integer(-1_000_000), "3a000f423f"),
        (CborValue::Integer(MIN_SAFE_INTEGER), "3b001ffffffffffffe"),
        (CborValue::Float(1.1), "fb3ff199999999999a"),
        (CborValue::Float(-0.0), "fb8000000000000000"),
        (CborValue::Bytes(vec![1, 2, 3, 4]), "4401020304"),
        (text(""), "60"),
        (text("IETF"), "6449455446"),
        (text("ü"), "62c3bc"),
        (text("水"), "63e6b0b4"),
        (text("𐅑"), "64f0908591"),
        (CborValue::Array(vec![]), "80"),
        (
            CborValue::Array(vec![
                CborValue::Integer(1),
                CborValue::Integer(2),
                CborValue::Integer(3),
            ]),
            "83010203",
        ),
        (
            CborValue::Array(vec![
                CborValue::Integer(1),
                CborValue::Array(vec![CborValue::Integer(2), CborValue::Integer(3)]),
                CborValue::Array(vec![CborValue::Integer(4), CborValue::Integer(5)]),
            ]),
            "8301820203820405",
        ),
        (
            map([
                ("a", CborValue::Integer(1)),
                (
                    "b",
                    CborValue::Array(vec![CborValue::Integer(2), CborValue::Integer(3)]),
                ),
            ]),
            "a26161016162820203",
        ),
    ]
}

#[test]
fn encodes_and_decodes_rfc_8949_vectors() {
    for (value, wire) in known_vectors() {
        assert_eq!(to_hex(&encode(&value)), wire, "encoding {value:?}");
        let decoded = decode(&from_hex(wire));
        match (&value, &decoded) {
            // `-0` survives the round trip as a float, distinguishable from `0`
            // exactly as `Object.is` distinguishes them upstream.
            (CborValue::Float(expected), CborValue::Float(actual)) if *expected == 0.0 => {
                assert!(actual.is_sign_negative(), "-0 lost its sign");
            }
            _ => assert_eq!(decoded, value, "decoding {wire}"),
        }
    }
}

#[test]
fn preserves_a_leading_unicode_bom() {
    assert_eq!(decode(&from_hex("63efbbbf")), text("\u{feff}"));
}

#[test]
fn treats_prototype_pollution_keys_as_data() {
    // Upstream defends against `__proto__` being interpreted as a prototype;
    // in Rust it is just a map key, but the round trip is worth pinning.
    let value = map([("__proto__", text("safe"))]);
    assert_eq!(decode(&encode(&value)), value);
}

#[test]
fn rejects_unsupported_encoder_values() {
    // Upstream's list also covers `undefined`, array holes, bigint, symbol,
    // function, Date and Map. None of those is representable as a `CborValue`,
    // so the type system rejects them ahead of the encoder.
    for value in [
        CborValue::Float(f64::NAN),
        CborValue::Float(f64::INFINITY),
        CborValue::Float(f64::NEG_INFINITY),
    ] {
        assert_eq!(
            encode_cbor(&value, CborOptions::default()),
            Err(CborError::NonFiniteNumber),
            "expected {value:?} to be rejected",
        );
    }

    for value in [
        CborValue::Float(MAX_SAFE_INTEGER as f64 + 1.0),
        CborValue::Float(MIN_SAFE_INTEGER as f64 - 1.0),
        CborValue::Integer(MAX_SAFE_INTEGER + 1),
        CborValue::Integer(MIN_SAFE_INTEGER - 1),
    ] {
        assert_eq!(
            encode_cbor(&value, CborOptions::default()),
            Err(CborError::UnsafeInteger),
            "expected {value:?} to be rejected",
        );
    }
}

#[test]
fn rejects_excessive_encoder_depth() {
    let mut too_deep = CborValue::Null;
    for _ in 0..=DEFAULT_MAX_CBOR_DEPTH {
        too_deep = CborValue::Array(vec![too_deep]);
    }
    assert_eq!(
        encode_cbor(&too_deep, CborOptions::default()),
        Err(CborError::DepthLimit {
            limit: DEFAULT_MAX_CBOR_DEPTH
        }),
    );
}

#[test]
fn rejects_invalid_decoder_input() {
    // Upstream's table, unchanged.
    let cases = [
        ("empty input", ""),
        ("truncated integer", "18"),
        ("reserved additional information", "1c"),
        ("indefinite byte string", "5f"),
        ("indefinite text string", "7f"),
        ("indefinite array", "9f"),
        ("indefinite map", "bf"),
        ("tag", "c000"),
        ("undefined", "f7"),
        ("unsupported simple value", "e0"),
        ("break outside an indefinite item", "ff"),
        ("float16", "f93c00"),
        ("float32", "fa3f800000"),
        ("positive infinity", "fb7ff0000000000000"),
        ("NaN", "fb7ff8000000000000"),
        ("truncated float64", "fb3ff00000"),
        ("truncated byte string", "44010203"),
        ("truncated text string", "636162"),
        ("truncated array", "8201"),
        ("truncated map", "a16161"),
        ("trailing data", "0000"),
        ("non-string map key", "a10102"),
        ("duplicate map key", "a2616101616102"),
        ("invalid UTF-8 byte", "61ff"),
        ("overlong UTF-8", "62c080"),
        ("UTF-8 surrogate", "63eda080"),
        ("unsafe positive integer", "1b0020000000000000"),
        ("unsafe negative integer", "3b001fffffffffffff"),
        ("unsafe integer encoded as float64", "fb4340000000000000"),
    ];
    for (label, wire) in cases {
        assert!(
            decode_cbor(&from_hex(wire), CborOptions::default()).is_err(),
            "expected {label} ({wire}) to be rejected",
        );
    }
}

#[test]
fn enforces_depth_and_declared_lengths_before_traversing() {
    let mut too_deep = vec![0x81u8; DEFAULT_MAX_CBOR_DEPTH as usize + 1];
    too_deep.push(0xf6);
    assert_eq!(
        decode_cbor(&too_deep, CborOptions::default()),
        Err(CborError::DepthLimit {
            limit: DEFAULT_MAX_CBOR_DEPTH
        }),
    );

    let oversized_bytes = format!("5a{:08x}", DEFAULT_MAX_CBOR_BYTE_LENGTH + 1);
    let oversized_text = format!("7a{:08x}", DEFAULT_MAX_CBOR_BYTE_LENGTH + 1);
    let oversized_array = format!("9a{:08x}", DEFAULT_MAX_CBOR_CONTAINER_LENGTH + 1);
    let oversized_map = format!("ba{:08x}", DEFAULT_MAX_CBOR_CONTAINER_LENGTH + 1);
    for wire in [
        oversized_bytes,
        oversized_text,
        oversized_array,
        oversized_map,
    ] {
        // Only the four-byte header is present, so anything but a length error
        // would mean the decoder tried to read the declared payload.
        assert!(
            matches!(
                decode_cbor(&from_hex(&wire), CborOptions::default()),
                Err(CborError::LengthLimit { .. })
            ),
            "expected {wire} to be rejected by its declared length",
        );
    }
}

#[test]
fn supports_stricter_caller_provided_limits() {
    assert!(decode_cbor(
        &from_hex("83010203"),
        CborOptions::default().with_max_container_length(2)
    )
    .is_err());
    assert!(decode_cbor(
        &from_hex("626162"),
        CborOptions::default().with_max_byte_length(2)
    )
    .is_err());
    assert!(encode_cbor(
        &CborValue::Array(vec![
            CborValue::Integer(1),
            CborValue::Integer(2),
            CborValue::Integer(3)
        ]),
        CborOptions::default().with_max_container_length(2)
    )
    .is_err());
    assert!(encode_cbor(&text("ab"), CborOptions::default().with_max_byte_length(2)).is_err());
}

#[test]
fn rejects_a_depth_limit_above_the_configured_maximum() {
    assert!(matches!(
        encode_cbor(&CborValue::Null, CborOptions::default().with_max_depth(513)),
        Err(CborError::InvalidLimit { .. })
    ));
}

// ---------------------------------------------------------------------------
// byte fixtures generated by upstream's own encoder
// ---------------------------------------------------------------------------

/// `{ z: 1, "2": 2, a: 3, "10": 4, "0": 5, "01": 6, "-1": 7 }`.
///
/// The single most important fixture in this file. It proves the port
/// reproduces JavaScript's `Object.keys` ordering: canonical array-index keys
/// (`0`, `2`, `10`) hoisted to the front in *numeric* order, then the remaining
/// keys — including the non-canonical `"01"` and `"-1"` — in insertion order.
/// RFC 8949 canonical CBOR would have sorted all seven bytewise instead.
const UPSTREAM_NUMERIC_KEY_ORDERING: &str = "a761300561320262313004617a0161610362303106622d3107";

/// `{ a: 1.0, b: 2.5, c: -0, d: 3.0 }`.
///
/// Integral floats collapse to CBOR integers, `2.5` stays float64, and `-0`
/// stays float64 rather than becoming integer `0`.
const UPSTREAM_INTEGRAL_FLOAT: &str = "a46161016162fb40040000000000006163fb8000000000000000616403";

const UPSTREAM_NESTED: &str = "a1656f75746572a165696e6e657284016374776ff5f6";
const UPSTREAM_DEEP_STRING: &str = "a164746578747268c3a96c6c6f2077c3b6726c6420f0908591";
const UPSTREAM_BIG_INTS: &str =
    "a3636d61781b001fffffffffffff636d696e3b001ffffffffffffe636d69641b00038d7ea4c68000";
const UPSTREAM_EMPTY_CONTAINERS: &str = "a36161806162a0616360";
const UPSTREAM_BYTES: &str = "440001feff";

#[test]
fn matches_upstream_bytes_for_javascript_property_order() {
    let value = map([
        ("z", CborValue::Integer(1)),
        ("2", CborValue::Integer(2)),
        ("a", CborValue::Integer(3)),
        ("10", CborValue::Integer(4)),
        ("0", CborValue::Integer(5)),
        ("01", CborValue::Integer(6)),
        ("-1", CborValue::Integer(7)),
    ]);
    assert_eq!(to_hex(&encode(&value)), UPSTREAM_NUMERIC_KEY_ORDERING);
}

#[test]
fn matches_upstream_bytes_for_integral_floats() {
    let value = map([
        ("a", CborValue::Float(1.0)),
        ("b", CborValue::Float(2.5)),
        ("c", CborValue::Float(-0.0)),
        ("d", CborValue::Float(3.0)),
    ]);
    assert_eq!(to_hex(&encode(&value)), UPSTREAM_INTEGRAL_FLOAT);
}

#[test]
fn matches_upstream_bytes_for_assorted_values() {
    let nested = map([(
        "outer",
        map([(
            "inner",
            CborValue::Array(vec![
                CborValue::Integer(1),
                text("two"),
                CborValue::Bool(true),
                CborValue::Null,
            ]),
        )]),
    )]);
    assert_eq!(to_hex(&encode(&nested)), UPSTREAM_NESTED);

    let deep_string = map([("text", text("héllo wörld 𐅑"))]);
    assert_eq!(to_hex(&encode(&deep_string)), UPSTREAM_DEEP_STRING);

    let big_ints = map([
        ("max", CborValue::Integer(MAX_SAFE_INTEGER)),
        ("min", CborValue::Integer(MIN_SAFE_INTEGER)),
        ("mid", CborValue::Integer(1_000_000_000_000_000)),
    ]);
    assert_eq!(to_hex(&encode(&big_ints)), UPSTREAM_BIG_INTS);

    let empty_containers = map([
        ("a", CborValue::Array(vec![])),
        ("b", CborValue::map()),
        ("c", text("")),
    ]);
    assert_eq!(
        to_hex(&encode(&empty_containers)),
        UPSTREAM_EMPTY_CONTAINERS
    );

    assert_eq!(
        to_hex(&encode(&CborValue::Bytes(vec![0, 1, 254, 255]))),
        UPSTREAM_BYTES
    );
}

#[test]
fn upstream_byte_fixtures_round_trip() {
    for wire in [
        UPSTREAM_NUMERIC_KEY_ORDERING,
        UPSTREAM_INTEGRAL_FLOAT,
        UPSTREAM_NESTED,
        UPSTREAM_DEEP_STRING,
        UPSTREAM_BIG_INTS,
        UPSTREAM_EMPTY_CONTAINERS,
        UPSTREAM_BYTES,
    ] {
        let bytes = from_hex(wire);
        let decoded = decode(&bytes);
        assert_eq!(to_hex(&encode(&decoded)), wire, "re-encoding {wire}");
    }
}

#[test]
fn json_conversion_rejects_byte_strings() {
    assert!(CborValue::Bytes(vec![1]).to_json().is_none());
    assert!(map([("nested", CborValue::Bytes(vec![1]))])
        .to_json()
        .is_none());
    assert!(map([("fine", CborValue::Integer(1))]).to_json().is_some());
}

#[test]
fn json_conversion_narrows_integral_floats() {
    // A peer that wrote an integer as float64 still lands in an integer-typed
    // schema field, because JavaScript would not have told the two apart.
    let value = decode(&from_hex("fb4000000000000000"));
    assert_eq!(value, CborValue::Float(2.0));
    assert_eq!(value.to_json().unwrap(), serde_json::json!(2));
}
