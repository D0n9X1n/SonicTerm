use super::*;

fn source_text(source: &dwrote::TextAnalysisSource<'_>, position: u32) -> (Vec<u16>, u32) {
    let mut text = std::ptr::null();
    let mut length = 0;
    let mut locale_length = 0;
    let mut locale = std::ptr::null();
    // SAFETY: source owns the COM object and returned UTF-16 slice for the duration of these reads.
    unsafe {
        assert_eq!((*source.as_ptr()).GetTextAtPosition(position, &mut text, &mut length), 0);
        assert_eq!((*source.as_ptr()).GetLocaleName(position, &mut locale_length, &mut locale), 0);
        let units = if length == 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts(text, length as usize).to_vec()
        };
        (units, locale_length)
    }
}

// The real DirectWrite analysis source receives full UTF-16 and advances only at scalar boundaries.
#[test]
fn fallback_bridge_uses_complete_utf16_code_units() {
    for input in ["\u{1f600}", "AB", "\u{20000}", "A\u{1f600}B", "\u{1f600}\u{20000}"] {
        let chars: Vec<_> = input.chars().collect();
        let expected: Vec<_> = input.encode_utf16().collect();
        let mut spans = Vec::new();
        let candidates = map_fallback_candidates(&chars, |source, start, remaining| {
            let (text, locale_length) = source_text(source, start);
            assert_eq!(text, expected[start as usize..]);
            assert_eq!(remaining as usize, expected.len() - start as usize);
            assert_eq!(locale_length, remaining);
            let mapped = if (0xd800..=0xdbff).contains(&text[0]) { 2 } else { 1 };
            spans.push((start, remaining));
            (mapped, Some(FontAttributes::new("candidate")))
        })
        .unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(spans.len(), chars.len());
        assert_eq!(spans[0], (0, expected.len() as u32));
    }
}

// Empty requests do not call native mapping, and stable candidate order survives deduplication.
#[test]
fn fallback_candidates_preserve_order_and_skip_empty_requests() {
    assert!(map_fallback_candidates(&[], |_, _, _| panic!("empty mapping")).unwrap().is_empty());
    let candidates = map_fallback_candidates(&['A', 'B', 'C'], |_, start, _| {
        (1, Some(FontAttributes::new(if start == 1 { "second" } else { "first" })))
    })
    .unwrap();
    assert_eq!(
        candidates.iter().map(|font| font.family.as_str()).collect::<Vec<_>>(),
        ["first", "second"]
    );
}

// A later invalid span fails the request instead of returning an already accumulated candidate list.
#[test]
fn invalid_later_mapping_discards_partial_candidates() {
    let mut calls = 0;
    let result = map_fallback_candidates(&['A', '\u{1f600}', 'B'], |_, start, remaining| {
        calls += 1;
        if calls == 1 {
            assert_eq!((start, remaining), (0, 4));
            (1, Some(FontAttributes::new("first")))
        } else {
            assert_eq!((start, remaining), (1, 3));
            (1, Some(FontAttributes::new("split-surrogate")))
        }
    });
    assert!(result.is_err());
    assert_eq!(calls, 2);
}

// Native zero/overlong/split-surrogate spans fail without retrying or accepting a partial candidate.
#[test]
fn fallback_bridge_rejects_invalid_native_progress() {
    for (input, mapped) in [("A", 0), ("A", 2), ("\u{1f600}", 1), ("A\u{1f600}B", 2)] {
        let mut calls = 0;
        let chars: Vec<_> = input.chars().collect();
        let result = map_fallback_candidates(&chars, |_, _, _| {
            calls += 1;
            assert_eq!(calls, 1);
            (mapped, Some(FontAttributes::new("invalid")))
        });
        assert!(result.is_err());
        assert_eq!(calls, 1);
    }
}
