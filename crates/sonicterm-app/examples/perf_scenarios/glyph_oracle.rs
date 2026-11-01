//! Test-only helper tiles shared across exact-run Windows CI suites; renderers are never shared.
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use serde::{Deserialize, Serialize};
use sonicterm_gpu::glyph_working_set::{GlyphWorkingSet, TileIdentity};

use crate::digest::sha256_hex;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Row {
    schema: u32,
    fixture: String,
    dpi: usize,
    family: String,
    size: String,
    tiles: BTreeMap<String, [u32; 2]>,
}

/// Portable exact tile identity: actual font bytes replace machine-local file paths.
pub(crate) fn identities(
    tiles: &HashMap<TileIdentity, [u32; 2]>,
) -> Result<BTreeMap<String, [u32; 2]>, String> {
    let mut sources = HashMap::new();
    let mut result = BTreeMap::new();
    for (identity, dimensions) in tiles {
        let source = &identity.source.face.source;
        let content = match sources.get(source) {
            Some(content) => content,
            None => {
                let content = portable_source(source)?;
                sources.entry(source.clone()).or_insert(content)
            }
        };
        let mut portable = identity.clone();
        portable.source.face.source = content.clone();
        let key = format!("{portable:?}");
        if let Some(previous) = result.insert(key, *dimensions) {
            if previous != *dimensions {
                return Err("one content identity has conflicting raster sizes".into());
            }
        }
    }
    Ok(result)
}

fn portable_source(source: &str) -> Result<String, String> {
    // Built-ins belong to the source digest; GDI memory names belong to the verified font/registry inventory.
    if source.starts_with("builtin:") || source.starts_with("memory:") {
        return Ok(source.to_owned());
    }
    let bytes = std::fs::read(source).map_err(|error| format!("font {source}: {error}"))?;
    Ok(format!("file:{}", sha256_hex(&bytes)))
}

/// Export only the producer's live measurement; the caller still runs every drift/completeness check.
pub(crate) fn export(fixture: &str, dpi: usize, set: &GlyphWorkingSet) -> Result<(), String> {
    let Some(directory) = std::env::var_os("SONICTERM_GLYPH_ORACLE_EXPORT") else {
        return Ok(());
    };
    let row = Row {
        schema: 1,
        fixture: fixture.into(),
        dpi,
        family: crate::workload::FONT_FAMILY.into(),
        size: crate::workload::FONT_SIZE.into(),
        tiles: identities(&set.tile_identities)?,
    };
    let bytes = serde_json::to_vec(&row).map_err(|error| error.to_string())?;
    std::fs::write(Path::new(&directory).join(format!("{fixture}-{dpi}.json")), bytes)
        .map_err(|error| error.to_string())
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn decode(bytes: &[u8], fixture: &str, dpi: usize) -> Result<BTreeMap<String, [u32; 2]>, String> {
    let row: Row = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    if row.schema != 1
        || row.fixture != fixture
        || row.dpi != dpi
        || row.family != crate::workload::FONT_FAMILY
        || row.size != crate::workload::FONT_SIZE
        || row.tiles.is_empty()
        || row.tiles.values().any(|size| size.contains(&0))
    {
        return Err("helper row does not describe the requested complete fixture/strike".into());
    }
    Ok(row.tiles)
}

/// Whether the workflow's verify step chose local measurement: the producer ran on another runner
/// image, so its helper tiles may not match this runner's fonts.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn measures_locally(mode: Option<&std::ffi::OsStr>) -> bool {
    mode.is_some_and(|mode| mode == "local")
}

/// Read the verified producer artifact, or measure locally when no artifact was requested or the
/// producer ran on another runner image.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(crate) fn load(
    fixture: &str,
    dpi: usize,
) -> Result<Option<BTreeMap<String, [u32; 2]>>, String> {
    let Some(directory) = std::env::var_os("SONICTERM_GLYPH_ORACLE_DIR") else {
        return Ok(None);
    };
    if measures_locally(std::env::var_os("SONICTERM_GLYPH_ORACLE_MODE").as_deref()) {
        // When: measures_locally holds, the artifact is from another runner image and is not read.
        return Ok(None);
    }
    let expected =
        std::env::var("SONICTERM_GLYPH_ORACLE_DIGEST").map_err(|error| error.to_string())?;
    load_verified(Path::new(&directory), &expected, fixture, dpi).map(Some)
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn load_verified(
    directory: &Path,
    expected: &str,
    fixture: &str,
    dpi: usize,
) -> Result<BTreeMap<String, [u32; 2]>, String> {
    let manifest =
        std::fs::read(directory.join("manifest.json")).map_err(|error| error.to_string())?;
    if sha256_hex(&manifest) != expected {
        return Err("helper manifest differs from the verified producer output".into());
    }
    let manifest: serde_json::Value =
        serde_json::from_slice(&manifest).map_err(|error| error.to_string())?;
    let name = format!("{fixture}-{dpi}.json");
    let bytes = std::fs::read(directory.join(&name)).map_err(|error| error.to_string())?;
    if manifest["files"][&name].as_str() != Some(sha256_hex(&bytes).as_str()) {
        return Err(format!("helper row {name} differs from the verified producer output"));
    }
    decode(&bytes, fixture, dpi)
}

#[cfg(test)]
#[path = "glyph_oracle_tests.rs"]
mod glyph_oracle_tests;
