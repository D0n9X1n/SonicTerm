//! Comment-preserving edits and verification for saved status-line lists.

use super::*;
use toml_edit::{DocumentMut, Item, Table};

pub(super) fn patch_status_line_values(
    document: &mut DocumentMut,
    save: &crate::status_overrides::StatusLineSave,
    disk: &crate::status_line::ResolvedStatusLine,
) -> Result<()> {
    let disk_save = crate::status_overrides::StatusLineSave::from_resolved(disk);
    let mut changes: Vec<(&str, toml_edit::Value)> = Vec::new();
    if save.enabled != disk_save.enabled {
        changes.push(("enabled", save.enabled.into()));
    }
    if save.hide_sensitive != disk_save.hide_sensitive {
        changes.push(("hide_sensitive", save.hide_sensitive.into()));
    }
    if save.position != disk_save.position {
        changes.push(("position", save.position.as_str().into()));
    }
    if save.font_scale != disk_save.font_scale {
        changes.push(("font_scale", save.font_scale.as_f64().into()));
    }
    let lists: Vec<(&str, &[crate::status_line::StatusSegmentId])> = ["left", "center", "right"]
        .into_iter()
        .enumerate()
        .filter(|(index, _)| save.lists[*index] != disk_save.lists[*index])
        .map(|(index, key)| (key, save.lists[index].as_slice()))
        .collect();
    if changes.is_empty() && lists.is_empty() {
        // When: changes is empty, the file already says what the session shows.
        return Ok(());
    }
    if document.get("status_line").is_none() {
        document.insert("status_line", Item::Table(Table::new()));
    }
    let table = document
        .get_mut("status_line")
        .and_then(Item::as_table_like_mut)
        .ok_or_else(|| anyhow::anyhow!("status_line must be a table or inline table"))?;
    for (key, mut value) in changes {
        if let Some(existing) = table.get(key).and_then(Item::as_value) {
            *value.decor_mut() = existing.decor().clone();
        }
        table.insert(key, Item::Value(value));
    }
    for (key, ids) in lists {
        if let Some(array) = table.get_mut(key).and_then(Item::as_array_mut) {
            // The file already has this list as an array, so it is edited in place.
            patch_list_in_place(array, ids);
        } else {
            // When: the key is absent or not an array, a fresh list is written.
            let list: toml_edit::Array = ids.iter().map(|id| id.as_str()).collect();
            table.insert(key, Item::Value(toml_edit::Value::Array(list)));
        }
    }
    if let Some(table) = document.get_mut("status_line").and_then(Item::as_table_mut) {
        // A table only implied by `[status_line.segment.*]` headers now holds keys of its own.
        table.set_implicit(false);
    }
    Ok(())
}

/// One array item and the text around it: `lead` is its own lines before it (own-line comments
/// and indentation), and `end` closes its line (the comment after it).
struct ListSlot {
    value: toml_edit::Value,
    lead: String,
    end: String,
}

/// Split array decoration at its first newline: the head closes the line before an item, and the
/// rest leads the item. Text without a newline is all lead.
fn split_line_end(text: &str) -> (&str, &str) {
    text.find('\n').map_or(("", text), |newline| text.split_at(newline + 1))
}

/// The text of one piece of decoration, empty when it is unset.
fn decor_text(raw: Option<&toml_edit::RawString>) -> &str {
    raw.and_then(toml_edit::RawString::as_str).unwrap_or("")
}

/// The comment lines of array decoration `text`, each with its line ending and without its
/// indentation.
fn comment_lines(text: &str) -> impl Iterator<Item = &str> {
    text.split_inclusive('\n').map(str::trim_start).filter(|line| line.starts_with('#'))
}

/// Rewrite `array` to hold exactly `ids`, in order, editing it in place. Save removes items,
/// never comments. The text between two items is one gap, which toml_edit splits between the
/// first item's suffix and the next item's prefix (or the array's trailing text); each gap is
/// split at its first newline into the line end of the item before it and the lead of the item
/// after it. An item that stays keeps its lead and its line end, a new item follows the list's
/// layout, and every comment of a removed item is kept, in order, as its own line before `]`.
fn patch_list_in_place(array: &mut toml_edit::Array, ids: &[crate::status_line::StatusSegmentId]) {
    let trailing = decor_text(Some(array.trailing())).to_owned();
    let prefixes: Vec<String> =
        array.iter().map(|value| decor_text(value.decor().prefix()).to_owned()).collect();
    let suffixes: Vec<String> =
        array.iter().map(|value| decor_text(value.decor().suffix()).to_owned()).collect();
    let multiline =
        [&trailing].into_iter().chain(&prefixes).chain(&suffixes).any(|text| text.contains('\n'));
    if !multiline {
        // When: `multiline` is false, the list holds no comment that could move.
        patch_single_line_list(array, ids);
        return;
    }
    // `gaps[index]` is everything between item `index` and the next item, or `]` after the last.
    let gaps: Vec<String> = (0..prefixes.len())
        .map(|index| format!("{}{}", suffixes[index], prefixes.get(index + 1).unwrap_or(&trailing)))
        .collect();
    let opening = prefixes.first().unwrap_or(&trailing);
    // The text before item `index`: the opening for the first item, else the previous gap.
    let before_item =
        |index: usize| index.checked_sub(1).map_or(opening, |previous| &gaps[previous]);
    let closing_rest = split_line_end(gaps.last().unwrap_or(&trailing)).1.to_owned();
    // The save path hands toml_edit LF text and restores CRLF after rendering.
    let newline = "\n";
    // New items and moved comments take the indentation of the last item that starts a line, or
    // of the first comment when the list has no such item.
    let indent = (0..prefixes.len())
        .rev()
        .find_map(|index| {
            let text = before_item(index);
            text.rfind('\n').map(|line_start| text[line_start + 1..].to_owned())
        })
        .or_else(|| {
            let first = closing_rest
                .split_inclusive('\n')
                .find(|line| line.trim_start().starts_with('#'))?;
            Some(first[..first.len() - first.trim_start().len()].to_owned())
        })
        .unwrap_or_default();
    let mut slots: Vec<Option<ListSlot>> = array
        .iter()
        .enumerate()
        .map(|(index, value)| {
            Some(ListSlot {
                value: value.clone(),
                lead: split_line_end(before_item(index)).1.to_owned(),
                end: split_line_end(&gaps[index]).0.to_owned(),
            })
        })
        .collect();
    // `closing` is the text that ends the line before the next item, starting at the bracket.
    let mut closing = split_line_end(opening).0.to_owned();
    let mut values = Vec::with_capacity(ids.len());
    for id in ids {
        let existing = slots
            .iter_mut()
            .find_map(|slot| slot.take_if(|slot| slot.value.as_str() == Some(id.as_str())));
        let slot = existing.unwrap_or_else(|| ListSlot {
            value: toml_edit::Value::from(id.as_str()),
            lead: indent.clone(),
            end: newline.to_owned(),
        });
        let mut value = slot.value;
        value.decor_mut().set_prefix(format!("{closing}{}", slot.lead));
        // The item's old suffix is part of a gap, which `closing` and the leads now carry.
        value.decor_mut().set_suffix("");
        closing = slot.end;
        values.push(value);
    }
    // Every item no id took is removed; its comments stay, in order, as own lines before `]`.
    let mut moved = String::new();
    for slot in slots.iter().flatten() {
        for line in comment_lines(&slot.lead).chain(comment_lines(&slot.end)) {
            moved.push_str(&indent);
            moved.push_str(line);
            if !line.ends_with('\n') {
                moved.push_str(newline);
            }
        }
    }
    if !moved.is_empty() && !closing.ends_with('\n') {
        // A moved comment starts its own line, so the last item's line is ended first.
        closing.push_str(newline);
    }
    array.clear();
    for value in values {
        array.push_formatted(value);
    }
    array.set_trailing(format!("{closing}{moved}{closing_rest}"));
}

/// Rewrite a one-line `array`, which holds no comments, to hold exactly `ids`: an item that stays
/// keeps its spacing, and a new item is separated by one space.
fn patch_single_line_list(
    array: &mut toml_edit::Array,
    ids: &[crate::status_line::StatusSegmentId],
) {
    let mut kept: Vec<Option<toml_edit::Value>> = array.iter().cloned().map(Some).collect();
    let mut values = Vec::with_capacity(ids.len());
    for (position, id) in ids.iter().enumerate() {
        let existing = kept
            .iter_mut()
            .find_map(|slot| slot.take_if(|value| value.as_str() == Some(id.as_str())));
        let value = existing.unwrap_or_else(|| {
            let mut value = toml_edit::Value::from(id.as_str());
            value.decor_mut().set_prefix(if position == 0 { "" } else { " " });
            value.decor_mut().set_suffix("");
            value
        });
        values.push(value);
    }
    array.clear();
    for value in values {
        array.push_formatted(value);
    }
}

#[cfg(test)]
#[path = "config_status_save_tests.rs"]
mod config_status_save_tests;
