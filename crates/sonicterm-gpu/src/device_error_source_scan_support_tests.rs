#![cfg(test)]
//! Test-only renderer device-gate reachability and source inventory parsing.

use super::*;

/// `code` with the exact read-only surface operations of `method` blanked, keeping
/// every offset.
pub(super) fn mask_surface_reads(method: &str, code: &str) -> String {
    let mut masked = code.to_owned();
    for (owner, snippet) in SURFACE_READS {
        if *owner == method {
            masked = masked.replace(snippet, &" ".repeat(snippet.len()));
        }
    }
    masked
}

pub(super) fn is_ident_byte(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphanumeric()
}

pub(super) fn find_bytes(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    let start = from.min(haystack.len());
    haystack[start..].windows(needle.len()).position(|window| window == needle).map(|at| start + at)
}

/// Replace every byte in `range` except line breaks with a space.
pub(super) fn blank(bytes: &mut [u8], range: Range<usize>) {
    let end = range.end.min(bytes.len());
    for byte in &mut bytes[range.start.min(end)..end] {
        if *byte != b'\n' {
            *byte = b' ';
        }
    }
}

pub(super) fn block_comment_end(bytes: &[u8], start: usize) -> usize {
    let mut depth = 0usize;
    let mut at = start;
    while at + 1 < bytes.len() {
        if bytes[at] == b'/' && bytes[at + 1] == b'*' {
            depth += 1;
            at += 2;
        } else if bytes[at] == b'*' && bytes[at + 1] == b'/' {
            depth -= 1;
            at += 2;
            if depth == 0 {
                return at;
            }
        } else {
            at += 1;
        }
    }
    bytes.len()
}

/// The index of the quote that closes the string opened at `open`.
pub(super) fn string_end(bytes: &[u8], open: usize) -> usize {
    let mut at = open + 1;
    while at < bytes.len() && bytes[at] != b'"' {
        at += if bytes[at] == b'\\' { 2 } else { 1 };
    }
    at.min(bytes.len())
}

/// The end of a raw string literal that starts at `at`, if one does.
pub(super) fn raw_string_end(bytes: &[u8], at: usize) -> Option<usize> {
    if bytes[at] != b'r' {
        return None;
    }
    let previous = |offset: usize| at.checked_sub(offset).map(|index| bytes[index]);
    let prefixed = match previous(1) {
        Some(b'b') => !previous(2).is_some_and(is_ident_byte),
        Some(byte) => !is_ident_byte(byte),
        None => true,
    };
    if !prefixed {
        return None;
    }
    let mut quote = at + 1;
    while bytes.get(quote) == Some(&b'#') {
        quote += 1;
    }
    if bytes.get(quote) != Some(&b'"') {
        return None;
    }
    let mut terminator = vec![b'"'];
    terminator.resize(quote - at, b'#');
    find_bytes(bytes, &terminator, quote + 1).map(|end| end + terminator.len())
}

/// The end of a character literal that starts at `at`, if one does; a
/// lifetime or a label is not one.
pub(super) fn char_literal_end(bytes: &[u8], at: usize) -> Option<usize> {
    if bytes[at] != b'\'' {
        return None;
    }
    let first = *bytes.get(at + 1)?;
    if first == b'\\' {
        return find_bytes(bytes, b"'", at + 3).map(|end| end + 1);
    }
    let width = match first {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    };
    (bytes.get(at + 1 + width) == Some(&b'\'')).then_some(at + 2 + width)
}

/// Normalize checkout line endings, then blank comments and literals while
/// preserving offsets within that normalized source.
pub(super) fn code_only(source: &str) -> String {
    let normalized = source.replace("\r\n", "\n");
    let mut bytes = normalized.as_bytes().to_vec();
    let mut at = 0;
    while at < bytes.len() {
        let byte = bytes[at];
        let next = bytes.get(at + 1).copied();
        if byte == b'/' && next == Some(b'/') {
            let end = find_bytes(&bytes, b"\n", at).unwrap_or(bytes.len());
            blank(&mut bytes, at..end);
            at = end;
        } else if byte == b'/' && next == Some(b'*') {
            let end = block_comment_end(&bytes, at);
            blank(&mut bytes, at..end);
            at = end;
        } else if let Some(end) = raw_string_end(&bytes, at) {
            blank(&mut bytes, at..end);
            at = end;
        } else if byte == b'"' {
            let end = string_end(&bytes, at);
            blank(&mut bytes, at + 1..end);
            at = end + 1;
        } else if let Some(end) = char_literal_end(&bytes, at) {
            blank(&mut bytes, at + 1..end - 1);
            at = end;
        } else {
            at += 1;
        }
    }
    String::from_utf8(bytes).expect("blanking keeps UTF-8 boundaries")
}

pub(super) fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

/// The indentation of a line that opens an inherent `impl GpuRenderer` block.
pub(super) fn renderer_impl_indent(line: &str) -> Option<usize> {
    let head = line.trim().strip_prefix("impl ")?.strip_suffix(" {")?;
    if head == "GpuRenderer" || head.ends_with("::GpuRenderer") {
        Some(indent_of(line))
    } else {
        None
    }
}

pub(super) fn method_name(line: &str, indent: usize) -> Option<&str> {
    if indent_of(line) != indent {
        return None;
    }
    let (head, after_fn) = line[indent..].split_once("fn ")?;
    let modifiers = head
        .split_whitespace()
        .all(|word| word.starts_with("pub") || matches!(word, "const" | "async" | "unsafe"));
    if !modifiers {
        return None;
    }
    let end = after_fn.find(|c: char| !(c.is_alphanumeric() || c == '_'))?;
    Some(&after_fn[..end])
}

/// Join rustfmt's split method chains, so a receiver and its call read as one
/// expression.
pub(super) fn compact_chains(code: &str) -> String {
    let mut out = String::with_capacity(code.len());
    let mut pending = String::new();
    for character in code.chars() {
        if character.is_whitespace() {
            pending.push(character);
            continue;
        }
        if character != '.' {
            out.push_str(&pending);
        }
        pending.clear();
        out.push(character);
    }
    out.push_str(&pending);
    out
}

/// Every method of every `impl GpuRenderer` block in the blanked `sources`,
/// with cfg-gated duplicates merged, and the number of blocks found.
pub(super) fn renderer_methods(sources: &[String]) -> (usize, Vec<Method>) {
    let mut blocks = 0;
    let mut methods: Vec<Method> = Vec::new();
    for source in sources {
        let lines: Vec<&str> = source.lines().collect();
        let mut index = 0;
        while index < lines.len() {
            let Some(indent) = renderer_impl_indent(lines[index]) else {
                index += 1;
                continue;
            };
            blocks += 1;
            let close = format!("{}}}", " ".repeat(indent));
            let mut current: Option<usize> = None;
            index += 1;
            while index < lines.len() && lines[index] != close {
                let line = lines[index];
                if let Some(name) = method_name(line, indent + 4) {
                    let existing = methods.iter().position(|method| method.name == name);
                    current = Some(existing.unwrap_or_else(|| {
                        methods.push(Method { name: name.to_owned(), code: String::new() });
                        methods.len() - 1
                    }));
                }
                if let Some(current) = current {
                    methods[current].code.push_str(line);
                    methods[current].code.push('\n');
                }
                index += 1;
            }
        }
    }
    for method in &mut methods {
        method.code = compact_chains(&method.code);
    }
    (blocks, methods)
}

/// The index of the bracket that closes the one at `open`.
pub(super) fn matching_close(code: &[u8], open: usize) -> usize {
    let (opening, closing) = match code[open] {
        b'(' => (b'(', b')'),
        b'[' => (b'[', b']'),
        _ => (b'{', b'}'),
    };
    let mut depth = 0usize;
    for (index, &byte) in code.iter().enumerate().skip(open) {
        if byte == opening {
            depth += 1;
        } else if byte == closing {
            depth -= 1;
            if depth == 0 {
                return index;
            }
        }
    }
    code.len()
}

/// The index of the brace that closes the block enclosing `from`.
pub(super) fn enclosing_close(code: &[u8], from: usize) -> usize {
    let mut depth = 0usize;
    for (index, &byte) in code.iter().enumerate().skip(from) {
        if byte == b'{' {
            depth += 1;
        } else if byte == b'}' {
            if depth == 0 {
                return index;
            }
            depth -= 1;
        }
    }
    code.len()
}

pub(super) fn skip_whitespace(code: &[u8], mut at: usize) -> usize {
    while code.get(at).is_some_and(u8::is_ascii_whitespace) {
        at += 1;
    }
    at
}

/// `text` without a trailing whole word `word`.
pub(super) fn strip_word<'a>(text: &'a str, word: &str) -> Option<&'a str> {
    let rest = text.trim_end().strip_suffix(word)?;
    if rest.as_bytes().last().is_some_and(|&byte| is_ident_byte(byte)) {
        return None;
    }
    Some(rest)
}

/// The binding of a gate call, read from the text before the call.
pub(super) fn gate_binding(before: &str) -> Option<GateBinding> {
    let before = before.trim_end().strip_suffix('=')?;
    if before.ends_with(['=', '!', '<', '>']) {
        return None;
    }
    let before = before.trim_end();
    if let Some(pattern) = before.strip_suffix(')') {
        let open = pattern.rfind('(')?;
        let binding = strip_word(&pattern[..open], "Some")?;
        let keyword = strip_word(binding, "let")?;
        return Some(if strip_word(keyword, "if").is_some() {
            GateBinding::IfLet
        } else {
            GateBinding::LetElse
        });
    }
    let name = before.trim_end_matches(|c: char| c == '_' || c.is_ascii_alphanumeric());
    if name.len() == before.len() {
        return None;
    }
    let name = strip_word(name, "mut").unwrap_or(name);
    strip_word(name, "let").map(|_| GateBinding::Propagated)
}

/// Byte ranges of `code` that run only after the device gate admitted work: a
/// `gpu_work` closure, an `if let` body, or the rest of the block after a
/// `let`-`else` or propagated scope. A gate call whose result is discarded or
/// only tested controls nothing.
pub(super) fn gate_ranges(code: &str) -> Vec<Range<usize>> {
    let bytes = code.as_bytes();
    let mut ranges = Vec::new();
    for receiver in GATE_RECEIVERS {
        for call in [".gpu_work(", ".enter_gpu_work("] {
            let pattern = format!("{receiver}{call}");
            for (at, _) in code.match_indices(pattern.as_str()) {
                if at > 0 && (is_ident_byte(bytes[at - 1]) || bytes[at - 1] == b'.') {
                    continue;
                }
                let open = at + pattern.len() - 1;
                let close = matching_close(bytes, open);
                if call == ".gpu_work(" {
                    if let Some(body) = gpu_work_closure_body(code, open, close) {
                        ranges.push(body);
                    }
                    continue;
                }
                let after = skip_whitespace(bytes, close + 1);
                let rest = code.get(after..).unwrap_or_default();
                match gate_binding(&code[..at]) {
                    Some(GateBinding::IfLet) if rest.starts_with('{') => {
                        ranges.push(after..matching_close(bytes, after));
                    }
                    Some(GateBinding::LetElse) if rest.starts_with("else") => {
                        let brace = skip_whitespace(bytes, after + 4);
                        if bytes.get(brace) == Some(&b'{') {
                            let end = matching_close(bytes, brace) + 1;
                            ranges.push(end..enclosing_close(bytes, end));
                        }
                    }
                    Some(GateBinding::Propagated) => {
                        for conversion in [".ok_or_else(", ".ok_or("] {
                            if !rest.starts_with(conversion) {
                                continue;
                            }
                            let converted = matching_close(bytes, after + conversion.len() - 1);
                            let question = skip_whitespace(bytes, converted + 1);
                            if bytes.get(question) == Some(&b'?') {
                                ranges.push(question + 1..enclosing_close(bytes, question + 1));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    ranges
}

/// Recognize the production zero-argument closure after the label argument.
/// Label expressions are evaluated before the gate, so none of that argument
/// may be counted as guarded. Skip only balanced argument delimiters here.
pub(super) fn gpu_work_closure_body(code: &str, open: usize, close: usize) -> Option<Range<usize>> {
    let bytes = code.as_bytes();
    let mut at = open + 1;
    while at < close {
        match bytes[at] {
            b'(' | b'[' | b'{' => at = matching_close(bytes, at) + 1,
            b',' => break,
            _ => at += 1,
        }
    }
    if at >= close {
        return None;
    }
    at = skip_whitespace(bytes, at + 1);
    if code[at..].starts_with("move ") {
        at = skip_whitespace(bytes, at + 4);
    }
    if !code[at..].starts_with("||") {
        return None;
    }
    let body = skip_whitespace(bytes, at + 2);
    let end = if bytes.get(body) == Some(&b'{') { matching_close(bytes, body) + 1 } else { close };
    (body < end && end <= close).then_some(body..end)
}

pub(super) fn is_whole_field(code: &[u8], at: usize, len: usize) -> bool {
    let before = at.checked_sub(1).map(|index| code[index]);
    let after = code.get(at + len).copied();
    !before.is_some_and(|byte| is_ident_byte(byte) || byte == b'.')
        && !after.is_some_and(is_ident_byte)
}

/// Offsets where `code` names one of the wgpu handles in `GPU_HANDLES`, also when the
/// receiver and the field are split by spaces, a line break, or a blanked comment.
pub(super) fn handle_uses(code: &str) -> Vec<usize> {
    let mut uses = Vec::new();
    for handle in GPU_HANDLES {
        let (receiver, field) = handle.split_once('.').expect("receiver.field handle");
        uses.extend(field_uses(code, receiver, field));
    }
    uses
}

/// The end of the ASCII whitespace that starts at `at`.
pub(super) fn skip_space(bytes: &[u8], mut at: usize) -> usize {
    while bytes.get(at).is_some_and(u8::is_ascii_whitespace) {
        at += 1;
    }
    at
}

/// Offsets of `receiver.field` in `code` with optional whitespace around the dot. The
/// receiver must not follow an identifier or a dot, and the field must end a word.
pub(super) fn field_uses(code: &str, receiver: &str, field: &str) -> Vec<usize> {
    let bytes = code.as_bytes();
    let mut found = Vec::new();
    for (at, _) in code.match_indices(receiver) {
        let joined = at
            .checked_sub(1)
            .is_some_and(|before| is_ident_byte(bytes[before]) || bytes[before] == b'.');
        let dot = skip_space(bytes, at + receiver.len());
        if joined || bytes.get(dot) != Some(&b'.') {
            continue;
        }
        let name = skip_space(bytes, dot + 1);
        let end = name + field.len();
        if code.get(name..end) == Some(field) && !bytes.get(end).copied().is_some_and(is_ident_byte)
        {
            found.push(at);
        }
    }
    found
}

/// Offsets of stored-value device reads, such as `self.device.limits()`.
pub(super) fn device_reads(code: &str) -> Vec<usize> {
    let mut reads = Vec::new();
    for read in DEVICE_READS {
        reads.extend(code.match_indices(*read).map(|(at, _)| at));
    }
    reads
}

/// Offsets where `code` calls the renderer method `callee`.
pub(super) fn method_calls(code: &str, callee: &str) -> Vec<usize> {
    let mut calls = Vec::new();
    for form in [format!("self.{callee}("), format!("Self::{callee}(")] {
        calls.extend(code.match_indices(form.as_str()).map(|(at, _)| at));
    }
    calls
}

/// Whether `method` reaches the GPU outside its gate, given the methods
/// already known to.
///
/// A gated method may read stored device values and call the
/// [`NOT_GPU_WORK`] readers anywhere; every other handle use, and every call
/// into a reaching method, must sit in a range its gate controls. A method
/// with no controlling gate reaches the GPU if it names a handle or `wgpu::`,
/// or calls a reaching method.
pub(super) fn reaches_gpu(
    method: &Method,
    ranges: &[Range<usize>],
    reaching: &BTreeSet<String>,
) -> bool {
    let masked = mask_surface_reads(&method.name, &method.code);
    let code = masked.as_str();
    let calls_into = |skip_readers: bool| {
        let mut calls = Vec::new();
        for callee in reaching {
            if skip_readers && NOT_GPU_WORK.contains(&callee.as_str()) {
                continue;
            }
            calls.extend(method_calls(code, callee));
        }
        calls
    };
    if ranges.is_empty() {
        return !handle_uses(code).is_empty()
            || code.contains("wgpu::")
            || !calls_into(false).is_empty();
    }
    let mut exempt: Vec<Range<usize>> = Vec::new();
    for (name, snippet) in UNGATED_BY_DESIGN {
        if method.name == *name {
            exempt.extend(code.match_indices(*snippet).map(|(at, _)| at..at + snippet.len()));
        }
    }
    let reads = device_reads(code);
    let mut work: Vec<usize> =
        handle_uses(code).into_iter().filter(|at| !reads.contains(at)).collect();
    if method.name == CONSTRUCTOR {
        let (local_work, bootstrap) = constructor_handle_uses(code);
        work.extend(local_work);
        exempt.extend(bootstrap);
    }
    work.extend(calls_into(true));
    work.iter().any(|at| {
        !ranges.iter().any(|range| range.contains(at))
            && !exempt.iter().any(|range| range.contains(at))
    })
}

/// The constructor uses local handles before they become `self` fields. Its
/// pre-device exceptions are exact bootstrap calls, not the whole method or
/// even the whole prefix; configuring a surface anywhere still needs a gate.
pub(super) fn constructor_handle_uses(code: &str) -> (Vec<usize>, Vec<Range<usize>>) {
    let bootstrap_end = code.find("let format = ").expect("constructor bootstrap boundary");
    let mut exempt = Vec::new();
    for call in ["instance.create_surface(", "instance.request_adapter("] {
        for (at, _) in code[..bootstrap_end].match_indices(call) {
            // Only the instance receiver is exempt; nested argument work still needs its own gate.
            exempt.push(at..at + "instance".len());
        }
    }
    // The shared negotiation receives the instance and surface before any device exists.
    let negotiation = "recovery_context::negotiate_device(&instance, &surface)";
    for (at, _) in code[..bootstrap_end].match_indices(negotiation) {
        exempt.push(at..at + negotiation.len());
    }
    let handlers = "install_device_error_handlers(&device)";
    for (at, _) in code[..bootstrap_end].match_indices(handlers) {
        exempt.push(at..at + handlers.len());
    }
    for (at, _) in code[..bootstrap_end].match_indices("compatible_surface: Some(&surface)") {
        exempt.push(at..at + "compatible_surface: Some(&surface)".len());
    }
    // These access immutable metadata, not GPU work, and are needed to choose initial configuration.
    for read in ["device.limits()", "surface.get_capabilities(&adapter)"] {
        exempt.extend(code.match_indices(read).map(|(at, _)| at..at + read.len()));
    }
    let mut work = Vec::new();
    for handle in ["device", "queue", "surface", "instance"] {
        for (at, _) in code.match_indices(handle) {
            if is_whole_field(code.as_bytes(), at, handle.len())
                && (code.as_bytes().get(at + handle.len()) == Some(&b'.')
                    || at.checked_sub(1).is_some_and(|before| code.as_bytes()[before] == b'&'))
            {
                work.push(at);
            }
        }
    }
    (work, exempt)
}

/// Renderer methods that reach the GPU outside the device gate, directly or
/// through another such method.
pub(super) fn reaching_methods(methods: &[Method]) -> BTreeSet<String> {
    let gates: Vec<Vec<Range<usize>>> =
        methods.iter().map(|method| gate_ranges(&method.code)).collect();
    let mut reaching: BTreeSet<String> = BTreeSet::new();
    loop {
        let mut added = Vec::new();
        for (method, ranges) in methods.iter().zip(&gates) {
            if !reaching.contains(&method.name) && reaches_gpu(method, ranges, &reaching) {
                added.push(method.name.clone());
            }
        }
        if added.is_empty() {
            return reaching;
        }
        reaching.extend(added);
    }
}

/// Every violation of the device-gate contract in `sources`.
pub(super) fn gate_violations(sources: &[&str]) -> Vec<String> {
    let blanked: Vec<String> = sources.iter().copied().map(code_only).collect();
    let (blocks, methods) = renderer_methods(&blanked);
    if blocks == 0 {
        return vec!["no `impl GpuRenderer` block".to_owned()];
    }
    let mut violations = Vec::new();
    for name in GATED_METHODS.iter().chain(NOT_GPU_WORK) {
        if !methods.iter().any(|method| method.name == *name) {
            violations.push(format!("no renderer method `{name}`"));
        }
    }
    for name in reaching_methods(&methods) {
        if !NOT_GPU_WORK.contains(&name.as_str()) {
            violations.push(format!("`{name}` reaches the GPU outside the device gate"));
        }
    }
    for method in &methods {
        if GATED_METHODS.contains(&method.name.as_str()) && gate_ranges(&method.code).is_empty() {
            violations.push(format!("`{}` has no gate that controls its GPU work", method.name));
        }
    }
    violations
}

pub(super) fn block_after(source: &str, header: &str) -> String {
    let source = source.replace("\r\n", "\n");
    let start = source.find(header).unwrap_or_else(|| panic!("missing `{header}`"));
    let block = &source[start..];
    block[..block.find("\n}\n").unwrap_or_else(|| panic!("unterminated `{header}`"))].to_owned()
}

pub(super) fn without_function(source: &str, signature: &str) -> String {
    let source = source.replace("\r\n", "\n");
    let Some(start) = source.find(signature) else {
        return source;
    };
    let end = source[start..].find("\n}\n").map_or(source.len(), |end| start + end + 3);
    format!("{}{}", &source[..start], &source[end..])
}

/// Every production source file under the crate's `src/`, recursively, with
/// its path relative to `src/`.
pub(super) fn production_sources() -> Vec<(String, String)> {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut pending = vec![src.clone()];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).expect("read a gpu source directory") {
            let path = entry.expect("source entry").path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            let name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
            if !name.ends_with(".rs") || name.ends_with("_tests.rs") {
                continue;
            }
            let source = std::fs::read_to_string(&path).expect("read source file");
            // A file-level test gate excludes only test fixtures, never a renderer phase.
            if source.starts_with("#![cfg(test)]\n") || source.starts_with("#![cfg(test)]\r\n") {
                continue;
            }
            let relative = path.strip_prefix(&src).expect("under src").display().to_string();
            files.push((relative, source));
        }
    }
    files.sort();
    files
}

pub(super) fn banned_calls(name: &str, text: &str) -> Vec<String> {
    let mut offenders = Vec::new();
    for (index, line) in code_only(text).lines().enumerate() {
        for banned in BANNED {
            if line.contains(*banned) {
                offenders.push(format!("{name}:{}: {banned}", index + 1));
            }
        }
    }
    offenders
}

/// `code` with each method chain split across spaces or lines rejoined around its
/// member dots, so `device` and `.create_buffer(` on two lines read as one call.
/// Comments and strings must already be blanked.
pub(super) fn join_chains(code: &str) -> String {
    let chars: Vec<char> = code.chars().collect();
    let mut out = String::with_capacity(code.len());
    let mut at = 0;
    while at < chars.len() {
        if !chars[at].is_whitespace() {
            out.push(chars[at]);
            at += 1;
            continue;
        }
        let mut next = at;
        while next < chars.len() && chars[next].is_whitespace() {
            next += 1;
        }
        let before_dot = chars.get(next) == Some(&'.') && chars.get(next + 1) != Some(&'.');
        let after_dot = out.ends_with('.') && !out.ends_with("..");
        if !before_dot && !after_dot {
            out.push(' ');
        }
        at = next;
    }
    out
}

/// Offsets in a normalized negotiation body that use a device handle outside its
/// exact bootstrap calls: the adapter request and the handler installation. A use is a
/// member call on the handle or a borrow of it.
pub(super) fn negotiation_work(body: &str) -> Vec<usize> {
    let bytes = body.as_bytes();
    let mut exempt = Vec::new();
    for call in ["instance.request_adapter(", "install_device_error_handlers(&device)"] {
        exempt.extend(body.match_indices(call).map(|(at, _)| at..at + call.len()));
    }
    let mut work = Vec::new();
    for handle in ["device", "queue", "surface", "instance"] {
        for (at, _) in body.match_indices(handle) {
            let reaches = is_whole_field(bytes, at, handle.len())
                && (bytes.get(at + handle.len()) == Some(&b'.') || is_borrowed(bytes, at));
            if reaches && !exempt.iter().any(|range| range.contains(&at)) {
                work.push(at);
            }
        }
    }
    work
}

/// Whether the handle at `at` in normalized `code` is borrowed: its previous token is
/// `&`, or a whole `mut` after `&`, with only spaces or blanked comments between them.
pub(super) fn is_borrowed(code: &[u8], at: usize) -> bool {
    let previous = |end: usize| code[..end].iter().rposition(|byte| !byte.is_ascii_whitespace());
    let mut token = previous(at);
    let after_mut = token.filter(|&last| {
        code[..=last].ends_with(b"mut")
            && !last.checked_sub(3).is_some_and(|before| is_ident_byte(code[before]))
    });
    if let Some(last) = after_mut {
        token = previous(last - 2);
    }
    token.is_some_and(|last| code[last] == b'&')
}

/// The comment-free, chain-joined body of `negotiate_device` in `source`.
pub(super) fn negotiation_body(source: &str) -> String {
    join_chains(&block_after(&code_only(source), "pub(super) async fn negotiate_device("))
}

/// Normalize checkout line endings before literal mutation anchors and gate scanning.
pub(super) fn recovery_renderer(sources: [&str; 5]) -> String {
    sources.join("\n").replace("\r\n", "\n")
}

/// The renderer methods reachable from `assemble_frame` through renderer-method calls that create,
/// write or gate device work: a GPU handle used beyond a fixed device read, or a device-gate entry.
pub(super) fn assembly_device_work(sources: &[String]) -> Vec<String> {
    let blanked: Vec<String> =
        sources.iter().map(|text| code_only(&text.replace("\r\n", "\n"))).collect();
    let (_, methods) = renderer_methods(&blanked);
    let mut reached: BTreeSet<String> = BTreeSet::from(["assemble_frame".to_owned()]);
    let mut frontier = vec!["assemble_frame".to_owned()];
    while let Some(name) = frontier.pop() {
        for caller in methods.iter().filter(|method| method.name == name) {
            for callee in &methods {
                if !method_calls(&caller.code, &callee.name).is_empty()
                    && reached.insert(callee.name.clone())
                {
                    frontier.push(callee.name.clone());
                }
            }
        }
    }
    methods
        .iter()
        .filter(|method| reached.contains(&method.name))
        .filter(|method| {
            let reads = device_reads(&method.code);
            handle_uses(&method.code).iter().any(|at| !reads.contains(at))
                || method.code.contains(".enter_gpu_work(")
                || method.code.contains(".gpu_work(")
        })
        .map(|method| method.name.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[cfg(test)]
#[path = "device_error_source_scan_tests.rs"]
mod device_error_source_scan_tests;
