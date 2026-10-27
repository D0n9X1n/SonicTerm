//! FreeType library creation and owned file or memory stream callbacks.

use super::*;

impl Library {
    /// Initializes a FreeType library and applies SonicTerm's configured properties.
    pub fn new() -> anyhow::Result<Library> {
        let mut lib = ptr::null_mut();
        let res =
            // SAFETY: `lib` is writable output storage initialized by FreeType on success.
            unsafe { FT_Init_FreeType(&mut lib as *mut _) };
        let lib = ft_result(res, lib).context("FT_Init_FreeType")?;
        let mut library = Library { inner: std::rc::Rc::new(LibraryInner { lib }) };

        let config = configuration();
        if let Some(vers) = config.freetype_interpreter_version {
            // When: `freetype_interpreter_version` is `Some`, override FreeType's default.
            let interpreter_version: FT_UInt = vers;
            // SAFETY: `library.inner.lib` is live, both byte literals are NUL-terminated, and the
            // property-value pointer remains readable for this synchronous call.
            unsafe {
                FT_Property_Set(
                    library.inner.lib,
                    b"truetype\0" as *const u8 as *const FT_String,
                    b"interpreter-version\0" as *const u8 as *const FT_String,
                    &interpreter_version as *const FT_UInt as *const _,
                );
            }
        }

        {
            let no_long_names: FT_Bool = if config.freetype_pcf_long_family_names { 0 } else { 1 };
            // SAFETY: `library.inner.lib` is live, both byte literals are NUL-terminated, and the
            // property-value pointer remains readable for this synchronous call.
            unsafe {
                FT_Property_Set(
                    library.inner.lib,
                    b"pcf\0" as *const u8 as *const FT_String,
                    b"no-long-family-names\0" as *const u8 as *const FT_String,
                    &no_long_names as *const FT_Bool as *const _,
                );
            }
        }

        // Due to patent concerns, the freetype library disables the LCD
        // filtering feature by default, and since we always build our
        // own copy of freetype, it is likewise disabled by default for
        // us too.  As a result, this call will generally fail.
        // Freetype is still able to render a decent result without it!
        library.set_lcd_filter(FT_LcdFilter::FT_LCD_FILTER_DEFAULT).ok();

        Ok(library)
    }

    /// Returns the number of faces in a given font.
    /// For a TTF this will be 1.
    /// For a TTC, it will be the number of contained fonts
    pub fn query_num_faces(&self, source: &FontDataSource) -> anyhow::Result<u32> {
        let face = self.new_face(source, -1).context("query_num_faces")?;
        let num_faces =
            // SAFETY: `face` points to the live initialized record returned by `new_face`.
            unsafe { (*face).num_faces }.try_into();

        // SAFETY: this temporary face is uniquely owned here and has not been released.
        unsafe {
            FT_Done_Face(face);
        }

        Ok(num_faces?)
    }

    /// Opens a managed FreeType face for a locator handle and named variation.
    pub fn face_from_locator(&self, handle: &FontDataHandle) -> anyhow::Result<Face> {
        let source = handle.clone();

        let mut index = handle.index;
        if handle.variation != 0 {
            index |= handle.variation << 16;
        }

        let face = self
            .new_face(&source.source, index as _)
            .with_context(|| format!("face_from_locator({:?})", handle))?;

        Ok(Face {
            face,
            source,
            size: None,
            library: std::rc::Rc::clone(&self.inner),
            palette: None,
        })
    }

    fn new_face(&self, source: &FontDataSource, face_index: FT_Long) -> anyhow::Result<FT_Face> {
        let mut face = ptr::null_mut();

        // A face that lists an `SVG ` or `COLR` table opens on the file's shared bytes, so
        // FreeType's whole-table extraction points into them instead of copying per face. A
        // negative `face_index` only counts faces and loads no colour table.
        let source = match u32::try_from(face_index) {
            Ok(index) => crate::font_bytes::pin_colour_face(source, index),
            Err(_) => std::borrow::Cow::Borrowed(source),
        };

        // FT_Open_Face will take ownership of this and closes it in both
        // the error case and the success case (although the latter is when
        // the face is dropped).
        let stream = FreeTypeStream::from_source(&source)?;

        let args = FT_Open_Args {
            flags: FT_OPEN_STREAM,
            memory_base: ptr::null(),
            memory_size: 0,
            pathname: ptr::null_mut(),
            stream,
            driver: ptr::null_mut(),
            num_params: 0,
            params: ptr::null_mut(),
        };

        let res =
            // SAFETY: `self.inner.lib`, `args`, and its stream are live; `face` is writable output,
            // and FreeType assumes stream ownership before this call returns.
            unsafe { FT_Open_Face(self.inner.lib, &args, face_index, &mut face as *mut _) };

        ft_result(res, face)
            .with_context(|| format!("FT_Open_Face(\"{:?}\", face_index={})", source, face_index))
    }

    /// Selects the LCD filtering mode for this FreeType library.
    pub fn set_lcd_filter(&mut self, filter: FT_LcdFilter) -> anyhow::Result<()> {
        // SAFETY: `self.inner.lib` is live and `filter` is a FreeType enum value.
        unsafe {
            ft_result(FT_Library_SetLcdFilter(self.inner.lib, filter), ())
                .context("FT_Library_SetLcdFilter")
        }
    }
}

/// Our own stream implementation.
/// This is present because we cannot guarantee to be able to convert
/// Path -> c-string on Windows systems, but also because we've seen
/// mysterious errors about not being able to open a resource.
/// The intent is to avoid a potential problem and to help reveal
/// more context on problems opening files as/when that happens.
struct FreeTypeStream {
    stream: FT_StreamRec_,
    backing: StreamBacking,
    name: String,
}

#[allow(dead_code)]
enum StreamBacking {
    File(BufReader<File>),
    Static(&'static [u8]),
    Memory(Arc<Box<[u8]>>),
}

impl FreeTypeStream {
    pub fn from_source(source: &FontDataSource) -> anyhow::Result<FT_Stream> {
        let (backing, base, len) = match source {
            FontDataSource::OnDisk(path) => {
                // When: `source` is on disk, construct the stream from that file path.
                return Self::open_path(path);
            }
            FontDataSource::BuiltIn { data, .. } => {
                let base = data.as_ptr();
                let len = data.len();
                (StreamBacking::Static(data), base, len)
            }
            FontDataSource::Memory { data, .. } => {
                let base = data.as_ptr();
                let len = data.len();
                (StreamBacking::Memory(Arc::clone(data)), base, len)
            }
        };

        let name = source.name_or_path_str().to_string();

        if len > c_ulong::MAX as usize {
            anyhow::bail!("{} is too large to pass to freetype! (len={})", name, len);
        }

        let stream = Box::new(Self {
            stream: FT_StreamRec_ {
                base: base as *mut _,
                size: len as c_ulong,
                pos: 0,
                descriptor: FT_StreamDesc_ { pointer: ptr::null_mut() },
                pathname: FT_StreamDesc_ { pointer: ptr::null_mut() },
                read: None,
                close: Some(Self::close),
                memory: ptr::null_mut(),
                cursor: ptr::null_mut(),
                limit: ptr::null_mut(),
            },
            backing,
            name,
        });
        let stream = Box::into_raw(stream);
        // SAFETY: `stream` is a stable live box pointer stored for `close` to reclaim.
        unsafe {
            (*stream).stream.descriptor.pointer = stream as *mut _;
            Ok(&mut (*stream).stream)
        }
    }

    fn open_path(path: &Path) -> anyhow::Result<FT_Stream> {
        let file = File::open(path).with_context(|| format!("opening file {}", path.display()))?;

        let meta =
            file.metadata().with_context(|| format!("querying metadata for {}", path.display()))?;

        if !meta.is_file() {
            anyhow::bail!("{} is not a file", path.display());
        }

        let len = meta.len();
        if len as usize > c_ulong::MAX as usize {
            anyhow::bail!("{} is too large to pass to freetype! (len={})", path.display(), len);
        }

        let stream = Box::new(Self {
            stream: FT_StreamRec_ {
                base: ptr::null_mut(),
                size: len as c_ulong,
                pos: 0,
                descriptor: FT_StreamDesc_ { pointer: ptr::null_mut() },
                pathname: FT_StreamDesc_ { pointer: ptr::null_mut() },
                read: Some(Self::read),
                close: Some(Self::close),
                memory: ptr::null_mut(),
                cursor: ptr::null_mut(),
                limit: ptr::null_mut(),
            },
            backing: StreamBacking::File(BufReader::new(file)),
            name: path.to_string_lossy().to_string(),
        });
        let stream = Box::into_raw(stream);
        // SAFETY: `stream` is a stable live box pointer stored for `close` to reclaim.
        unsafe {
            (*stream).stream.descriptor.pointer = stream as *mut _;
            Ok(&mut (*stream).stream)
        }
    }

    /// Called by freetype when it wants to read data from the file
    // SAFETY: FreeType passes the live stream created here; for nonzero `count`, `buffer` names
    // `count` writable bytes, and no concurrent callback aliases the boxed stream state.
    unsafe extern "C" fn read(
        stream: FT_Stream,
        offset: c_ulong,
        buffer: *mut c_uchar,
        count: c_ulong,
    ) -> c_ulong {
        if count == 0 {
            // When: `count == 0`, FreeType requests a seek-only result without copying bytes.
            return 0;
        }

        let myself =
            // SAFETY: `stream` is live and its descriptor stores the stable owning box pointer.
            unsafe { &mut *((*stream).descriptor.pointer as *mut Self) };
        match &mut myself.backing {
            StreamBacking::Static(_) | StreamBacking::Memory(_) => {
                log::error!("read called on memory data {} !?", myself.name);
                0
            }
            StreamBacking::File(file) => {
                // When: the backing is a file, serve FreeType's requested range via seek/read.
                #[cfg(windows)]
                let seek_offset = u64::from(offset);
                #[cfg(not(windows))]
                let seek_offset = offset;
                if let Err(err) = file.seek(SeekFrom::Start(seek_offset)) {
                    // When: `file.seek(...)` returns `Err`, report a zero-byte callback result.
                    log::error!("failed to seek {} to offset {}: {:#}", myself.name, offset, err);
                    return 0;
                }

                let buf =
                    // SAFETY: nonzero `count` guarantees `buffer` names `count` writable bytes.
                    unsafe { std::slice::from_raw_parts_mut(buffer, count as usize) };
                match file.read(buf) {
                    Ok(len) => len as c_ulong,
                    Err(err) => {
                        log::error!(
                            "failed to read {} bytes @ offset {} of {}: {:#}",
                            count,
                            offset,
                            myself.name,
                            err
                        );
                        0
                    }
                }
            }
        }
    }

    /// Called by freetype when the stream is closed
    // SAFETY: FreeType invokes this exactly once for the live stream whose descriptor stores the
    // `Box::into_raw` pointer allocated by `from_source` or `open_path`.
    unsafe extern "C" fn close(stream: FT_Stream) {
        let myself =
            // SAFETY: the descriptor stores the unique unreclaimed box pointer for final drop.
            unsafe { Box::from_raw((*stream).descriptor.pointer as *mut Self) };
        drop(myself);
    }
}

#[cfg(test)]
#[path = "ftwrap_library_tests.rs"]
mod ftwrap_library_tests;
