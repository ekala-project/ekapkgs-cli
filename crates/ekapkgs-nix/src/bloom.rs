//! Simple bloom filter for chunk digest negotiation.
//!
//! Uses the "double hashing" trick: given a 32-byte blake3 digest, we split
//! bytes 0..8 and 8..16 into two u64 values (h1, h2) and derive k hash
//! functions as `h_i = (h1 + i * h2) % num_bits`.
//!
//! This avoids any external dependency while providing good distribution
//! properties (the input is already a cryptographic hash).

/// A bloom filter backed by a bit vector.
pub struct BloomFilter {
    bits: Vec<u8>,
    num_bits: u64,
    num_hashes: u32,
}

/// Optimal number of hash functions for a given number of items and bits.
fn optimal_k(num_bits: u64, num_items: u64) -> u32 {
    if num_items == 0 {
        return 1;
    }
    let k = ((num_bits as f64 / num_items as f64) * core::f64::consts::LN_2).round() as u32;
    k.clamp(1, 30)
}

impl BloomFilter {
    /// Create a bloom filter sized for `num_items` with a target false-positive
    /// rate of approximately 1%.
    ///
    /// The filter size is `ceil(-num_items * ln(fpr) / ln(2)^2)` bits.
    pub fn new(num_items: usize) -> Self {
        // Target 1% FPR: bits = -n * ln(0.01) / (ln(2))^2 ≈ n * 9.585
        let raw_bits = ((num_items as f64 * 9.585).ceil() as u64).max(64);
        let num_bytes = raw_bits.div_ceil(8) as usize;
        // Use the rounded-up byte count so num_bits matches from_bytes().
        let num_bits = num_bytes as u64 * 8;
        let num_hashes = optimal_k(num_bits, num_items as u64);
        Self {
            bits: vec![0u8; num_bytes],
            num_bits,
            num_hashes,
        }
    }

    /// Reconstruct a bloom filter from its serialized form.
    pub fn from_bytes(data: &[u8], num_hashes: u32) -> Self {
        let num_bits = data.len() as u64 * 8;
        Self {
            bits: data.to_vec(),
            num_bits,
            num_hashes: num_hashes.max(1),
        }
    }

    /// Insert a 32-byte digest into the filter.
    pub fn insert(&mut self, digest: &[u8; 32]) {
        let (h1, h2) = double_hash(digest);
        for i in 0..self.num_hashes {
            let bit = h1.wrapping_add((i as u64).wrapping_mul(h2)) % self.num_bits;
            self.bits[bit as usize / 8] |= 1 << (bit % 8);
        }
    }

    /// Test whether a digest is probably in the set.
    ///
    /// Returns `true` if the digest may have been inserted (possibly a false
    /// positive), `false` if it was definitely not inserted.
    pub fn maybe_contains(&self, digest: &[u8; 32]) -> bool {
        let (h1, h2) = double_hash(digest);
        for i in 0..self.num_hashes {
            let bit = h1.wrapping_add((i as u64).wrapping_mul(h2)) % self.num_bits;
            if self.bits[bit as usize / 8] & (1 << (bit % 8)) == 0 {
                return false;
            }
        }
        true
    }

    /// Return the raw bit vector.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bits
    }

    /// Number of hash functions used.
    pub fn num_hashes(&self) -> u32 {
        self.num_hashes
    }
}

/// Extract two independent u64 hash values from a 32-byte blake3 digest.
fn double_hash(digest: &[u8; 32]) -> (u64, u64) {
    let h1 = u64::from_le_bytes(digest[0..8].try_into().unwrap());
    let h2 = u64::from_le_bytes(digest[8..16].try_into().unwrap());
    // Ensure h2 is odd so the generated sequence covers all positions.
    (h1, h2 | 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_query() {
        let mut bf = BloomFilter::new(100);
        let digest = [42u8; 32];
        assert!(!bf.maybe_contains(&digest));
        bf.insert(&digest);
        assert!(bf.maybe_contains(&digest));
    }

    #[test]
    fn no_false_negatives() {
        let mut bf = BloomFilter::new(1000);
        let digests: Vec<[u8; 32]> = (0..1000u32)
            .map(|i| {
                let mut d = [0u8; 32];
                d[..4].copy_from_slice(&i.to_le_bytes());
                d
            })
            .collect();
        for d in &digests {
            bf.insert(d);
        }
        for d in &digests {
            assert!(bf.maybe_contains(d), "false negative detected");
        }
    }

    #[test]
    fn roundtrip_serialization() {
        let mut bf = BloomFilter::new(100);
        let digest = [7u8; 32];
        bf.insert(&digest);
        let k = bf.num_hashes();
        let bytes = bf.into_bytes();

        let bf2 = BloomFilter::from_bytes(&bytes, k);
        assert!(bf2.maybe_contains(&digest));
    }

    #[test]
    fn false_positive_rate_reasonable() {
        // Insert 10K items, test 10K non-inserted items, FPR should be < 5%.
        let mut bf = BloomFilter::new(10_000);
        for i in 0..10_000u32 {
            let mut d = [0u8; 32];
            d[..4].copy_from_slice(&i.to_le_bytes());
            bf.insert(&d);
        }
        let mut false_positives = 0u32;
        for i in 10_000..20_000u32 {
            let mut d = [0u8; 32];
            d[..4].copy_from_slice(&i.to_le_bytes());
            if bf.maybe_contains(&d) {
                false_positives += 1;
            }
        }
        let fpr = false_positives as f64 / 10_000.0;
        assert!(fpr < 0.05, "FPR too high: {fpr:.3}");
    }
}
