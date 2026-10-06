use super::*;

/// The digest is FIPS 180-4 SHA-256: the standard vectors for the empty input, "abc" and a
/// two-block message, and a million-byte input that crosses many block boundaries.
#[test]
fn the_digest_matches_the_standard_sha256_vectors() {
    assert_eq!(sha256_hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
    );
    assert_eq!(
        sha256_hex(&vec![b'a'; 1_000_000]),
        "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
    );
}

/// An identity names its namespace and the bytes' digest; equal bytes in different namespaces, or
/// different bytes in one namespace, never share an identity.
#[test]
fn an_identity_separates_namespaces_and_contents() {
    let file = face_content_id(FaceNamespace::File, b"face");
    assert_eq!(file, format!("file:sha256:{}", sha256_hex(b"face")));
    assert_ne!(file, face_content_id(FaceNamespace::BuiltIn, b"face"), "file vs builtin");
    assert_ne!(file, face_content_id(FaceNamespace::Memory, b"face"), "file vs memory");
    assert_ne!(
        face_content_id(FaceNamespace::BuiltIn, b"face"),
        face_content_id(FaceNamespace::Memory, b"face"),
        "builtin vs memory"
    );
    assert_ne!(file, face_content_id(FaceNamespace::File, b"other face"), "other bytes");
}

/// Only a known namespace, `sha256` and 64 lowercase hex digits form an identity; a file name, a
/// path, a bare digest, an unknown namespace or a short or uppercase digest does not.
#[test]
fn only_a_namespaced_sha256_digest_is_an_identity() {
    for namespace in FaceNamespace::ALL {
        assert!(is_face_content_id(&face_content_id(namespace, b"face")), "{namespace:?}");
    }
    let digest = sha256_hex(b"face");
    let refused = [
        String::new(),
        "seguiemj.ttf".to_owned(),
        "/System/Library/Fonts/ReviewedFace.ttf".to_owned(),
        digest.clone(),
        format!("font:sha256:{digest}"),
        format!("file:sha1:{digest}"),
        format!("file:sha256:{}", &digest[..63]),
        format!("file:sha256:{}", digest.to_uppercase()),
        format!("file:sha256:{digest}0"),
    ];
    for content in refused {
        assert!(!is_face_content_id(&content), "{content:?} is not an identity");
    }
}
