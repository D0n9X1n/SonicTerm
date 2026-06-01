use std::collections::HashMap;

use sonicterm_cfg::keymap::Keymap;

#[test]
fn bundled_keymaps_do_not_bind_multiple_actions_to_the_same_chord() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..");

    for keymap_name in ["wezterm", "wezterm-windows"] {
        let keymap_path = root.join(format!("assets/keymaps/{keymap_name}.toml"));
        let keymap =
            Keymap::load(&keymap_path).unwrap_or_else(|err| panic!("load {keymap_name}: {err:#}"));
        let mut seen = HashMap::new();

        for binding in &keymap.bindings {
            let chord = binding.keys.to_ascii_lowercase();
            if let Some(previous) = seen.insert(chord.clone(), &binding.action.0) {
                panic!(
                    "{keymap_name} binds chord {chord:?} to both {previous:?} and {:?}",
                    binding.action.0
                );
            }
        }
    }
}
