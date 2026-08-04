use super::*;

#[test]
#[deny(unused_unsafe)]
fn raw_pointer_ownership_constructors_remain_unsafe() {
    if false {
        let _ =
            // SAFETY: this branch never executes; the call exists only to require Blob's unsafe API.
            unsafe { Blob::with_reference(std::ptr::null_mut()) };
        let _ =
            // SAFETY: this branch never executes; the call exists only to require Font's unsafe API.
            unsafe { Font::new(std::ptr::null_mut()) };
    }
}

#[test]
fn on_disk_blobs_copy_bytes_instead_of_mapping_mutable_files() {
    const SOURCE: &str = include_str!("hbwrap.rs");

    assert!(!SOURCE.contains("MmapOptions"));
    assert!(!SOURCE.contains("release_arc_mmap"));
    assert!(SOURCE.contains("file.read_to_end(&mut data)"));
}

#[test]
fn on_disk_blob_survives_source_file_replacement() {
    let original = b"owned harfbuzz blob bytes";
    let path =
        std::env::temp_dir().join(format!("sonicterm-hbwrap-blob-{}.bin", std::process::id()));
    std::fs::write(&path, original).unwrap();
    let blob = Blob::from_source(&FontDataSource::OnDisk(path.clone())).unwrap();

    std::fs::write(&path, b"replacement").unwrap();
    assert_eq!(blob.as_slice(), original);

    drop(blob);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn blob_lengths_are_checked_and_mmap_dependency_is_removed() {
    assert_eq!(checked_blob_len(0).unwrap(), 0);
    assert_eq!(checked_blob_len(c_uint::MAX as usize).unwrap(), c_uint::MAX);
    #[cfg(target_pointer_width = "64")]
    assert!(checked_blob_len(c_uint::MAX as usize + 1).is_err());

    const MANIFEST: &str = include_str!("../Cargo.toml");
    assert!(!MANIFEST.lines().any(|line| line.trim_start().starts_with("memmap2 =")));
}

#[test]
fn blob_creation_failure_cleanup_is_left_to_harfbuzz() {
    const SOURCE: &str = include_str!("hbwrap.rs");

    assert_eq!(SOURCE.matches("release_arc_vec(user_data").count(), 1);
    assert_eq!(SOURCE.matches("release_arc(user_data").count(), 1);
}

#[test]
fn callback_table_validation_rejects_null_and_empty_singletons() {
    let mut empty = 0u8;
    let mut owned = 0u8;
    let empty_ptr = &mut empty as *mut u8;
    let owned_ptr = &mut owned as *mut u8;

    assert!(owned_callback_table(std::ptr::null_mut::<u8>(), empty_ptr, "callback").is_err());
    assert!(owned_callback_table(empty_ptr, empty_ptr, "callback").is_err());
    assert_eq!(owned_callback_table(owned_ptr, empty_ptr, "callback").unwrap(), owned_ptr);
}
