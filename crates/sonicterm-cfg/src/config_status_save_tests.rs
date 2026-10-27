//! Saved status-line lists preserve comments, user edits and session choices.

use super::super::config_tests::font_persist_test_dir;
use super::super::persist::{apply_line_ending, line_ending_convention, normalize_for_toml_edit};
use super::*;

/// The lists `save` holds after a reload of `path`, resolved as a fresh start would.
fn reloaded_lists(path: &std::path::Path) -> crate::status_overrides::StatusLineSave {
    let config: Config = toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    crate::status_overrides::StatusLineSave::from_resolved(&config.status_line.resolve().unwrap())
}

/// The save the session would make from the file at `path` with `change` applied to its choices.
fn session_save(
    path: &std::path::Path,
    change: impl FnOnce(
        &mut crate::status_overrides::StatusLineOverrides,
        &crate::status_line::ResolvedStatusLine,
    ),
) -> crate::status_overrides::StatusLineSave {
    let config: Config = toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let file = config.status_line.resolve().unwrap();
    let mut overrides = crate::status_overrides::StatusLineOverrides::default();
    change(&mut overrides, &file);
    crate::status_overrides::StatusLineSave::from_resolved(&overrides.effective(&file))
}

#[test]
fn a_font_only_save_adds_no_status_line_keys() {
    // The session agrees with the file, so only the font values change.
    let dir = font_persist_test_dir("status-unchanged");
    let path = dir.join("sonicterm.toml");
    std::fs::write(&path, "# mine\n[font]\nsize = 13\nweight_scale = 1\n").unwrap();
    let save = session_save(&path, |_, _| {});
    Config::persist_runtime_values(&path, 14.0, 1.0, None, Some(&save)).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("status_line"), "{text}");
    assert!(text.starts_with("# mine\n") && text.contains("size = 14"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_save_writes_only_the_lists_that_differ_and_reloads_to_what_was_shown() {
    // Moving `cpu` out of the diagnostics preset's right list writes `left` and `right` as
    // explicit lists, never `center`, `enabled` or `preset`; a reload shows exactly those lists.
    use crate::status_line::StatusSegmentId as Seg;
    use crate::status_overrides::MoveDirection;
    let dir = font_persist_test_dir("status-lists");
    let path = dir.join("sonicterm.toml");
    let original = "[status_line]\n# keep me\nenabled = true\npreset = \"diagnostics\"\n";
    std::fs::write(&path, original).unwrap();
    let save = session_save(&path, |overrides, file| {
        overrides.move_segment(file, Seg::Cpu, MoveDirection::Left);
        overrides.move_segment(file, Seg::Cpu, MoveDirection::Left);
    });
    Config::persist_runtime_values(&path, 13.0, 1.0, None, Some(&save)).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("# keep me") && text.contains("preset = \"diagnostics\""), "{text}");
    assert!(text.contains("left = [") && text.contains("right = ["), "{text}");
    assert!(!text.contains("center ="), "an unchanged list is not written: {text}");
    assert_eq!(text.matches("enabled").count(), 1, "an unchanged key is not written: {text}");
    assert_eq!(reloaded_lists(&path), save, "the reload shows what was on screen");
    assert!(save.lists[0].contains(&Seg::Cpu));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_save_after_an_outside_edit_writes_only_what_still_differs() {
    // The file gained `cpu` on its own since the session last read it; the session's cpu
    // choice no longer differs, so only its `hide_sensitive` choice is written.
    use crate::status_line::StatusSegmentId as Seg;
    let dir = font_persist_test_dir("status-outside-edit");
    let path = dir.join("sonicterm.toml");
    std::fs::write(&path, "[status_line]\nright = [\"health\"]\n").unwrap();
    let save = session_save(&path, |overrides, file| {
        overrides.toggle_segment(file, Seg::Cpu);
        overrides.set_hide_sensitive(file, true);
    });
    std::fs::write(&path, "[status_line]\nright = [\"health\", \"cpu\"] # edited\n").unwrap();
    Config::persist_runtime_values(&path, 13.0, 1.0, None, Some(&save)).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("right = [\"health\", \"cpu\"] # edited"), "{text}");
    assert!(text.contains("hide_sensitive = true"), "{text}");
    assert_eq!(reloaded_lists(&path), save);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_status_line_save_edits_a_commented_list_in_place() {
    // A changed list is edited, not replaced: the items that stay keep their comments, the
    // removed item's comment moves to a line before `]`, and the array keeps its trailing comment.
    use crate::status_line::StatusSegmentId as Seg;
    let dir = font_persist_test_dir("status-array-comments");
    let path = dir.join("sonicterm.toml");
    let original = "[status_line]\nright = [\n    # always first\n    \"health\",\n    # the memory figure\n    \"mem\",\n    # end of list\n]\n";
    std::fs::write(&path, original).unwrap();
    let save = session_save(&path, |overrides, file| {
        overrides.toggle_segment(file, Seg::Mem);
        overrides.toggle_segment(file, Seg::Cpu);
    });
    Config::persist_runtime_values(&path, 13.0, 1.0, None, Some(&save)).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("    # always first\n    \"health\","), "{text}");
    assert!(!text.contains("\"mem\""), "the removed item is gone: {text}");
    let tail = "\n    \"cpu\",\n    # the memory figure\n    # end of list\n]";
    assert!(text.contains(tail), "the new item follows the layout, the comment moves: {text}");
    assert_eq!(reloaded_lists(&path), save);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_status_line_save_keeps_the_comment_on_the_line_before_a_removed_item() {
    // toml_edit stores a line-end comment in the next item's prefix; hiding that next item keeps
    // the comment on the line it was written on.
    use crate::status_line::StatusSegmentId as Seg;
    let dir = font_persist_test_dir("status-line-end-comment");
    let path = dir.join("sonicterm.toml");
    let original =
        "[status_line]\nright = [\n  \"mem\", # KEEP: explains memory\n  \"health\",\n]\n";
    std::fs::write(&path, original).unwrap();
    let save = session_save(&path, |overrides, file| overrides.toggle_segment(file, Seg::Health));
    Config::persist_runtime_values(&path, 13.0, 1.0, None, Some(&save)).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("right = [\n  \"mem\", # KEEP: explains memory\n]\n"), "{text}");
    assert_eq!(reloaded_lists(&path), save);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_status_line_save_keeps_the_list_wide_note_after_a_removed_last_item() {
    // Without a trailing comma toml_edit keeps the lines before `]` in the last item's suffix:
    // hiding that item keeps the list-wide note, and the item's own comment moves before `]`.
    use crate::status_line::StatusSegmentId as Seg;
    let dir = font_persist_test_dir("status-list-wide-note");
    let path = dir.join("sonicterm.toml");
    let original = "[status_line]\nright = [\n  \"health\", # health\n  \"mem\" # memory\n  # KEEP: list-wide note\n] # outside\n";
    std::fs::write(&path, original).unwrap();
    let save = session_save(&path, |overrides, file| overrides.toggle_segment(file, Seg::Mem));
    Config::persist_runtime_values(&path, 13.0, 1.0, None, Some(&save)).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    let expected =
        "right = [\n  \"health\" # health\n  # memory\n  # KEEP: list-wide note\n] # outside\n";
    assert!(text.contains(expected), "{text}");
    assert_eq!(reloaded_lists(&path), save);
    let _ = std::fs::remove_dir_all(dir);
}

/// The document `right = <array>` after `patch_list_in_place` rewrites the array to hold `ids`.
fn patched_right(array: &str, ids: &[crate::status_line::StatusSegmentId]) -> String {
    let mut document: DocumentMut = format!("right = {array}\n").parse().unwrap();
    let list = document.get_mut("right").and_then(Item::as_array_mut).unwrap();
    patch_list_in_place(list, ids);
    document.to_string()
}

#[test]
fn removing_the_first_middle_or_last_item_moves_its_comment_before_the_bracket() {
    // The items that stay keep their line-end comments; the removed item's comment is kept as its
    // own line just before `]`.
    use crate::status_line::StatusSegmentId::{Cpu, Health, Mem};
    let list = "[\n  \"health\", # h\n  \"mem\", # m\n  \"cpu\", # c\n]";
    let first = patched_right(list, &[Mem, Cpu]);
    assert_eq!(first, "right = [\n  \"mem\", # m\n  \"cpu\", # c\n  # h\n]\n");
    let middle = patched_right(list, &[Health, Cpu]);
    assert_eq!(middle, "right = [\n  \"health\", # h\n  \"cpu\", # c\n  # m\n]\n");
    let last = patched_right(list, &[Health, Mem]);
    assert_eq!(last, "right = [\n  \"health\", # h\n  \"mem\", # m\n  # c\n]\n");
    // Without a trailing comma the last item's comment is held in its suffix; it moves too.
    let no_comma = patched_right("[\n  \"health\", # h\n  \"mem\" # m\n]", &[Health]);
    assert_eq!(no_comma, "right = [\n  \"health\" # h\n  # m\n]\n");
}

#[test]
fn an_own_line_comment_before_a_removed_item_moves_before_the_bracket() {
    // Save removes items, never comments: the own-line comment before a removed item is kept, in
    // order with that item's line-end comment, before `]`; reordering moves it with its item.
    use crate::status_line::StatusSegmentId::{Cpu, Health, Mem};
    let list = "[\n  \"health\",\n  # about mem\n  \"mem\", # m\n  \"cpu\",\n]";
    let removed = patched_right(list, &[Health, Cpu]);
    assert_eq!(removed, "right = [\n  \"health\",\n  \"cpu\",\n  # about mem\n  # m\n]\n");
    let reordered = patched_right(list, &[Mem, Health]);
    assert_eq!(reordered, "right = [\n  # about mem\n  \"mem\", # m\n  \"health\",\n]\n");
    let emptied = patched_right(list, &[]);
    assert_eq!(emptied, "right = [\n  # about mem\n  # m\n]\n", "an empty list keeps them too");
}

/// Every comment in `text`, one entry per comment, sorted: what a save must neither lose nor
/// repeat. The test arrays hold no `#` inside a string.
fn comment_texts(text: &str) -> Vec<String> {
    let mut comments: Vec<String> = text
        .lines()
        .filter_map(|line| line.find('#').map(|start| line[start..].trim_end().to_owned()))
        .collect();
    comments.sort();
    comments
}

#[test]
fn a_list_edit_removes_items_but_never_a_comment() {
    // Over arrays with comments in every position (before items, after commas, held in the
    // closing text, on the `[` and `]` lines and after the array), with and without a trailing
    // comma, with LF and CRLF, removing the first, middle, last or every item or adding one: the
    // output parses, holds the requested list, keeps every comment exactly once, and leaves every
    // byte outside the array unchanged.
    use crate::status_line::StatusSegmentId as Seg;
    let listed = [
        // Own-line comments before each item, line-end comments, a closing note, trailing comma.
        "[\n  # lead health\n  \"health\", # h\n  # lead mem\n  \"mem\", # m\n  # lead cpu\n  \"cpu\", # c\n  # closing note\n]",
        // No trailing comma: the last line-end comment and the list-wide note are its suffix.
        "[\n  \"health\", # h\n  \"mem\", # m\n  \"cpu\" # c\n  # KEEP: list-wide note\n]",
        // A comment on the `[` line, blank lines and an own-line comment.
        "[ # opening note\n\n  \"health\",\n  # lead mem\n  \"mem\",\n\n  \"cpu\", # c\n]",
        // The last item shares the `]` line.
        "[\n  # lead health\n  \"health\", # h\n  \"mem\",\n  \"cpu\" ]",
        // Each comma follows a comment, on the next line.
        "[\n  \"health\" # h\n  , \"mem\" # m\n  # between\n  , \"cpu\"\n]",
    ];
    let edits: [&[Seg]; 5] = [
        &[Seg::Mem, Seg::Cpu],
        &[Seg::Health, Seg::Cpu],
        &[Seg::Health, Seg::Mem],
        &[],
        &[Seg::Health, Seg::Mem, Seg::Cpu, Seg::Link],
    ];
    let mut cases: Vec<(&str, &[Seg])> = Vec::new();
    for array in listed {
        cases.extend(edits.iter().map(|ids| (array, *ids)));
    }
    let empty = "[\n  # nothing listed yet\n]";
    cases.extend([(empty, &[][..]), (empty, &[Seg::Link][..])]);
    let mut checked = 0;
    for line_ending in [LineEnding::Lf, LineEnding::CrLf] {
        // CRLF goes through the save path's own conversion: LF for toml_edit, CRLF restored.
        let line_ends = |text: &str| apply_line_ending(text.to_owned(), line_ending);
        let before = line_ends("# head\n[status_line]\nenabled = true\nright = ");
        let after =
            line_ends(" # on the bracket line\n# after the array\nhide_sensitive = false\n");
        for (array, ids) in &cases {
            let original = format!("{before}{}{after}", line_ends(array));
            let mut document: DocumentMut =
                normalize_for_toml_edit(&original, line_ending).parse().unwrap();
            let list = document
                .get_mut("status_line")
                .and_then(Item::as_table_mut)
                .and_then(|table| table.get_mut("right"))
                .and_then(Item::as_array_mut)
                .unwrap();
            patch_list_in_place(list, ids);
            let output = apply_line_ending(document.to_string(), line_ending);
            let case = format!("{original:?} -> {ids:?}\n{output}");
            assert!(
                matches!(line_ending_convention(&output), Ok(kept) if kept == line_ending),
                "the line endings stay uniform: {case}"
            );
            let parsed: toml::Value =
                toml::from_str(&output).unwrap_or_else(|error| panic!("{case}\n{error}"));
            let written: Vec<&str> = parsed["status_line"]["right"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(toml::Value::as_str)
                .collect();
            let requested: Vec<&str> = ids.iter().map(|id| id.as_str()).collect();
            assert_eq!(written, requested, "{case}");
            assert_eq!(comment_texts(&output), comment_texts(&original), "{case}");
            assert!(output.starts_with(&format!("{before}[")), "{case}");
            assert!(output.ends_with(&format!("]{after}")), "{case}");
            checked += 1;
        }
    }
    assert_eq!(checked, 54, "every case ran");
}

#[test]
fn a_one_line_list_is_still_edited_by_the_one_line_path() {
    // A list without a newline holds no comment, so it goes through `patch_single_line_list` as
    // before and keeps its spacing.
    use crate::status_line::StatusSegmentId::{Cpu, Health, Mem};
    let one_line = [
        ("[\"health\", \"mem\"]", &[Health][..], Some("right = [\"health\"]\n")),
        (
            "[\"health\", \"mem\"]",
            &[Health, Mem, Cpu][..],
            Some("right = [\"health\", \"mem\", \"cpu\"]\n"),
        ),
        ("[ \"health\" , \"mem\" ]", &[Mem, Health][..], None),
        ("[]", &[Cpu][..], Some("right = [\"cpu\"]\n")),
    ];
    for (array, ids, expected) in one_line {
        let mut direct: DocumentMut = format!("right = {array}\n").parse().unwrap();
        patch_single_line_list(direct.get_mut("right").and_then(Item::as_array_mut).unwrap(), ids);
        let patched = patched_right(array, ids);
        assert_eq!(patched, direct.to_string(), "{array}");
        if let Some(expected) = expected {
            assert_eq!(patched, expected);
        }
    }
}

#[test]
fn a_save_that_would_not_load_or_reads_a_broken_file_changes_nothing() {
    // A save listing one id twice fails validation; an unparsable file on disk is never rewritten.
    use crate::status_line::StatusSegmentId as Seg;
    let dir = font_persist_test_dir("status-refused");
    let path = dir.join("sonicterm.toml");
    std::fs::write(&path, "[status_line]\nright = [\"health\"]\n").unwrap();
    let mut save = session_save(&path, |_, _| {});
    save.lists[0].push(Seg::Health);
    assert!(Config::persist_runtime_values(&path, 13.0, 1.0, None, Some(&save)).is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "[status_line]\nright = [\"health\"]\n");
    std::fs::write(&path, "[status_line\nbroken").unwrap();
    let save = crate::status_overrides::StatusLineSave::from_resolved(
        &crate::status_line::StatusLineConfig::default().resolve().unwrap(),
    );
    assert!(Config::persist_runtime_values(&path, 13.0, 1.0, None, Some(&save)).is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "[status_line\nbroken");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_status_line_save_keeps_crlf_and_unknown_keys_and_refuses_a_concurrent_change() {
    // The format-preserving write keeps line endings and unknown keys; a disk change between the
    // read and the replace is refused and kept.
    let dir = font_persist_test_dir("status-format");
    let path = dir.join("sonicterm.toml");
    std::fs::write(&path, "future_key = 1\r\n[status_line]\r\nenabled = false\r\n").unwrap();
    let save = session_save(&path, |overrides, file| overrides.set_enabled(file, true));
    Config::persist_runtime_values(&path, 13.0, 1.0, None, Some(&save)).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("future_key = 1\r\n") && text.contains("enabled = true\r\n"), "{text:?}");
    assert!(!text.replace("\r\n", "").contains('\n'), "LF stays CRLF: {text:?}");
    let concurrent = b"[status_line]\nenabled = false\n# theirs\n";
    std::fs::write(&path, "[status_line]\nenabled = false\n").unwrap();
    let result = Config::persist_runtime_values_before_commit(
        &path,
        13.0,
        1.0,
        None,
        Some(&save),
        |destination| {
            std::fs::write(destination, concurrent)
                .with_context(|| format!("write concurrent config at {destination:?}"))
        },
    );
    assert!(result.is_err());
    assert_eq!(std::fs::read(&path).unwrap(), concurrent);
    let _ = std::fs::remove_dir_all(dir);
}

/// Save writes `position` only when the session's differs from the file's, and a reload then
/// shows the saved position.
#[test]
fn a_status_line_save_writes_position_only_when_it_differs() {
    use crate::status_line::StatusPosition;
    let dir = font_persist_test_dir("status-position");
    let path = dir.join("sonicterm.toml");
    std::fs::write(&path, "[status_line]\nenabled = true\n").unwrap();
    let unchanged = session_save(&path, |_, _| {});
    Config::persist_runtime_values(&path, 13.0, 1.0, None, Some(&unchanged)).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("position"), "the file's position is not written: {text}");
    let mut moved = unchanged.clone();
    moved.position = StatusPosition::Bottom;
    Config::persist_runtime_values(&path, 13.0, 1.0, None, Some(&moved)).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("position = \"bottom\""), "{text}");
    assert_eq!(reloaded_lists(&path), moved, "the reload shows the saved position");
    let _ = std::fs::remove_dir_all(dir);
}

/// Save writes `font_scale` only when the session's differs from the file's, as `0.85`, and a
/// reload shows it.
#[test]
fn a_status_line_save_writes_font_scale_only_when_it_differs() {
    use crate::status_line::StatusFontScale;
    let dir = font_persist_test_dir("status-font-scale");
    let path = dir.join("sonicterm.toml");
    std::fs::write(&path, "[status_line]\nenabled = true\n").unwrap();
    let unchanged = session_save(&path, |_, _| {});
    Config::persist_runtime_values(&path, 13.0, 1.0, None, Some(&unchanged)).unwrap();
    assert!(!std::fs::read_to_string(&path).unwrap().contains("font_scale"));
    let mut larger = unchanged.clone();
    larger.font_scale = StatusFontScale::DEFAULT.stepped(1);
    Config::persist_runtime_values(&path, 13.0, 1.0, None, Some(&larger)).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("font_scale = 0.85"), "{text}");
    assert_eq!(reloaded_lists(&path), larger, "the reload shows the saved scale");
    let _ = std::fs::remove_dir_all(dir);
}

/// Save's rule for `[status_line]` keys: a key already in the file is updated in place, even to its
/// default; a key absent from the file is written only when the session differs from its default.
/// Here the file sets all three keys away from their defaults and the session shows the defaults,
/// so each key is rewritten where it stands, once, with its comment kept.
#[test]
fn a_status_line_save_updates_present_keys_in_place_even_to_their_defaults() {
    let dir = font_persist_test_dir("status-present-keys");
    let path = dir.join("sonicterm.toml");
    std::fs::write(
        &path,
        "[status_line]\nenabled = true # on\nposition = \"bottom\"\nfont_scale = 0.75\n",
    )
    .unwrap();
    let defaults = crate::status_overrides::StatusLineSave::from_resolved(
        &crate::status_line::StatusLineConfig::default().resolve().unwrap(),
    );
    Config::persist_runtime_values(&path, 13.0, 1.0, None, Some(&defaults)).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(
        text.contains(
            "[status_line]\nenabled = false # on\nposition = \"top\"\nfont_scale = 0.8\n"
        ),
        "each present key is rewritten in place: {text}"
    );
    for key in ["enabled", "position", "font_scale"] {
        assert_eq!(text.matches(&format!("{key} =")).count(), 1, "{key} once: {text}");
    }
    assert_eq!(reloaded_lists(&path), defaults, "the reload shows the session's defaults");
    let _ = std::fs::remove_dir_all(dir);
}

/// The other half of the rule: a file without `enabled`, `position` or `font_scale` stays without
/// them when the session shows their defaults.
#[test]
fn a_status_line_save_leaves_absent_keys_absent_at_their_defaults() {
    let dir = font_persist_test_dir("status-absent-keys");
    let path = dir.join("sonicterm.toml");
    std::fs::write(&path, "[status_line]\nhide_sensitive = false\n").unwrap();
    let defaults = crate::status_overrides::StatusLineSave::from_resolved(
        &crate::status_line::StatusLineConfig::default().resolve().unwrap(),
    );
    Config::persist_runtime_values(&path, 13.0, 1.0, None, Some(&defaults)).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    for key in ["enabled", "position", "font_scale"] {
        assert!(!text.contains(key), "{key} stays absent at its default: {text}");
    }
    let _ = std::fs::remove_dir_all(dir);
}
