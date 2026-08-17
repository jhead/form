//! Hex helpers, mirroring the `fromHex`/`toHex` used by upstream's tests.

pub fn from_hex(hex: &str) -> Vec<u8> {
    assert!(hex.len() % 2 == 0, "Hex fixture must contain whole bytes");
    (0..hex.len() / 2)
        .map(|index| u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).expect("hex byte"))
        .collect()
}

pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
