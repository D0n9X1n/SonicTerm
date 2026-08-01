use super::*;
use roxmltree::{Document, Node};

fn elements<'a, 'input>(document: &'a Document<'input>, name: &str) -> Vec<Node<'a, 'input>> {
    document.descendants().filter(|node| node.has_tag_name(name)).collect()
}

fn manifest() -> Document<'static> {
    Document::parse(WINDOWS_WIX_MANIFEST).expect("WiX manifest must be valid XML")
}

#[test]
fn wix_registers_every_script_type_without_claiming_a_default() {
    let document = manifest();
    let registry_values = elements(&document, "RegistryValue");
    for (extension, prog_id) in [
        (".ps1", "SonicTerm.ps1"),
        (".cmd", "SonicTerm.cmd"),
        (".bat", "SonicTerm.bat"),
        (".sh", "SonicTerm.sh"),
    ] {
        let prog_key = format!(r"Software\Classes\{prog_id}");
        assert!(
            registry_values.iter().any(|node| {
                node.attribute("Root") == Some("HKLM")
                    && node.attribute("Key") == Some(prog_key.as_str())
                    && node.attribute("Name").is_none()
                    && node.attribute("Value").is_some_and(|value| !value.is_empty())
            }),
            "missing nonempty ProgID description for {prog_id}"
        );
        assert!(
            registry_values.iter().any(|node| {
                node.attribute("Key") == Some(format!(r"{prog_key}\shell\open\command").as_str())
                    && node.attribute("Value")
                        == Some(r#""[APPLICATIONFOLDER]sonicterm-windows.exe" --open-script "%1""#)
            }),
            "missing safe open command for {prog_id}"
        );
        assert!(
            registry_values.iter().any(|node| {
                node.attribute("Key")
                    == Some(format!(r"Software\Classes\{extension}\OpenWithProgids").as_str())
                    && node.attribute("Name") == Some(prog_id)
            }),
            "missing OpenWithProgids candidacy for {extension}"
        );
        assert!(
            !registry_values.iter().any(|node| {
                node.attribute("Key") == Some(format!(r"Software\Classes\{extension}").as_str())
                    && node.attribute("Name").is_none()
            }),
            "installer must not set the {extension} default"
        );
    }
    assert!(!WINDOWS_WIX_MANIFEST.contains("UserChoice"));
}

#[test]
fn wix_registers_complete_default_apps_capabilities() {
    let document = manifest();
    let registry_values = elements(&document, "RegistryValue");
    let capabilities = r"Software\SonicTerm\SonicTerm\Capabilities";
    for name in ["ApplicationDescription", "ApplicationName", "ApplicationIcon"] {
        assert!(
            registry_values.iter().any(|node| {
                node.attribute("Key") == Some(capabilities)
                    && node.attribute("Name") == Some(name)
                    && node.attribute("Value").is_some_and(|value| !value.is_empty())
            }),
            "missing required capability {name}"
        );
    }
    for (extension, prog_id) in [
        (".ps1", "SonicTerm.ps1"),
        (".cmd", "SonicTerm.cmd"),
        (".bat", "SonicTerm.bat"),
        (".sh", "SonicTerm.sh"),
    ] {
        assert!(registry_values.iter().any(|node| {
            node.attribute("Key")
                == Some(r"Software\SonicTerm\SonicTerm\Capabilities\FileAssociations")
                && node.attribute("Name") == Some(extension)
                && node.attribute("Value") == Some(prog_id)
        }));
    }
    assert!(registry_values.iter().any(|node| {
        node.attribute("Key") == Some(r"Software\RegisteredApplications")
            && node.attribute("Name") == Some("SonicTerm")
            && node.attribute("Value") == Some(r"Software\SonicTerm\SonicTerm\Capabilities")
    }));
}

#[test]
fn wix_removes_only_owned_keys_and_refreshes_after_both_mutations() {
    let document = manifest();
    let removed_keys = elements(&document, "RemoveRegistryKey");
    assert!(
        removed_keys.iter().all(|node| {
            !node.attribute("Key").is_some_and(|key| {
                [".ps1", ".cmd", ".bat", ".sh"]
                    .iter()
                    .any(|extension| key == format!(r"Software\Classes\{extension}"))
            })
        }),
        "must not remove shared extension keys"
    );

    for (action, execute) in [
        ("RollbackRefreshShellAssociationsInstall", "rollback"),
        ("RefreshShellAssociationsInstall", "deferred"),
        ("RollbackRefreshShellAssociationsUninstall", "rollback"),
        ("RefreshShellAssociationsUninstall", "deferred"),
    ] {
        let custom = elements(&document, "CustomAction")
            .into_iter()
            .find(|node| node.attribute("Id") == Some(action))
            .unwrap_or_else(|| panic!("missing custom action {action}"));
        assert_eq!(custom.attribute("FileKey"), Some("exe0"));
        assert_eq!(custom.attribute("ExeCommand"), Some("--refresh-shell-associations"));
        assert_eq!(custom.attribute("Execute"), Some(execute));
        assert_eq!(custom.attribute("Impersonate"), Some("yes"));
        assert_eq!(custom.attribute("Return"), Some("ignore"));
    }

    let sequence = elements(&document, "InstallExecuteSequence")[0];
    let scheduled: Vec<_> = sequence.children().filter(Node::is_element).collect();
    let install = scheduled
        .iter()
        .find(|node| node.attribute("Action") == Some("RefreshShellAssociationsInstall"))
        .expect("install refresh sequence");
    assert_eq!(install.attribute("After"), Some("RollbackRefreshShellAssociationsInstall"));
    assert_eq!(install.text().map(str::trim), Some("NOT Installed"));
    let install_rollback = scheduled
        .iter()
        .find(|node| node.attribute("Action") == Some("RollbackRefreshShellAssociationsInstall"))
        .expect("install rollback refresh sequence");
    assert_eq!(install_rollback.attribute("After"), Some("WriteRegistryValues"));
    let uninstall = scheduled
        .iter()
        .find(|node| node.attribute("Action") == Some("RefreshShellAssociationsUninstall"))
        .expect("uninstall refresh sequence");
    assert_eq!(uninstall.attribute("After"), Some("RollbackRefreshShellAssociationsUninstall"));
    assert_eq!(uninstall.text().map(str::trim), Some("REMOVE=\"ALL\""));
    let uninstall_rollback = scheduled
        .iter()
        .find(|node| node.attribute("Action") == Some("RollbackRefreshShellAssociationsUninstall"))
        .expect("uninstall rollback refresh sequence");
    assert_eq!(uninstall_rollback.attribute("After"), Some("RemoveRegistryValues"));
}

#[test]
fn every_registration_component_is_reachable_from_the_feature() {
    let document = manifest();
    let refs: Vec<_> = elements(&document, "ComponentRef")
        .into_iter()
        .filter_map(|node| node.attribute("Id"))
        .collect();
    for component in ["script_progids", "script_capabilities", "script_open_with"] {
        assert!(refs.contains(&component), "feature does not include {component}");
    }
}
