//! A standard bit-vector Bloom filter using double hashing (Kirsch-Mitzenmacher):
//! the k-th probe index is `h1 + k*h2 mod m`, derived from two independent
//! 64-bit hashes of the key. Sized from the expected element count and a
//! target false-positive rate using the standard formulas:
//!
//! m = ceil(-n * ln(p) / (ln 2)^2)
//! k = round((m / n) * ln 2)

#[derive(Debug, Clone)]
pub struct BloomFilter {
    bits: Vec<u64>,
    num_bits: usize,
    num_hashes: u32,
}

impl BloomFilter {
    pub fn with_rate(expected_items: usize, false_positive_rate: f64) -> Self {
        let n = expected_items.max(1) as f64;
        let p = false_positive_rate.clamp(1e-6, 0.5);
        let m = (-n * p.ln() / (std::f64::consts::LN_2.powi(2))).ceil() as usize;
        let m = m.max(64);
        let k = ((m as f64 / n) * std::f64::consts::LN_2).round().max(1.0) as u32;
        let num_words = (m + 63) / 64;
        Self {
            bits: vec![0u64; num_words],
            num_bits: num_words * 64,
            num_hashes: k.min(32),
        }
    }

    fn hashes(key: &[u8]) -> (u64, u64) {
        // Two independent-enough hashes derived from FNV-1a with different
        // seeds/offsets, avoiding an external hashing dependency.
        let h1 = fnv1a(key, 0xcbf29ce484222325);
        let h2 = fnv1a(key, 0x84222325cbf29ce4);
        (h1, h2 | 1) // ensure h2 is odd so it's coprime with power-of-two-ish m
    }

    pub fn insert(&mut self, key: &[u8]) {
        let (h1, h2) = Self::hashes(key);
        for i in 0..self.num_hashes as u64 {
            let idx = (h1.wrapping_add(i.wrapping_mul(h2)) as usize) % self.num_bits;
            self.bits[idx / 64] |= 1 << (idx % 64);
        }
    }

    pub fn contains(&self, key: &[u8]) -> bool {
        let (h1, h2) = Self::hashes(key);
        for i in 0..self.num_hashes as u64 {
            let idx = (h1.wrapping_add(i.wrapping_mul(h2)) as usize) % self.num_bits;
            if self.bits[idx / 64] & (1 << (idx % 64)) == 0 {
                return false;
            }
        }
        true
    }

    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(4 + 4 + self.bits.len() * 8);
        out.extend_from_slice(&(self.num_bits as u32).to_le_bytes());
        out.extend_from_slice(&self.num_hashes.to_le_bytes());
        for word in &self.bits {
            out.extend_from_slice(&word.to_le_bytes());
        }
        out
    }

    pub fn deserialize(data: &[u8]) -> Option<Self> {
        if data.len() < 8 {
            return None;
        }
        let num_bits = u32::from_le_bytes(data[0..4].try_into().ok()?) as usize;
        let num_hashes = u32::from_le_bytes(data[4..8].try_into().ok()?);
        let word_bytes = &data[8..];
        if word_bytes.len() % 8 != 0 {
            return None;
        }
        let bits: Vec<u64> = word_bytes
            .chunks_exact(8)
            .map(|c| u64::from_le_bytes(c.try_into().unwrap()))
            .collect();
        Some(Self { bits, num_bits, num_hashes })
    }
}

fn fnv1a(data: &[u8], seed: u64) -> u64 {
    const PRIME: u64 = 0x100000001b3;
    let mut hash = seed;
    for &b in data {
        hash ^= b as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_false_negatives() {
        let mut bf = BloomFilter::with_rate(1000, 0.01);
        let keys: Vec<Vec<u8>> = (0..1000).map(|i| format!("key-{i}").into_bytes()).collect();
        for k in &keys {
            bf.insert(k);
        }
        for k in &keys {
            assert!(bf.contains(k), "false negative for {:?}", String::from_utf8_lossy(k));
        }
    }

    #[test]
    fn false_positive_rate_is_roughly_bounded() {
        let mut bf = BloomFilter::with_rate(1000, 0.01);
        for i in 0..1000 {
            bf.insert(format!("present-{i}").as_bytes());
        }
        let mut false_positives = 0;
        let trials = 5000;
        for i in 0..trials {
            if bf.contains(format!("absent-{i}").as_bytes()) {
                false_positives += 1;
            }
        }
        let rate = false_positives as f64 / trials as f64;
        // Generous bound: sized for 1%, allow up to 5% in practice for a
        // small, deterministic hash-based filter.
        assert!(rate < 0.05, "false positive rate too high: {rate}");
    }

    #[test]
    fn serialize_roundtrip_preserves_membership() {
        let mut bf = BloomFilter::with_rate(100, 0.01);
        bf.insert(b"hello");
        bf.insert(b"world");
        let bytes = bf.serialize();
        let bf2 = BloomFilter::deserialize(&bytes).unwrap();
        assert!(bf2.contains(b"hello"));
        assert!(bf2.contains(b"world"));
    }
}
