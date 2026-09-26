use super::{
    warm_window_pool_may_spawn, warm_window_pool_should_spawn, warm_window_pool_target,
    WARM_WINDOW_POOL_MAX,
};

#[test]
fn zero_disables_warm_pool_on_every_adapter() {
    assert_eq!(warm_window_pool_target(0, false), 0);
    assert_eq!(warm_window_pool_target(0, true), 0);
    assert!(!warm_window_pool_should_spawn(0, 0, false));
    assert!(!warm_window_pool_should_spawn(0, 0, true));
}

#[test]
fn hardware_honors_configured_target_up_to_maximum() {
    assert_eq!(warm_window_pool_target(1, false), 1);
    assert_eq!(warm_window_pool_target(2, false), 2);
    assert_eq!(warm_window_pool_target(99, false), WARM_WINDOW_POOL_MAX);
    assert!(warm_window_pool_should_spawn(0, 2, false));
    assert!(warm_window_pool_should_spawn(1, 2, false));
    assert!(!warm_window_pool_should_spawn(2, 2, false));
}

#[test]
fn software_adapter_caps_nonzero_target_at_one() {
    assert_eq!(warm_window_pool_target(1, true), 1);
    assert_eq!(warm_window_pool_target(5, true), 1);
    assert_eq!(warm_window_pool_target(99, true), 1);
    assert!(warm_window_pool_should_spawn(0, 5, true));
    assert!(!warm_window_pool_should_spawn(1, 5, true));
}

/// A stopped device blocks prewarming whatever the size rule says, so a pass
/// cannot create and drop a hidden window on every wake.
#[test]
fn stopped_device_blocks_prewarming() {
    for (len, configured, software) in [(0, 1, false), (0, 2, false), (1, 2, false), (0, 5, true)] {
        assert!(warm_window_pool_should_spawn(len, configured, software));
        assert!(warm_window_pool_may_spawn(true, len, configured, software));
        assert!(!warm_window_pool_may_spawn(false, len, configured, software));
    }
    assert!(!warm_window_pool_may_spawn(true, 1, 1, false));
}

fn maintenance_uses_device_gate(source: &str) -> bool {
    let source = source.replace("\r\n", "\n");
    let start = source.find("fn warm_window_pool_maintain(").expect("maintenance function");
    let body = &source[start..];
    let body = &body[..body.find("\n    }\n").expect("maintenance body end")];
    body.contains("GpuRenderer::device_accepts_gpu_work")
        && body.contains("warm_window_pool_may_spawn(")
        && !body.contains("warm_window_pool_should_spawn(")
}

/// Maintenance must retain the stopped-device gate under Unix and Windows checkout line endings.
#[test]
fn maintenance_consults_the_device_gate() {
    let lf = include_str!("tear_out.rs").replace("\r\n", "\n");
    for source in [lf.clone(), lf.replace('\n', "\r\n")] {
        assert!(maintenance_uses_device_gate(&source));
    }
}

/// Line-ending handling must not hide a missing device predicate or an ungated spawn rule.
#[test]
fn maintenance_source_rejects_ungated_prewarming() {
    let lf = include_str!("tear_out.rs").replace("\r\n", "\n");
    for source in [lf.clone(), lf.replace('\n', "\r\n")] {
        let without_predicate = source.replace("GpuRenderer::device_accepts_gpu_work", "false");
        assert!(!maintenance_uses_device_gate(&without_predicate));
        let ungated_rule =
            source.replace("warm_window_pool_may_spawn(", "warm_window_pool_should_spawn(");
        assert!(!maintenance_uses_device_gate(&ungated_rule));
    }
}
