// Adapted from sandstorm-marketplace; see licenses/Marketplace-LICENSE.
use anyhow::{Result, ensure};
const ALPHABET: &[u8] = b"0123456789acdefghjkmnpqrstuvwxyz";
pub fn app_id(bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut buffer = 0u32;
    let mut bits = 0;
    for byte in bytes {
        buffer = (buffer << 8) | u32::from(*byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buffer >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    out
}
pub fn parse_app_id(text: &str) -> Result<[u8; 32]> {
    ensure!(text.len() == 52, "invalid app ID");
    let mut out = Vec::new();
    let mut buffer = 0u32;
    let mut bits = 0;
    for byte in text.bytes() {
        let value = ALPHABET
            .iter()
            .position(|&b| b == byte)
            .ok_or_else(|| anyhow::anyhow!("invalid app ID"))?;
        buffer = (buffer << 5) | value as u32;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    ensure!(buffer & ((1 << bits) - 1) == 0, "invalid app ID padding");
    out.try_into()
        .map_err(|_| anyhow::anyhow!("invalid app ID length"))
}
