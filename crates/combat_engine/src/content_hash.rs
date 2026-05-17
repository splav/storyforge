//! BLAKE3 content fingerprint for trace `init` lines (Phase 5 D3).
//!
//! Stable across crate versions, cross-platform deterministic.
//! Consumed by the bridge writer at combat start and by `TomlContentView`
//! (5c) for replay-time mismatch detection.

/// Compute a BLAKE3 hash over canonical-sorted-by-filename concatenation
/// of `(filename, contents)` pairs. Returns the 32-byte digest.
pub fn hash_content(files: &[(&str, &str)]) -> [u8; 32] {
    let mut sorted: Vec<&(&str, &str)> = files.iter().collect();
    sorted.sort_by_key(|(name, _)| *name);
    let mut hasher = blake3::Hasher::new();
    for (name, contents) in sorted {
        hasher.update(name.as_bytes());
        hasher.update(b"\0");
        hasher.update(contents.as_bytes());
        hasher.update(b"\n");
    }
    *hasher.finalize().as_bytes()
}

/// Format a 32-byte BLAKE3 digest as `blake3:<hex>` (for JSON trace).
pub fn format_hex(digest: &[u8; 32]) -> String {
    let mut s = String::with_capacity(7 + 64);
    s.push_str("blake3:");
    for b in digest {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_is_deterministic() {
        let h1 = hash_content(&[]);
        let h2 = hash_content(&[]);
        assert_eq!(h1, h2);
        // Known: BLAKE3 of empty input.
        assert_eq!(format_hex(&h1).len(), 7 + 64);
        assert!(format_hex(&h1).starts_with("blake3:"));
    }

    #[test]
    fn single_file_is_deterministic() {
        let h1 = hash_content(&[("a.toml", "content = true")]);
        let h2 = hash_content(&[("a.toml", "content = true")]);
        assert_eq!(h1, h2);
        // Different content → different hash.
        let h3 = hash_content(&[("a.toml", "content = false")]);
        assert_ne!(h1, h3);
    }

    #[test]
    fn multi_file_order_independent() {
        let a = ("a.toml", "foo");
        let b = ("b.toml", "bar");
        let c = ("c.toml", "baz");

        // All orderings must produce the same hash.
        let h_abc = hash_content(&[a, b, c]);
        let h_bca = hash_content(&[b, c, a]);
        let h_cab = hash_content(&[c, a, b]);
        let h_cba = hash_content(&[c, b, a]);
        assert_eq!(h_abc, h_bca);
        assert_eq!(h_abc, h_cab);
        assert_eq!(h_abc, h_cba);
    }

    #[test]
    fn multi_file_differs_from_single() {
        let h_single = hash_content(&[("a.toml", "foo")]);
        let h_multi = hash_content(&[("a.toml", "foo"), ("b.toml", "bar")]);
        assert_ne!(h_single, h_multi);
    }

    #[test]
    fn known_vector_smoke() {
        // Smoke test: just ensure format_hex output is 71 chars and valid hex prefix.
        let digest = hash_content(&[("x.toml", "hello world")]);
        let hex = format_hex(&digest);
        assert_eq!(hex.len(), 71); // "blake3:" (7) + 64 hex chars
        assert!(hex.starts_with("blake3:"));
        // All chars after prefix are valid lowercase hex.
        assert!(hex[7..].chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn filename_differs_same_content_gives_different_hash() {
        let h1 = hash_content(&[("a.toml", "same")]);
        let h2 = hash_content(&[("b.toml", "same")]);
        assert_ne!(h1, h2);
    }
}
