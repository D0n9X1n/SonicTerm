//! Pins how the Linux footprint reads `/proc/self/status`: both figures from one text, and a peak
//! that never reads below the resident set.

use super::*;

/// Status text with the given `VmRSS` and `VmHWM` values in KiB, as the kernel formats them.
fn status_text(resident_kib: u64, peak_kib: u64) -> String {
    format!(
        "Name:\tsonicterm\nVmHWM:\t{peak_kib:>8} kB\nVmRSS:\t{resident_kib:>8} kB\nThreads:\t4\n"
    )
}

#[test]
fn a_lagging_high_water_mark_is_raised_to_the_resident_set() {
    // The kernel can show `VmHWM` a page under `VmRSS`; the peak then reads as the resident set.
    let footprint = footprint_from_status(Some(&status_text(8196, 8192)));
    assert_eq!(footprint.current.bytes(), Some(8196 * 1024));
    assert_eq!(footprint.peak.bytes(), Some(8196 * 1024));
}

#[test]
fn a_true_peak_above_the_resident_set_is_kept() {
    // An ordinary high-water mark above the resident set is reported unchanged.
    let footprint = footprint_from_status(Some(&status_text(8192, 9000)));
    assert_eq!(footprint.current.bytes(), Some(8192 * 1024));
    assert_eq!(footprint.peak.bytes(), Some(9000 * 1024));
}

#[test]
fn an_unreadable_or_partial_status_reports_what_it_lacks_as_unsupported() {
    // No text gives no figures; a missing field is unsupported without disturbing the other.
    let unread = footprint_from_status(None);
    assert_eq!(unread.current.bytes(), None);
    assert_eq!(unread.peak.bytes(), None);
    let partial = footprint_from_status(Some("VmRSS:\t   100 kB\n"));
    assert_eq!(partial.current.bytes(), Some(100 * 1024));
    assert_eq!(partial.peak.bytes(), None);
}
