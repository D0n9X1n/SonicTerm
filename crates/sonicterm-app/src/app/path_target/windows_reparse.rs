//! Windows reparse-buffer decoding and link-target syntax.

use super::*;

/// Parse the target text of a symlink or junction. Only an NT drive path `\??\X:\…` and a target
/// relative to the link's folder name a local path; bare and `\\?\` drive paths, UNC, device,
/// volume-GUID, drive-relative and root-relative targets, `/` separators, `..` after a name, and
/// alternate-stream or reserved names return `None`.
pub(super) fn parse_link_target(raw: &str) -> Option<LinkTarget> {
    if raw.contains('/') {
        // When: `raw` contains `/`, refuse it; NT link text separates names only with `\`.
        return None;
    }
    if let Some(nt_path) = raw.strip_prefix(r"\??\") {
        // When: `raw` is an NT path, only a drive root below `\??\` is local; UNC, volume-GUID and device forms fail `split_drive_root`.
        let (drive, rest) = split_drive_root(nt_path)?;
        return parse_absolute_names(rest).map(|names| LinkTarget::Absolute { drive, names });
    }
    if raw.starts_with('\\') || raw.contains(':') {
        // When: `raw` starts with `\` or holds `:`, it is a `\\?\`, UNC, device, rooted, drive or drive-relative form.
        return None;
    }
    let (parent_steps, names) = parse_relative_names(raw)?;
    Some(LinkTarget::Relative { parent_steps, names })
}

/// Split `X:\rest` or `X:/rest` into the upper-case drive letter and the text after its root.
pub(super) fn split_drive_root(path: &str) -> Option<(char, &str)> {
    let bytes = path.as_bytes();
    let rooted = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/');
    if !rooted {
        // When: `rooted` is false, `path` has no drive letter followed by `:` and a separator.
        return None;
    }
    Some((char::from(bytes[0]).to_ascii_uppercase(), &path[3..]))
}

/// Split the names below an absolute target's drive root. NT resolves such a target without Win32
/// normalization, so a `.`, `..`, empty, or reserved name refuses the whole target.
pub(super) fn parse_absolute_names(rest: &str) -> Option<Vec<String>> {
    let rest = rest.strip_suffix('\\').unwrap_or(rest);
    if rest.is_empty() {
        // When: `rest` is empty after one trailing separator, the target is the drive root itself.
        return Some(Vec::new());
    }
    rest.split('\\').map(|name| windows_name_allowed(name).then(|| name.to_string())).collect()
}

/// Split a target relative to the link's folder into leading `..` climbs and the names below. A
/// `..` after a name is refused: if that name is itself a link, the folder `..` reaches depends
/// on whether Windows resolves the link first, so the walk cannot know it.
pub(super) fn parse_relative_names(target: &str) -> Option<(usize, Vec<String>)> {
    let target = target.strip_suffix('\\').unwrap_or(target);
    let mut parent_steps = 0;
    let mut names = Vec::new();
    for part in target.split('\\') {
        match part {
            "." => {
                // When: `part` is `.`, it names the folder already reached and adds no name.
            }
            ".." if names.is_empty() => parent_steps += 1,
            _ if windows_name_allowed(part) => names.push(part.to_string()),
            _ => {
                // When: `part` is a `..` after a name, empty, or reserved, refuse the target rather than guess its folder.
                return None;
            }
        }
    }
    Some((parent_steps, names))
}

/// The little-endian `u16` at `offset` in `bytes`, widened, or `None` past the end.
pub(super) fn read_u16_le(bytes: &[u8], offset: usize) -> Option<usize> {
    let pair: [u8; 2] = bytes.get(offset..offset.checked_add(2)?)?.try_into().ok()?;
    Some(usize::from(u16::from_le_bytes(pair)))
}

/// Decode the substitute name of a symlink or junction from the `REPARSE_DATA_BUFFER` that
/// `FSCTL_GET_REPARSE_POINT` returns, following its documented layout. A symlink whose relative
/// flag disagrees with whether its text starts with `\`, a junction whose text does not, any other
/// tag, and a buffer too short for the lengths it declares return `None`.
pub(super) fn decode_reparse_target(buffer: &[u8]) -> Option<String> {
    let tag_bytes: [u8; 4] = buffer.get(..4)?.try_into().ok()?;
    let data_length = read_u16_le(buffer, 4)?;
    let data = buffer.get(8..8 + data_length)?;
    let (path_start, relative) = match u32::from_le_bytes(tag_bytes) {
        IO_REPARSE_TAG_SYMLINK => {
            let flag_bytes: [u8; 4] = data.get(8..12)?.try_into().ok()?;
            (12, u32::from_le_bytes(flag_bytes) & SYMLINK_FLAG_RELATIVE != 0)
        }
        IO_REPARSE_TAG_MOUNT_POINT => (8, false),
        _ => {
            // When: `tag_bytes` hold neither a symlink nor a junction tag, so the data names no target the walk can check.
            return None;
        }
    };
    let name_start = path_start + read_u16_le(data, 0)?;
    let name_bytes = data.get(name_start..name_start + read_u16_le(data, 2)?)?;
    let (pairs, remainder) = name_bytes.as_chunks::<2>();
    if !remainder.is_empty() {
        // When: `remainder` holds a byte, the substitute name has an odd byte length and cannot be UTF-16 text.
        return None;
    }
    let units = pairs.iter().copied().map(u16::from_le_bytes).collect::<Vec<_>>();
    let target = String::from_utf16(&units).ok()?;
    if target.is_empty() || relative == target.starts_with('\\') {
        // When: `target` is empty, or `relative` disagrees with a leading `\`, the link names no path the walk can place.
        return None;
    }
    Some(target)
}

#[cfg(test)]
#[path = "windows_reparse_tests.rs"]
mod windows_reparse_tests;
