//! The one bincode call site for both sides of the plugin boundary.
//!
//! Mirrors the artifact codec in `lumen_ir::artifact` exactly: plain
//! `bincode::serialize` to encode, `DefaultOptions` with fixint encoding and
//! a size limit to decode. `DefaultOptions` alone defaults to *varint*, which
//! would mis-decode fixint bytes; the limit keeps a corrupt length prefix
//! from triggering an enormous pre-allocation.

use bincode::Options;
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Upper bound on any single payload crossing the boundary. Matches the
/// artifact body cap in `lumen_ir::artifact`.
pub const MAX_PAYLOAD: u64 = 512 * 1024 * 1024;

/// Encode a payload for the boundary.
pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, String> {
    bincode::serialize(value).map_err(|e| e.to_string())
}

/// Decode a payload from the boundary.
pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, String> {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_PAYLOAD)
        .deserialize(bytes)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let v: Vec<String> = vec!["a".into(), "bc".into()];
        let bytes = encode(&v).unwrap();
        let back: Vec<String> = decode(&bytes).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn truncated_is_an_error_not_a_panic() {
        let bytes = encode(&vec![1u64, 2, 3]).unwrap();
        let err = decode::<Vec<u64>>(&bytes[..bytes.len() - 3]).unwrap_err();
        assert!(!err.is_empty());
    }

    #[test]
    fn oversized_length_prefix_is_rejected() {
        // A u64 length prefix claiming more elements than the cap allows.
        let bytes = (u64::MAX).to_le_bytes().to_vec();
        assert!(decode::<Vec<u8>>(&bytes).is_err());
    }
}
