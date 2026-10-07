//! Known answers and streaming equivalence for the harness's SHA-256.

use super::*;

#[test]
fn sha256_matches_published_vectors() {
    // The S11 payload check is only as good as this hasher, so it is checked against FIPS 180-4.
    let empty = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    assert_eq!(sha256_hex(b""), empty);
    let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    assert_eq!(sha256_hex(b"abc"), abc);
    let two_blocks = "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1";
    assert_eq!(sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"), two_blocks);
    let million_a = "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0";
    assert_eq!(sha256_hex(&vec![b'a'; 1_000_000]), million_a);
}

/// Every split of a message into two updates, across the block and padding boundaries, gives the one-shot
/// digest: the partial-block path and the 55/56/63/64-byte padding cases each agree.
#[test]
fn chunked_updates_match_the_one_shot_digest() {
    for length in [0_usize, 1, 55, 56, 57, 63, 64, 65, 119, 120, 127, 128, 129, 1000] {
        let message: Vec<u8> = (0..length).map(|index| (index * 31 % 251) as u8).collect();
        let expected = sha256_hex(&message);
        for split in 0..=length {
            let mut hasher = Sha256::new();
            hasher.update(&message[..split]);
            hasher.update(&message[split..]);
            assert_eq!(hasher.finish_hex(), expected, "length {length}, split {split}");
        }
        let mut bytewise = Sha256::new();
        for byte in &message {
            bytewise.update(std::slice::from_ref(byte));
        }
        assert_eq!(bytewise.finish_hex(), expected, "length {length}, one byte at a time");
    }
}

/// The million-'a' vector hashed through 4 KiB updates, as a streamed sidecar is, matches the published value.
#[test]
fn a_large_stream_matches_the_published_vector() {
    let mut hasher = Sha256::new();
    let chunk = [b'a'; 4096];
    let mut left = 1_000_000_usize;
    while left > 0 {
        let take = left.min(chunk.len());
        hasher.update(&chunk[..take]);
        left -= take;
    }
    assert_eq!(
        hasher.finish_hex(),
        "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
    );
}
