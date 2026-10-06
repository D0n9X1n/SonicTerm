//! A font face's content identity: the SHA-256 of the bytes it was loaded from, in the namespace of
//! where those bytes came from. Two files that share a name but differ in content never share an
//! identity, and one face's identity is the same on every checkout and host path.

/// Where a face's bytes came from. Each source has its own namespace, so equal bytes from a file,
/// from built-in data and from memory never share an identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaceNamespace {
    /// A font file on disk.
    File,
    /// Font data compiled into the binary.
    BuiltIn,
    /// Font data handed over in memory.
    Memory,
}

impl FaceNamespace {
    /// Every namespace, in the order an identity's prefix is checked.
    pub const ALL: [Self; 3] = [Self::File, Self::BuiltIn, Self::Memory];

    /// The identity's prefix for this namespace.
    #[must_use]
    pub fn prefix(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::BuiltIn => "builtin",
            Self::Memory => "memory",
        }
    }
}

/// The content identity of a face loaded from `bytes` in `namespace`: `<namespace>:sha256:<hex>`,
/// with 64 lowercase hex digits. The face index within a collection is kept apart from it.
#[must_use]
pub fn face_content_id(namespace: FaceNamespace, bytes: &[u8]) -> String {
    format!("{}:sha256:{}", namespace.prefix(), sha256_hex(bytes))
}

/// Whether `content` is a well-formed identity: a known namespace, `sha256`, then 64 lowercase hex
/// digits.
#[must_use]
pub fn is_face_content_id(content: &str) -> bool {
    let mut parts = content.splitn(3, ':');
    let (Some(namespace), Some(algorithm), Some(digest)) =
        (parts.next(), parts.next(), parts.next())
    else {
        // When: `content` has fewer than three `:`-separated parts, it names no namespace and digest.
        return false;
    };
    FaceNamespace::ALL.iter().any(|known| known.prefix() == namespace)
        && algorithm == "sha256"
        && digest.len() == 64
        && digest.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

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

/// The lowercase hex FIPS 180-4 SHA-256 of `bytes`, by hand, so the identity needs no crate.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut state: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let (blocks, remainder) = bytes.as_chunks::<64>();
    for block in blocks {
        compress(&mut state, block);
    }
    // Padding: one 0x80 byte, zeros to 56 bytes past a block boundary, then the bit length.
    let mut tail = remainder.to_vec();
    tail.push(0x80);
    while tail.len() % 64 != 56 {
        tail.push(0);
    }
    tail.extend_from_slice(&(bytes.len() as u64).wrapping_mul(8).to_be_bytes());
    for block in tail.as_chunks::<64>().0 {
        compress(&mut state, block);
    }
    state.iter().map(|word| format!("{word:08x}")).collect()
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
#[path = "face_content_tests.rs"]
mod face_content_tests;
