//! A tiny deterministic PRNG (SplitMix64) for the synthetic generator.
//!
//! Determinism is a hard requirement: two `generate(&cfg)` runs with the same
//! seed must produce byte-identical output (verified in `tests/valid.rs`). We
//! therefore avoid `rand`/entropy entirely and seed from `SynthConfig::seed`.

/// SplitMix64 — a fast, seedable, entropy-free PRNG.
#[derive(Debug, Clone)]
pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// Next pseudo-random `u64`.
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}
