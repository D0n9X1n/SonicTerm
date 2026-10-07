//! SHA-256 for the harness, by hand so it needs no crate: an incremental [`Sha256`] that hashes a stream as
//! it is written, and [`sha256_hex`], its one-shot wrapper. Ungated, so every platform runs its tests.
// The delivery replay (Windows) uses the wrapper and the guard-correlation sidecar (macOS, Windows) the stream;
// a Linux build compiles neither caller.
#![cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]

/// FIPS 180-4 SHA-256 round constants.
const ROUND_CONSTANTS: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// The FIPS 180-4 initial hash value.
const INITIAL_STATE: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

/// An incremental SHA-256: [`Sha256::update`] any number of times, then [`Sha256::finish`]. It holds one
/// partial 64-byte block, so hashing a stream never buffers more than that.
#[derive(Clone, Debug)]
pub(crate) struct Sha256 {
    state: [u32; 8],
    /// The bytes of the block being filled; only `pending_len` of them are meaningful.
    pending: [u8; 64],
    pending_len: usize,
    /// Every byte hashed so far, for the length the padding appends.
    total_len: u64,
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256 {
    /// A hasher that has seen no bytes.
    pub(crate) fn new() -> Self {
        Self { state: INITIAL_STATE, pending: [0; 64], pending_len: 0, total_len: 0 }
    }

    /// Hash `bytes` after everything hashed so far.
    pub(crate) fn update(&mut self, mut bytes: &[u8]) {
        self.total_len = self.total_len.wrapping_add(bytes.len() as u64);
        if self.pending_len > 0 {
            // When: pending_len is nonzero, a partial block is completed from `bytes` first.
            let take = (64 - self.pending_len).min(bytes.len());
            self.pending[self.pending_len..self.pending_len + take].copy_from_slice(&bytes[..take]);
            self.pending_len += take;
            bytes = &bytes[take..];
            if self.pending_len < 64 {
                // When: pending_len is still below a block, `bytes` is used up and nothing compresses.
                return;
            }
            let block = self.pending;
            compress(&mut self.state, &block);
            self.pending_len = 0;
        }
        let (blocks, remainder) = bytes.as_chunks::<64>();
        for block in blocks {
            compress(&mut self.state, block);
        }
        self.pending[..remainder.len()].copy_from_slice(remainder);
        self.pending_len = remainder.len();
    }

    /// The digest: one 0x80 byte, zeros to 56 bytes past a block boundary, then the bit length.
    pub(crate) fn finish(mut self) -> [u8; 32] {
        let bit_len = self.total_len.wrapping_mul(8);
        let mut tail = [0_u8; 128];
        tail[..self.pending_len].copy_from_slice(&self.pending[..self.pending_len]);
        tail[self.pending_len] = 0x80;
        let tail_len = if self.pending_len < 56 { 64 } else { 128 };
        tail[tail_len - 8..tail_len].copy_from_slice(&bit_len.to_be_bytes());
        for block in tail[..tail_len].as_chunks::<64>().0 {
            compress(&mut self.state, block);
        }
        let mut digest = [0_u8; 32];
        for (out, word) in digest.as_chunks_mut::<4>().0.iter_mut().zip(self.state) {
            *out = word.to_be_bytes();
        }
        digest
    }

    /// The digest as 64 lowercase hex characters.
    pub(crate) fn finish_hex(self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        // One allocation of exactly 64 characters, however the digest is used.
        let mut hex = String::with_capacity(64);
        for byte in self.finish() {
            hex.push(char::from(HEX[usize::from(byte >> 4)]));
            hex.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        hex
    }
}

/// The lowercase hex FIPS 180-4 SHA-256 of `bytes`.
// Only the Windows delivery replay and the tests call the one-shot form.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finish_hex()
}

/// One SHA-256 compression of `block` into `state`.
fn compress(state: &mut [u32; 8], block: &[u8; 64]) {
    let mut schedule = [0_u32; 64];
    for (word, word_bytes) in schedule.iter_mut().zip(block.as_chunks::<4>().0) {
        *word = u32::from_be_bytes(*word_bytes);
    }
    for index in 16..64 {
        let early = schedule[index - 15];
        let late = schedule[index - 2];
        let small_zero = early.rotate_right(7) ^ early.rotate_right(18) ^ (early >> 3);
        let small_one = late.rotate_right(17) ^ late.rotate_right(19) ^ (late >> 10);
        schedule[index] = schedule[index - 16]
            .wrapping_add(small_zero)
            .wrapping_add(schedule[index - 7])
            .wrapping_add(small_one);
    }
    let mut work = *state;
    for (constant, word) in ROUND_CONSTANTS.iter().zip(schedule) {
        let [first, second, third, fourth, fifth, sixth, seventh, eighth] = work;
        let big_one = fifth.rotate_right(6) ^ fifth.rotate_right(11) ^ fifth.rotate_right(25);
        let choose = (fifth & sixth) ^ (!fifth & seventh);
        let temp_one = eighth
            .wrapping_add(big_one)
            .wrapping_add(choose)
            .wrapping_add(*constant)
            .wrapping_add(word);
        let big_zero = first.rotate_right(2) ^ first.rotate_right(13) ^ first.rotate_right(22);
        let majority = (first & second) ^ (first & third) ^ (second & third);
        let temp_two = big_zero.wrapping_add(majority);
        work = [
            temp_one.wrapping_add(temp_two),
            first,
            second,
            third,
            fourth.wrapping_add(temp_one),
            fifth,
            sixth,
            seventh,
        ];
    }
    for (state_word, work_word) in state.iter_mut().zip(work) {
        *state_word = state_word.wrapping_add(work_word);
    }
}

#[cfg(test)]
#[path = "digest_tests.rs"]
mod digest_tests;
