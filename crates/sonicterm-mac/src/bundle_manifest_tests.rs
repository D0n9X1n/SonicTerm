use super::*;
use roxmltree::{Document, Node};

fn info_plist_xml_from(script: &str) -> String {
    let normalized = script.replace("\r\n", "\n");
    let marker = "cat > \"$APP/Contents/Info.plist\" <<PLIST\n";
    let start = normalized.find(marker).expect("Info.plist heredoc") + marker.len();
    let remainder = &normalized[start..];
    let end = remainder.find("\nPLIST\n").expect("Info.plist heredoc terminator");
    remainder[..end].replace("${VERSION}", "1.2.1")
}

fn info_plist_xml() -> String {
    info_plist_xml_from(MACOS_DMG_SCRIPT)
}

fn element_children<'a, 'input>(node: Node<'a, 'input>) -> Vec<Node<'a, 'input>> {
    node.children().filter(Node::is_element).collect()
}

fn dict_value<'a, 'input>(dict: Node<'a, 'input>, key: &str) -> Node<'a, 'input> {
    let children = element_children(dict);
    let index = children
        .iter()
        .position(|child| child.tag_name().name() == "key" && child.text() == Some(key))
        .unwrap_or_else(|| panic!("missing key {key}"));
    children[index + 1]
}

#[test]
fn info_plist_extraction_accepts_windows_line_endings() {
    let crlf = MACOS_DMG_SCRIPT.replace("\r\n", "\n").replace('\n', "\r\n");
    assert_eq!(info_plist_xml_from(&crlf), info_plist_xml());
}

#[test]
fn native_registration_and_runtime_share_one_font_directory() {
    // Native font registration must resolve the same four faces as the runtime asset loader.
    let xml = info_plist_xml();
    let parseable =
        xml.lines().filter(|line| !line.starts_with("<!DOCTYPE")).collect::<Vec<_>>().join("\n");
    let document = Document::parse(&parseable).unwrap();
    let plist = document.descendants().find(|node| node.has_tag_name("plist")).unwrap();
    let root_dict = element_children(plist)[0];
    assert_eq!(dict_value(root_dict, "ATSApplicationFontsPath").text(), Some("assets/fonts"));
    assert!(!MACOS_DMG_SCRIPT.contains("Resources/Fonts"));
}

/// The parsed root dictionary of the generated Info.plist.
fn with_root_dict<T>(check: impl FnOnce(Node<'_, '_>) -> T) -> T {
    let xml = info_plist_xml();
    let dtd = r#"<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">"#;
    assert_eq!(xml.lines().filter(|line| *line == dtd).count(), 1);
    let parseable_xml = xml.lines().filter(|line| *line != dtd).collect::<Vec<_>>().join("\n");
    let document = Document::parse(&parseable_xml).expect("generated Info.plist must be valid XML");
    let plist = document.descendants().find(|node| node.has_tag_name("plist")).unwrap();
    check(element_children(plist)[0])
}

/// The text of every element child of `array`.
fn strings(array: Node<'_, '_>) -> Vec<String> {
    assert!(array.has_tag_name("array"));
    element_children(array).into_iter().map(|node| node.text().unwrap().to_string()).collect()
}

/// The app claims scripts and Unix executables only in the Shell role at Alternate rank, so
/// installing it never takes over an existing default.
#[test]
fn packaged_app_declares_alternate_shell_document_types() {
    with_root_dict(|root_dict| {
        let declarations = element_children(dict_value(root_dict, "CFBundleDocumentTypes"));
        assert_eq!(declarations.len(), 2, "scripts and Unix executables only");
        let expected: [(&str, &[&str]); 2] = [
            ("Shell Script", &["public.shell-script", "com.apple.terminal.shell-script"]),
            ("Unix Executable", &["public.unix-executable"]),
        ];
        for (declaration, (name, content_types)) in declarations.into_iter().zip(expected) {
            assert_eq!(dict_value(declaration, "CFBundleTypeName").text(), Some(name));
            assert_eq!(dict_value(declaration, "CFBundleTypeRole").text(), Some("Shell"));
            assert_eq!(dict_value(declaration, "LSHandlerRank").text(), Some("Alternate"));
            assert_eq!(strings(dict_value(declaration, "LSItemContentTypes")), content_types);
        }
    });
}

/// The app declares exactly the ssh and x-man-page schemes it can serve; telnet is not claimed.
#[test]
fn packaged_app_declares_only_the_served_url_schemes() {
    with_root_dict(|root_dict| {
        let url_types = element_children(dict_value(root_dict, "CFBundleURLTypes"));
        assert_eq!(url_types.len(), 1);
        assert_eq!(
            dict_value(url_types[0], "CFBundleURLName").text(),
            Some("com.d0n9x1n.sonicterm.terminal-links")
        );
        assert_eq!(strings(dict_value(url_types[0], "CFBundleURLSchemes")), ["ssh", "x-man-page"]);
    });
    assert!(!MACOS_DMG_SCRIPT.to_ascii_lowercase().contains("telnet"));
}
