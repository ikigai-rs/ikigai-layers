//! Entry ids: sortable by time, unique within a layer by construction.

/// Crockford's base32 alphabet, lowercase (the ledger's spelling of its ids).
const CROCKFORD: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";

/// The id of the entry stamped with `seq` at `millis`: 10 characters of Crockford base32
/// over the low 48 bits of the millisecond clock, then 13 over the feed sequence number.
///
/// The ULID's ordering (ids sort by time, then by sequence) without its randomness, and
/// that is on purpose: a layer's sequence is gapless and assigned under the layer's write
/// lock, so the second half alone makes every id in a layer distinct, under any clock,
/// a fixed test clock included. The sequence is also recoverable from the id
/// ([`seq_of`]), which is how a host that crashed between writing an entry and logging
/// its feed event repairs the feed on the next open.
///
/// ```
/// use ikigai_layers::id::{mint, seq_of};
///
/// let id = mint(1_791_000_000_123, 42);
/// assert_eq!(id.len(), 23);
/// assert_eq!(seq_of(&id), Some(42));
/// assert!(mint(1_791_000_000_123, 41) < id && id < mint(1_791_000_000_124, 0));
/// ```
pub fn mint(millis: u64, seq: u64) -> String {
    let mut id = String::with_capacity(23);
    for shift in (0..10).rev() {
        id.push(CROCKFORD[((millis >> (shift * 5)) & 31) as usize] as char);
    }
    // 13 × 5 = 65 bits, the top character carrying the top four: every u64 has its own
    // spelling.
    for shift in (0..13).rev() {
        id.push(CROCKFORD[((seq >> (shift * 5)) & 31) as usize] as char);
    }
    id
}

/// The feed sequence number an id was minted with, or `None` for a string that is not an
/// id this module mints.
pub fn seq_of(id: &str) -> Option<u64> {
    if id.len() != 23 || !id.bytes().all(|b| CROCKFORD.contains(&b)) {
        return None;
    }
    let mut seq: u64 = 0;
    for (i, b) in id.bytes().skip(10).enumerate() {
        let digit = CROCKFORD.iter().position(|c| *c == b)? as u64;
        // The top character carries the top four bits; anything larger is no u64.
        if i == 0 && digit > 15 {
            return None;
        }
        seq = (seq << 5) | digit;
    }
    Some(seq)
}

/// Whether `id` has the shape [`mint`] produces.
pub fn is_id(id: &str) -> bool {
    seq_of(id).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extremes_round_trip() {
        for seq in [0, 1, u64::MAX, u64::MAX / 3] {
            assert_eq!(seq_of(&mint(0, seq)), Some(seq));
        }
        assert_eq!(seq_of("0000000000g000000000000"), None);
        assert_eq!(seq_of("0000000000f000000000000"), Some(15 << 60));
        assert_eq!(seq_of("short"), None);
        assert_eq!(seq_of("0000000000000000000000U"), None);
    }
}
