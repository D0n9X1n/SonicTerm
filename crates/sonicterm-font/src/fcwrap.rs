//! Slightly higher level helper for fontconfig
#![allow(clippy::mutex_atomic)]

use anyhow::{anyhow, ensure, Error};
use config::{FontStretch, FontWeight};
pub use fontconfig::*;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};
use std::{fmt, mem, ptr};

pub const FC_CHARCELL: i32 = 110;
pub const FC_MONO: i32 = 100;
pub const FC_DUAL: i32 = 90;

pub struct FontSet {
    fonts: *mut FcFontSet,
}

// Lifecycle: `FontSet` releases its owned `FcFontSet` with `FcFontSetDestroy` once.
impl Drop for FontSet {
    fn drop(&mut self) {
        // SAFETY: `self.fonts` is the live set pointer owned by this wrapper.
        unsafe {
            FcFontSetDestroy(self.fonts);
        }
    }
}

impl fmt::Debug for FontSet {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        fmt.debug_list().entries(self.iter()).finish()
    }
}

pub struct FontSetIter<'a> {
    set: &'a FontSet,
    position: isize,
}

impl<'a> Iterator for FontSetIter<'a> {
    type Item = Pattern;

    fn next(&mut self) -> Option<Self::Item> {
        // SAFETY: `self.set.fonts` is live for `'a`; its `fonts` array contains `nfont`
        // initialized pattern pointers, and `position` advances from zero without exceeding it.
        unsafe {
            if self.position == (*self.set.fonts).nfont as isize {
                None
            } else {
                // When: `position == nfont` is false, reference the next initialized pattern.
                let pat = *(*self.set.fonts).fonts.offset(self.position).as_mut().unwrap();
                FcPatternReference(pat);
                self.position += 1;
                Some(Pattern { pat })
            }
        }
    }
}

impl FontSet {
    /// Iterates over owned references to the patterns in this font set.
    pub fn iter(&self) -> FontSetIter<'_> {
        FontSetIter { set: self, position: 0 }
    }
}

#[repr(C)]
pub enum MatchKind {
    Pattern = FcMatchPattern as isize,
}

pub struct FcResultWrap(FcResult);

impl FcResultWrap {
    /// Reports whether Fontconfig returned `FcResultMatch`.
    pub fn succeeded(&self) -> bool {
        self.0 == FcResultMatch
    }

    /// Converts the wrapped Fontconfig result code into a descriptive error.
    pub fn as_err(&self) -> Error {
        // the compiler thinks we defined these globals, when all
        // we did was import them from elsewhere
        match self.0 {
            fontconfig::FcResultMatch => anyhow!("FcResultMatch"),
            fontconfig::FcResultNoMatch => anyhow!("FcResultNoMatch"),
            fontconfig::FcResultTypeMismatch => anyhow!("FcResultTypeMismatch"),
            fontconfig::FcResultNoId => anyhow!("FcResultNoId"),
            fontconfig::FcResultOutOfMemory => anyhow!("FcResultOutOfMemory"),
            _ => anyhow!("FcResult holds invalid value {}", self.0),
        }
    }

    /// Returns a value for `FcResultMatch` or the wrapped Fontconfig error.
    pub fn result<T>(&self, t: T) -> Result<T, Error> {
        #[allow(non_upper_case_globals)]
        match self.0 {
            FcResultMatch => Ok(t),
            _ => Err(self.as_err()),
        }
    }
}

pub struct CharSet {
    cset: *mut FcCharSet,
}

pub struct CharSetRef<'a> {
    cset: *mut FcCharSet,
    phantom: std::marker::PhantomData<&'a FcCharSet>,
}

impl<'a> CharSetRef<'a> {
    /// Converts this borrowed Fontconfig character set into contiguous codepoint ranges.
    pub fn to_range_set(&self) -> crate::rangeset::RangeSet<u32> {
        let mut coverage = crate::rangeset::RangeSet::new();
        let mut next_base_code_point = FcChar32::default();
        const FC_CHARSET_MAP_SIZE: usize = 256 / 32;
        const FC_CHARSET_DONE: FcChar32 = FcChar32::MAX;
        let mut map = [FcChar32::default(); FC_CHARSET_MAP_SIZE];
        let mut base_code_point =
            // SAFETY: `self.cset` is live for `'a`; `map` provides eight writable `FcChar32`
            // entries and `next_base_code_point` is writable cursor output.
            unsafe { FcCharSetFirstPage(self.cset, map.as_mut_ptr(), &mut next_base_code_point) };
        let mut range_start = FcChar32::MAX;
        let mut code_point = FcChar32::MAX;
        while base_code_point != FC_CHARSET_DONE {
            for (i, mask) in map.iter().enumerate() {
                for j in 0..32 {
                    if mask & (1 << j) != 0 {
                        let new_code_point = base_code_point + (j + i * 32) as u32;
                        if new_code_point > 0 && new_code_point - 1 > code_point {
                            coverage.add_range_unchecked(range_start..code_point + 1);
                            range_start = new_code_point;
                        }
                        if range_start == FcChar32::MAX {
                            range_start = new_code_point;
                        }
                        code_point = new_code_point;
                    }
                }
            }
            base_code_point =
                // SAFETY: the charset and eight-entry map remain live, and the cursor output is
                // initialized by the prior page call before advancing.
                unsafe {
                    FcCharSetNextPage(
                        self.cset,
                        map.as_mut_ptr(),
                        &mut next_base_code_point,
                    )
                };
        }
        if range_start != FcChar32::MAX {
            coverage.add_range_unchecked(range_start..code_point + 1);
        }
        coverage
    }
}

// Lifecycle: `CharSet` releases its owned `FcCharSet` with `FcCharSetDestroy` once.
impl Drop for CharSet {
    fn drop(&mut self) {
        // SAFETY: `self.cset` is the live charset pointer owned by this wrapper.
        unsafe {
            FcCharSetDestroy(self.cset);
        }
    }
}

impl<'a> From<&'a CharSet> for CharSetRef<'a> {
    fn from(c: &'a CharSet) -> Self {
        Self { cset: c.cset, phantom: std::marker::PhantomData }
    }
}

impl CharSet {
    /// Creates an empty owned Fontconfig character set.
    pub fn new() -> anyhow::Result<Self> {
        // SAFETY: Fontconfig returns a newly owned charset pointer or null on failure.
        unsafe {
            let cset = FcCharSetCreate();
            ensure!(!cset.is_null(), "FcCharSetCreate failed");
            Ok(Self { cset })
        }
    }

    /// Adds one Unicode scalar value to this character set.
    pub fn add(&mut self, c: char) -> anyhow::Result<()> {
        // SAFETY: `self.cset` is live and `char` converts to a valid Unicode codepoint value.
        unsafe {
            ensure!(FcCharSetAddChar(self.cset, c as u32) != 0, "FcCharSetAddChar failed");
            Ok(())
        }
    }
}

pub struct Pattern {
    pat: *mut FcPattern,
}

impl Pattern {
    /// Creates an empty owned Fontconfig pattern.
    pub fn new() -> Result<Pattern, Error> {
        // SAFETY: Fontconfig returns a newly owned pattern pointer or null on failure.
        unsafe {
            let p = FcPatternCreate();
            ensure!(!p.is_null(), "FcPatternCreate failed");
            Ok(Pattern { pat: p })
        }
    }

    /// Borrows the first character-set property from this pattern.
    pub fn get_charset<'a>(&'a self) -> anyhow::Result<CharSetRef<'a>> {
        let mut c = ptr::null_mut();
        // SAFETY: `self.pat` is live, the property name is NUL-terminated, and `c` is writable
        // output; a matching return initializes it to pattern-owned charset storage.
        unsafe {
            FcPatternGetCharSet(self.pat, b"charset\0".as_ptr() as *const c_char, 0, &mut c);
        }
        ensure!(!c.is_null(), "pattern has no charset");
        Ok(CharSetRef { cset: c, phantom: std::marker::PhantomData })
    }

    /// Adds a referenced character-set property to this pattern.
    pub fn add_charset(&mut self, charset: &CharSet) -> anyhow::Result<()> {
        // SAFETY: both pattern and charset pointers are live and the property name is
        // NUL-terminated; Fontconfig copies or references the value per its pattern contract.
        unsafe {
            ensure!(
                FcPatternAddCharSet(self.pat, b"charset\0".as_ptr() as *const c_char, charset.cset)
                    != 0,
                "failed to add charset property"
            );
            Ok(())
        }
    }

    /// Counts codepoints shared by this pattern's charset and another charset.
    pub fn charset_intersect_count(&self, charset: &CharSet) -> anyhow::Result<u32> {
        // SAFETY: both wrappers hold live pointers, the property name is NUL-terminated, and `c`
        // is writable output initialized to pattern-owned charset storage when present.
        unsafe {
            let mut c = ptr::null_mut();
            FcPatternGetCharSet(self.pat, b"charset\0".as_ptr() as *const c_char, 0, &mut c);
            ensure!(!c.is_null(), "pattern has no charset");
            Ok(FcCharSetIntersectCount(c, charset.cset))
        }
    }

    /// Adds a string-valued property to this pattern.
    pub fn add_string(&mut self, key: &str, value: &str) -> Result<(), Error> {
        let key = CString::new(key)?;
        let value = CString::new(value)?;
        // SAFETY: the pattern is live; both C strings are NUL-terminated and remain readable for
        // the synchronous call, which copies the property value into the pattern.
        unsafe {
            ensure!(
                FcPatternAddString(self.pat, key.as_ptr(), value.as_ptr() as *const u8) != 0,
                "failed to add string property {:?} -> {:?}",
                key,
                value
            );
            Ok(())
        }
    }

    #[allow(dead_code)]
    /// Adds a floating-point property to this pattern.
    pub fn add_double(&mut self, key: &str, value: f64) -> Result<(), Error> {
        let key = CString::new(key)?;
        // SAFETY: the pattern is live and `key` is NUL-terminated for the synchronous call;
        // Fontconfig copies the scalar property value.
        unsafe {
            ensure!(
                FcPatternAddDouble(self.pat, key.as_ptr(), value) != 0,
                "failed to set double property {:?} -> {}",
                key,
                value
            );
            Ok(())
        }
    }

    /// Adds an integer property to this pattern.
    pub fn add_integer(&mut self, key: &str, value: i32) -> Result<(), Error> {
        let key = CString::new(key)?;
        // SAFETY: the pattern is live and `key` is NUL-terminated for the synchronous call;
        // Fontconfig copies the scalar property value.
        unsafe {
            ensure!(
                FcPatternAddInteger(self.pat, key.as_ptr(), value) != 0,
                "failed to set integer property {:?} -> {}",
                key,
                value
            );
            Ok(())
        }
    }

    /// Adds a preferred family name to this pattern.
    pub fn family(&mut self, family: &str) -> Result<(), Error> {
        self.add_string("family", family)
    }

    /// Constrains this pattern to Fontconfig's monospaced spacing class.
    pub fn monospace(&mut self) -> Result<(), Error> {
        self.add_integer("spacing", FC_MONO)
    }

    /// Constrains this pattern to Fontconfig's dual-width spacing class.
    pub fn dual(&mut self) -> Result<(), Error> {
        self.add_integer("spacing", FC_DUAL)
    }

    /// Deletes every value for a named property and reports whether one existed.
    pub fn delete_property(&mut self, key: &str) -> Result<bool, Error> {
        let key = CString::new(key)?;
        // SAFETY: the pattern is live and `key` is a readable NUL-terminated name for this call.
        unsafe { Ok(FcPatternDel(self.pat, key.as_ptr()) != 0) }
    }

    /// Formats this pattern with Fontconfig's pattern-format expression language.
    pub fn format(&self, fmt: &str) -> Result<String, Error> {
        let fmt = CString::new(fmt)?;
        // SAFETY: the pattern is live and `fmt` is NUL-terminated; on success Fontconfig returns
        // owned NUL-terminated storage that is read before its paired `FcStrFree`.
        unsafe {
            let s = FcPatternFormat(self.pat, fmt.as_ptr() as *const u8);
            ensure!(!s.is_null(), "failed to format pattern");

            let res = CStr::from_ptr(s as *const c_char).to_string_lossy().into_owned();
            FcStrFree(s);
            Ok(res)
        }
    }

    /// Combines this request pattern with a matched font for rendering.
    pub fn render_prepare(&self, pat: &Pattern) -> Result<Pattern, Error> {
        // SAFETY: both pattern pointers are live; Fontconfig returns a newly owned pattern or null,
        // and a null config selects the current global configuration without transferring ownership.
        unsafe {
            let pat = FcFontRenderPrepare(ptr::null_mut(), self.pat, pat.pat);
            ensure!(!pat.is_null(), "failed to prepare pattern");
            Ok(Pattern { pat })
        }
    }

    /// Applies current Fontconfig substitutions for the requested match kind.
    pub fn config_substitute(&mut self, match_kind: MatchKind) -> Result<(), Error> {
        // SAFETY: `self.pat` is live, null selects the current config, and `MatchKind` has the
        // `FcMatchKind` discriminant representation expected by this call.
        unsafe {
            ensure!(
                FcConfigSubstitute(ptr::null_mut(), self.pat, mem::transmute(match_kind)) != 0,
                "FcConfigSubstitute failed"
            );
            Ok(())
        }
    }

    /// Fills unspecified pattern fields with Fontconfig defaults.
    pub fn default_substitute(&mut self) {
        // SAFETY: `self.pat` is a live mutable pattern pointer.
        unsafe {
            FcDefaultSubstitute(self.pat);
        }
    }

    /// Lists fonts matching this pattern with the properties used by discovery.
    pub fn list(&self) -> anyhow::Result<FontSet> {
        // SAFETY: `self.pat` is live; the object-set names are NUL-terminated, `FcFontList` returns
        // an owned set or null, and `oset` is destroyed exactly once after that synchronous call.
        unsafe {
            // This defines the fields that are retrieved
            let oset = FcObjectSetCreate();
            ensure!(!oset.is_null(), "FcObjectSetCreate failed");
            FcObjectSetAdd(oset, b"family\0".as_ptr() as *const c_char);
            FcObjectSetAdd(oset, b"file\0".as_ptr() as *const c_char);
            FcObjectSetAdd(oset, b"index\0".as_ptr() as *const c_char);
            FcObjectSetAdd(oset, b"spacing\0".as_ptr() as *const c_char);
            FcObjectSetAdd(oset, b"charset\0".as_ptr() as *const c_char);

            let fonts = FcFontList(ptr::null_mut(), self.pat, oset);
            let result = if !fonts.is_null() {
                Ok(FontSet { fonts })
            } else {
                // When: `fonts.is_null()` is true, Fontconfig produced no owned result set.
                Err(anyhow!("FcFontList failed"))
            };
            FcObjectSetDestroy(oset);
            result
        }
    }

    /// Returns Fontconfig's best matching owned pattern for this request.
    pub fn get_best_match(&self) -> Result<Self, Error> {
        // SAFETY: `self.pat` is live, null selects the current config, and `res.0` is writable
        // return-status storage; a successful result returns a newly owned pattern pointer.
        unsafe {
            let mut res = FcResultWrap(0);
            let best = FcFontMatch(ptr::null_mut(), self.pat, &mut res.0 as *mut _);

            if !res.succeeded() {
                Err(res.as_err())
            } else {
                // When: `res.succeeded()` is true, `best` is the owned matched pattern.
                Ok(Pattern { pat: best })
            }
        }
    }

    /// Sorts matching fonts by preference and optionally trims coverage duplicates.
    pub fn sort(&self, trim: bool) -> Result<FontSet, Error> {
        // SAFETY: `self.pat` is live, null selects the current config and discards coverage output,
        // `res.0` is writable status storage, and Fontconfig returns an owned set on success.
        unsafe {
            let mut res = FcResultWrap(0);
            let fonts = FcFontSort(
                ptr::null_mut(),
                self.pat,
                if trim { 1 } else { 0 },
                ptr::null_mut(),
                &mut res.0 as *mut _,
            );

            res.result(FontSet { fonts })
        }
    }

    /// Returns the first file-path property from this pattern.
    pub fn get_file(&self) -> Result<String, Error> {
        self.get_string("file")
    }

    #[allow(dead_code)]
    /// Returns the first floating-point value for a named property.
    pub fn get_double(&self, key: &str) -> Result<f64, Error> {
        // SAFETY: the pattern and NUL-terminated key are live for the call; `fval` is writable
        // output and is read only when Fontconfig reports `FcResultMatch`.
        unsafe {
            let key = CString::new(key)?;
            let mut fval: f64 = 0.0;
            let res =
                FcResultWrap(FcPatternGetDouble(self.pat, key.as_ptr(), 0, &mut fval as *mut _));
            if !res.succeeded() {
                Err(res.as_err())
            } else {
                // When: `res.succeeded()` is true, Fontconfig initialized `fval`.
                Ok(fval)
            }
        }
    }

    /// Returns the first integer value for a named property.
    pub fn get_integer(&self, key: &str) -> Result<c_int, Error> {
        // SAFETY: the pattern and NUL-terminated key are live for the call; `ival` is writable
        // output and is read only when Fontconfig reports `FcResultMatch`.
        unsafe {
            let key = CString::new(key)?;
            let mut ival: c_int = 0;
            let res =
                FcResultWrap(FcPatternGetInteger(self.pat, key.as_ptr(), 0, &mut ival as *mut _));
            if !res.succeeded() {
                Err(res.as_err())
            } else {
                // When: `res.succeeded()` is true, Fontconfig initialized `ival`.
                Ok(ival)
            }
        }
    }

    /// Returns a copied first string value for a named property.
    pub fn get_string(&self, key: &str) -> Result<String, Error> {
        // SAFETY: the pattern and NUL-terminated key are live; `ptr` is writable output and, on
        // `FcResultMatch`, points to pattern-owned NUL-terminated bytes copied before return.
        unsafe {
            let key = CString::new(key)?;
            let mut ptr: *mut u8 = ptr::null_mut();
            let res = FcResultWrap(FcPatternGetString(
                self.pat,
                key.as_ptr(),
                0,
                &mut ptr as *mut *mut u8,
            ));
            if !res.succeeded() {
                Err(res.as_err())
            } else {
                // When: `res.succeeded()` is true, `ptr` names an initialized Fontconfig string.
                Ok(CStr::from_ptr(ptr as *const c_char).to_string_lossy().into_owned())
            }
        }
    }
}

// Lifecycle: `Pattern` releases its owned `FcPattern` with `FcPatternDestroy` once.
impl Drop for Pattern {
    fn drop(&mut self) {
        // SAFETY: `self.pat` is the live pattern pointer owned by this wrapper.
        unsafe {
            FcPatternDestroy(self.pat);
        }
    }
}

impl fmt::Debug for Pattern {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        // unsafe{FcPatternPrint(self.pat);}
        fmt.write_str(
            &self
                .format("Pattern(%{+family,style,weight,width,slant,spacing,file,index,charset,fontformat{%{=unparse}}})")
                .unwrap(),
        )
    }
}

/// Maps a configured font weight to the nearest lower Fontconfig weight class.
pub fn to_fc_weight(weight: FontWeight) -> c_int {
    if weight >= FontWeight::EXTRABLACK {
        FC_WEIGHT_EXTRABLACK
    } else if weight >= FontWeight::BLACK {
        // When: `weight >= EXTRABLACK` is false but `weight >= BLACK` is true.
        FC_WEIGHT_BLACK
    } else if weight >= FontWeight::EXTRABOLD {
        // When: `weight >= BLACK` is false but `weight >= EXTRABOLD` is true.
        FC_WEIGHT_EXTRABOLD
    } else if weight >= FontWeight::BOLD {
        // When: `weight >= EXTRABOLD` is false but `weight >= BOLD` is true.
        FC_WEIGHT_BOLD
    } else if weight >= FontWeight::DEMIBOLD {
        // When: `weight >= BOLD` is false but `weight >= DEMIBOLD` is true.
        FC_WEIGHT_DEMIBOLD
    } else if weight >= FontWeight::MEDIUM {
        // When: `weight >= DEMIBOLD` is false but `weight >= MEDIUM` is true.
        FC_WEIGHT_MEDIUM
    } else if weight >= FontWeight::REGULAR {
        // When: `weight >= MEDIUM` is false but `weight >= REGULAR` is true.
        FC_WEIGHT_REGULAR
    } else if weight >= FontWeight::BOOK {
        // When: `weight >= REGULAR` is false but `weight >= BOOK` is true.
        FC_WEIGHT_BOOK
    } else if weight >= FontWeight::LIGHT {
        // When: `weight >= BOOK` is false but `weight >= LIGHT` is true.
        FC_WEIGHT_LIGHT
    } else if weight >= FontWeight::EXTRALIGHT {
        // When: `weight >= LIGHT` is false but `weight >= EXTRALIGHT` is true.
        FC_WEIGHT_EXTRALIGHT
    } else {
        // When: `weight >= EXTRALIGHT` is false, use Fontconfig's thinnest class.
        FC_WEIGHT_THIN
    }
}

/// Maps a configured font stretch to its exact Fontconfig width class.
pub fn to_fc_width(stretch: FontStretch) -> c_int {
    match stretch {
        FontStretch::UltraCondensed => FC_WIDTH_ULTRACONDENSED,
        FontStretch::ExtraCondensed => FC_WIDTH_EXTRACONDENSED,
        FontStretch::Condensed => FC_WIDTH_CONDENSED,
        FontStretch::SemiCondensed => FC_WIDTH_SEMICONDENSED,
        FontStretch::Normal => FC_WIDTH_NORMAL,
        FontStretch::SemiExpanded => FC_WIDTH_SEMIEXPANDED,
        FontStretch::Expanded => FC_WIDTH_EXPANDED,
        FontStretch::ExtraExpanded => FC_WIDTH_EXTRAEXPANDED,
        FontStretch::UltraExpanded => FC_WIDTH_ULTRAEXPANDED,
    }
}
