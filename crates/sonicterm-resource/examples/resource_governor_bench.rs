use enum_map::enum_map;
use sonicterm_resource::ResourceGovernor;
use sonicterm_types::{
    GovernorLimits, OwnerKind, OwnerLimits, ProcessKind, ResourceAmount, ResourceClass,
};
use std::{sync::Arc, time::Instant};

const ITERATIONS: usize = 200_000;
const THREADS: usize = 8;

fn governor(
) -> (ResourceGovernor, sonicterm_types::ResourceOwnerId, Vec<sonicterm_types::ResourceOwnerId>) {
    let limits = GovernorLimits {
        process_bytes: usize::MAX,
        class_bytes: enum_map! { _ => usize::MAX },
        class_items: enum_map! { _ => None },
    };
    let owner_limits = OwnerLimits {
        owner_bytes: usize::MAX,
        class_bytes: enum_map! { _ => usize::MAX },
        class_items: enum_map! { _ => None },
    };
    let governor = ResourceGovernor::new(ProcessKind::Gui, limits).unwrap();
    let window = governor
        .create_child(governor.root_owner(), OwnerKind::Window, owner_limits.clone())
        .unwrap();
    let panes = (0..THREADS)
        .map(|_| governor.create_child(window, OwnerKind::AppPane, owner_limits.clone()).unwrap())
        .collect::<Vec<_>>();
    (governor, panes[0], panes)
}

fn main() {
    let (governor, owner, owners) = governor();
    let start = Instant::now();
    for _ in 0..ITERATIONS {
        drop(
            governor
                .try_reserve(
                    owner,
                    ResourceClass::PtyOutput,
                    ResourceAmount { bytes: 4096, items: 1 },
                )
                .unwrap(),
        );
    }
    let uncontended_seconds = start.elapsed().as_secs_f64();

    let classes = [
        ResourceClass::GridVisible,
        ResourceClass::GridHistory,
        ResourceClass::Surface,
        ResourceClass::UploadStaging,
        ResourceClass::GlyphRaster,
        ResourceClass::GlyphAtlas,
        ResourceClass::PtyOutput,
        ResourceClass::RemoteOutput,
    ];
    let governor = Arc::new(governor);
    let start = Instant::now();
    let workers: Vec<_> = owners
        .into_iter()
        .zip(classes)
        .map(|(owner, class)| {
            let governor = governor.clone();
            std::thread::spawn(move || {
                for _ in 0..ITERATIONS / THREADS {
                    drop(
                        governor
                            .try_reserve(owner, class, ResourceAmount { bytes: 4096, items: 1 })
                            .unwrap(),
                    );
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    let contended_seconds = start.elapsed().as_secs_f64();

    println!(
        "{{\"schema\":\"resource-governor-bench/1\",\"iterations\":{ITERATIONS},\"threads\":{THREADS},\"uncontended_ops_per_sec\":{:.0},\"contended_ops_per_sec\":{:.0}}}",
        ITERATIONS as f64 / uncontended_seconds,
        ITERATIONS as f64 / contended_seconds,
    );
}
