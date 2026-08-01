use super::*;
use roxmltree::{Document, Node};

fn info_plist_xml() -> String {
    let marker = "cat > \"$APP/Contents/Info.plist\" <<PLIST\n";
    let start = MACOS_DMG_SCRIPT.find(marker).expect("Info.plist heredoc") + marker.len();
    let remainder = &MACOS_DMG_SCRIPT[start..];
    let end = remainder.find("\nPLIST\n").expect("Info.plist heredoc terminator");
    remainder[..end].replace("${VERSION}", "1.2.1")
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
fn packaged_app_declares_one_alternate_shell_document_type() {
    let xml = info_plist_xml();
    let dtd = r#"<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">"#;
    assert_eq!(xml.lines().filter(|line| *line == dtd).count(), 1);
    let parseable_xml = xml.lines().filter(|line| *line != dtd).collect::<Vec<_>>().join("\n");
    let document = Document::parse(&parseable_xml).expect("generated Info.plist must be valid XML");
    let plist = document.descendants().find(|node| node.has_tag_name("plist")).unwrap();
    let root_dict = element_children(plist)[0];
    let document_types = dict_value(root_dict, "CFBundleDocumentTypes");
    assert!(document_types.has_tag_name("array"));

    let declarations = element_children(document_types);
    assert_eq!(declarations.len(), 1, "only one script-handler declaration is allowed");
    let declaration = declarations[0];
    assert_eq!(dict_value(declaration, "CFBundleTypeRole").text(), Some("Shell"));
    assert_eq!(dict_value(declaration, "LSHandlerRank").text(), Some("Alternate"));

    let content_types = dict_value(declaration, "LSItemContentTypes");
    let values: Vec<_> =
        element_children(content_types).into_iter().map(|node| node.text().unwrap()).collect();
    assert_eq!(values, ["public.shell-script", "com.apple.terminal.shell-script"]);
}
