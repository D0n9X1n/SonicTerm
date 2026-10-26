//! Pins the generated role scripts, fixture bytes, nonce choice and scratch configuration.

use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::time::Duration;

use super::*;
use crate::scenarios::{all_plans, all_plans_on, plan, plan_for, Host, Workload, BUILD_HOST};

/// FIPS 180-4 SHA-256, used only here to pin script and fixture bytes.
struct Sha256 {
    state: [u32; 8],
    pending: Vec<u8>,
    total_len: u64,
}

const ROUND_CONSTANTS: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

impl Sha256 {
    fn new() -> Self {
        Self {
            state: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            pending: Vec::with_capacity(64),
            total_len: 0,
        }
    }

    fn update(&mut self, mut bytes: &[u8]) {
        self.total_len += bytes.len() as u64;
        if !self.pending.is_empty() {
            let take = (64 - self.pending.len()).min(bytes.len());
            self.pending.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
            if self.pending.len() == 64 {
                let block = std::mem::take(&mut self.pending);
                self.compress(block.as_slice().try_into().unwrap());
            }
        }
        let (blocks, remainder) = bytes.as_chunks::<64>();
        for block in blocks {
            self.compress(block);
        }
        self.pending.extend_from_slice(remainder);
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut schedule = [0_u32; 64];
        for (word, bytes) in schedule.iter_mut().zip(block.as_chunks::<4>().0) {
            *word = u32::from_be_bytes(*bytes);
        }
        for index in 16..64 {
            let early = schedule[index - 15];
            let late = schedule[index - 2];
            let small_zero = early.rotate_right(7) ^ early.rotate_right(18) ^ (early >> 3);
            let small_one = late.rotate_right(17) ^ late.rotate_right(19) ^ (late >> 10);
            schedule[index] = schedule[index - 16]
                .wrapping_add(small_zero)
                .wrapping_add(schedule[index - 7])
                .wrapping_add(small_one);
        }
        let mut work = self.state;
        for (constant, word) in ROUND_CONSTANTS.iter().zip(schedule) {
            let [first, second, third, fourth, fifth, sixth, seventh, eighth] = work;
            let big_one = fifth.rotate_right(6) ^ fifth.rotate_right(11) ^ fifth.rotate_right(25);
            let choose = (fifth & sixth) ^ (!fifth & seventh);
            let temp_one = eighth
                .wrapping_add(big_one)
                .wrapping_add(choose)
                .wrapping_add(*constant)
                .wrapping_add(word);
            let big_zero = first.rotate_right(2) ^ first.rotate_right(13) ^ first.rotate_right(22);
            let majority = (first & second) ^ (first & third) ^ (second & third);
            let temp_two = big_zero.wrapping_add(majority);
            work = [
                temp_one.wrapping_add(temp_two),
                first,
                second,
                third,
                fourth.wrapping_add(temp_one),
                fifth,
                sixth,
                seventh,
            ];
        }
        for (state_word, work_word) in self.state.iter_mut().zip(work) {
            *state_word = state_word.wrapping_add(work_word);
        }
    }

    fn finish(mut self) -> String {
        let bit_len = self.total_len.wrapping_mul(8);
        let mut tail = std::mem::take(&mut self.pending);
        tail.push(0x80);
        while tail.len() % 64 != 56 {
            tail.push(0);
        }
        tail.extend_from_slice(&bit_len.to_be_bytes());
        // The padding above leaves `tail` as whole 64-byte blocks, so `as_chunks` has no remainder.
        for block in tail.as_chunks::<64>().0 {
            self.compress(block);
        }
        self.state.iter().fold(String::with_capacity(64), |mut text, word| {
            text.push_str(&format!("{word:08x}"));
            text
        })
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finish()
}

/// Hashes a fixture exactly as `write_under` lays it out on disk.
fn fixture_sha256(fixtures: &[FixtureFile]) -> String {
    let mut hasher = Sha256::new();
    for fixture in fixtures {
        match &fixture.body {
            FixtureBody::Bytes(bytes) => hasher.update(bytes),
            FixtureBody::Repeated { block, count } => {
                for _ in 0..*count {
                    hasher.update(block);
                }
            }
        }
    }
    hasher.finish()
}

const SCRATCH: &str = "/tmp/perf-test";
const NONCE: &str = "0123456789abcdef";

#[test]
fn sha256_matches_published_vectors() {
    // The fixture pins below are only as good as this hasher, so it is checked against FIPS 180-4.
    let empty = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    assert_eq!(sha256_hex(b""), empty);
    let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    assert_eq!(sha256_hex(b"abc"), abc);
    let two_blocks = "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1";
    assert_eq!(sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"), two_blocks);
    // Uneven updates cross the 64-byte block boundary at every offset.
    let mut hasher = Sha256::new();
    let million = vec![b'a'; 1_000_000];
    for chunk in million.chunks(997) {
        hasher.update(chunk);
    }
    let million_a = "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0";
    assert_eq!(hasher.finish(), million_a);
}

#[test]
fn seeded_rng_matches_reference_splitmix64() {
    // Every fixture derives from this generator; the values come from an independent implementation.
    let mut zero = SeededRng::new(0);
    assert_eq!(
        [zero.next_u64(), zero.next_u64(), zero.next_u64()],
        [0xe220a8397b1dcdaf, 0x6e789e6aa1b965f4, 0x06c45d188009454f]
    );
    let mut other = SeededRng::new(1_234_567);
    assert_eq!(
        [other.next_u64(), other.next_u64(), other.next_u64()],
        [0x599ed017fb08fc85, 0x2c73f08458540fa5, 0x883ebce5a3f27c77]
    );
    let mut bounded = SeededRng::new(7);
    assert!((0..1_000).all(|_| bounded.below(36) < 36));
}

/// The whole generated script for S1, so a reader sees the protocol each role follows.
const IDLE_SCRIPT: &str = r##"#!/bin/sh
# perf_scenarios S1 default: generated role script; it ignores its arguments.
scratch='/tmp/perf-test'
role=0
while ! mkdir "$scratch/roles/$role" 2>/dev/null; do role=$((role + 1)); done
anchor_pid=$( (trap '' HUP; exec sleep 600) </dev/null >/dev/null 2>&1 & echo $! )
tty_name=$(tty 2>/dev/null) || tty_name=none
printf '{"role":%s,"leader_pid":%s,"anchor_pid":%s,"tty":"%s"}\n' "$role" "$$" "$anchor_pid" "$tty_name" > "$scratch/sessions/$role.json.tmp"
mv "$scratch/sessions/$role.json.tmp" "$scratch/sessions/$role.json"
while [ ! -e "$scratch/acks/$role" ]; do sleep 0.05; done
printf 'READY %s\n' "$role"
if [ -p "$scratch/go/$role" ]; then read -r _go < "$scratch/go/$role"; else while [ ! -e "$scratch/go/$role" ]; do sleep 0.05; done; fi
case "$role" in
0)
    export PS1='perf$ '
    exec /bin/zsh -f
    ;;
*)
    exec sleep 600
    ;;
esac
"##;

/// SHA-256 of every plan's role script, keyed `<scenario> <variant> <full|short>`.
const ROLE_SCRIPT_PINS: &[(&str, &str)] = &[
    ("S1 atlas-retry full", "719bfe93ca100bd8aa7c88d5a70421df8ffa0797387baf8e55d3430d8ed85159"),
    ("S1 atlas-retry short", "3ad8edf1efd44fbb434cdfcbf10a67bf9a0df05843e650e69a5699514a1ca12f"),
    ("S1 default full", "fac6505124bd493744dbb8738cebcdbab4969fb56cba37c3bf4e74f4446b1ef9"),
    ("S1 default short", "416899e3488ac5fb8b2543997cee2baaeef4dddeef6dd287993243afcd80fa06"),
    ("S1 gdi full", "8671b3f8ea813ef1e2e5d7bde78e50fd23dbdeb3abf35ecc78995944bef91111"),
    ("S1 gdi short", "14b0f10f88fd7d75e44e0192b2d9913bd817007c8717355ea3acba401b16b79a"),
    ("S1 role-exit full", "d8b4ac8707036e0b22f4544afdfe4683b4d0b2c73cf9af7cbf8cb7f046bbb92c"),
    ("S1 role-exit short", "7312104a6d7ff1a9c7ea70a5113bbfccea388fe1cb1a51e5aff4d355e24d11c9"),
    ("S1 status-line full", "21492e48d4cb7fece9d206d0d34972c66f58272990b34813cb897a1f8e721c11"),
    ("S1 status-line short", "07ce72fafeb71d24696ec2e57050465680d9e7898b2890bb44da0a2ecd5e04c3"),
    ("S1 wgpu full", "1bfb1b8ec08185ac9f52133a92825f36588fff7582defaa489f75a61811792a6"),
    ("S1 wgpu short", "6d5bb317c5fb6a297ef362e450ff599ceb439368e1205bc9ab2eb50d5bcfd07a"),
    ("S10 default full", "ec83408ef54d54f0f3b0989be5fcc2b6be8eaf86d4d1de5ebe718bcf43e61fb1"),
    ("S10 default short", "a329e1a8a78390cf46494f8c8e9a3f6305f7c6a907b9d9940502ec41f7833a3e"),
    ("S10 sync full", "2f99331c4ba2a7c6a6d31cb549bdeb0d56253f70d50f0e4d8807541fb9347826"),
    ("S10 sync short", "2d0e10cc479c763d6f64e09f35c2e52de8e7619e7a7290fadb2e594a0aa5c3c9"),
    ("S11 default full", "d3e9d0b3a54cf85d42a16870cc258f011185b3cc5409f75b79d9fc62dba900b0"),
    ("S11 default short", "9bb8ae50215767c464d3dd168affa108a8e612b217550906f338ff08b7c0e1c8"),
    ("S11 gdi full", "3ef47a844e901b2836f77ef6548fec78807d345f49524228a3e7430490d79514"),
    ("S11 gdi short", "acf8b3247fa585a29ae4c9053c54efee75f915a4689030839321e3de6c0da29e"),
    ("S11 release full", "f5981966ea5a6a183762ddb9fe889e4ef928844ff8fb0b5eae1c5d4d3f407976"),
    ("S11 release short", "94ed4514566191948541725f67102a38b054bc59180cb9cc7cec12a9693b933f"),
    ("S11 wgpu full", "e56affac6c4b15036336ee85f66be0005408fe978c738fddfa8eb6c30b8d2895"),
    ("S11 wgpu short", "9b2bf3ac61745112108d3ef8f3e38c1db077ad0704fe1123367d1089a3811e16"),
    ("S12 default full", "3d60537ebeaab2c8f0e7d18f0fac8ae015e5927eed94ffbd3e15c12db76373b4"),
    ("S12 default short", "800f25bd5fd34f2e15749136b3f6b7b23df09bab219127dc07da9cc0845e0022"),
    ("S12 status-line full", "a1407eb7222747d569bbd761157d651ebacd4334a3134332595310ea1a43e65e"),
    ("S12 status-line short", "4a03c33e4327ab15b3d551cef06c4345f3c7ece37f8cfd262dadfa1c0b1a9315"),
    ("S2 default full", "b8794652682021a93564b1a2821feb8336f918626dde0156749fc8245abf8864"),
    ("S2 default short", "0a29f4e7229a367ab9f687772abb28d8120420f2bcad9592f26e7cbd7a5accba"),
    ("S2 flood full", "c80b829c203c03d8a7bd02df240c39996c067ae4b80f88b02ad6576712eb477f"),
    ("S2 flood short", "f8b4bbf8f0aea9ed8b93f7a311af9e2608a04b0170bd69941e5a27ba6e6f54de"),
    ("S3 default full", "12b8b7fc1ddecb3119b906641ae11d5533bbfaa7ee48cd85cb58409fd7bac939"),
    ("S3 default short", "454ba851ad2ac3038d87c74b973a4f65173d6e2a597bf28867204871a21c562e"),
    ("S4 default full", "c0d00aef8743d934be6d50fb21428f32f7a7e60fa8055d323e52746fd044058e"),
    ("S4 default short", "3616cc09f09a6f8d6fcaacd792e2a362ab336ae6031ce91b37582e9205f1c371"),
    ("S5 default full", "2b9fe5553e3cef6279e430f3808faa5c9fa3551dc7acdd53ae8e52adfacdb950"),
    ("S5 default short", "de2d10a7449cb0216df18ac0a339298a7cd07035d12aa36fc613eced7adeda74"),
    ("S5 gdi full", "6fd9602dae468cfd0a17b84031b107539bc5ff20555abe142e802bc18ea0592a"),
    ("S5 gdi short", "eb71b4fbff7e0d71740a0b979a16724b2083d07c9bbf14b1dc6aa3f4bb730e67"),
    ("S5 wgpu full", "d051e7c02f8f55e38b0dc577aa57d1ca8ad59087034eeaf819db531c78361599"),
    ("S5 wgpu short", "ec1886dbc09898bc13adb6a05c6d6eadaa2005a885f030f0a5fd399ac7a6ffa3"),
    ("S6 default full", "9864653bc7f88db5a160cd74c68404541d371d3247d495b5db4de68680fdfa47"),
    ("S6 default short", "735d2424ae451cb72494c84a39aacd6e7cfb186956ec214b8021d9b4bd8bfc4b"),
    ("S6 flood full", "1e740b04d7b0b15cac6014923c669d6b59809ca9b237f7dc994412025c95fb00"),
    ("S6 flood short", "063788dcb3da6995a11ac2cfdae9e8cf5f4ce9920e4b1f752b0eef2b6443809e"),
    ("S6 rest full", "b880315de67856eeffb66252d1dcd2641cfd278cf8ec54bb34249a8272de1ab3"),
    ("S6 rest short", "4b6228b6e44753a85275322760c568dbb2e91d4346442027dca5e3f67fe88fdd"),
    ("S6 selection-drag full", "ea90120cc78d5150787211e3ca90363bb39195c01def76f783a537d4bdbf9192"),
    ("S6 selection-drag short", "a75bea918151beb718c836bbae15d2712f98f796e6a97feedf8943c59d554a9a"),
    ("S7 default full", "9f08a3ceff1862800648cbbf60f0a5cf43fd2ac17f96b8dd3f69efc5c185dce7"),
    ("S7 default short", "075f538393f42e86b87f4f1a7ebb8f951c3b298391eeaad895185b28678392b9"),
    ("S8 default full", "e903a64e0a7210ead6832a47af0d3206309cfcf92700a2c95d72e42817f1bcdc"),
    ("S8 default short", "afe9d2b6a1e2606892cfce766935e15327c34fbc63e45eda25ef180ccd70219e"),
    ("S9 default full", "e8b589db3e6877ac295220e3349d750a00226f6684dcf95aee80be0321725814"),
    ("S9 default short", "6276da9704628cdabacc26f25ca655d3f7fd67bcf1637111cc6d6c68d38f4232"),
];

/// SHA-256 of every distinct fixture, keyed by file and length or by frame set.
const FIXTURE_PINS: &[(&str, &str)] = &[
    ("bulk.txt 5242880", "1c45218ab8edc05eb5964253bffd8809540e4b0f0e19c3918b821be59818a5c7"),
    ("bulk.txt 52428800", "6329aefe7c851b9410cf38309a0c5279e508b928f5cbb86610c8767f0b5f6d7e"),
    ("atlas-retry.txt 3430", "9d6de336ba30247c36ca1c68bc7217727464395eb0870f437cebce368f26ef39"),
    ("dense.txt 17500", "1891903144eaf328e9013124afb2253fe5ba9948e0eb29b84b824fb9cde3fa36"),
    ("emoji-cjk.txt 25207", "2475f8724fa4dffa358dd6ecd12a90cb3483a1e7f839cb8d2e41f589f0282beb"),
    (
        "frames count=1200 synchronized=false",
        "aea4dc2d6bde4a4ddeaf8e03102ce13ba780072c0c2f0d487a62a2ff78eac7fc",
    ),
    (
        "frames count=1200 synchronized=true",
        "e5f382f2fd549d600cbb98d4076218ada2eca135adb9e859370fdbc000388f5a",
    ),
    (
        "frames count=300 synchronized=false",
        "bcb0c7d3dc13c67019b1d0d806ad5bec9a05a55d173523764472a977828c92f7",
    ),
    (
        "frames count=300 synchronized=true",
        "e67716c2e7eb4b61afaac17bb42e20a33b754ca80992f1c48c16ffe1cdd989fb",
    ),
    ("history.txt 97500", "a23f9a80a94d5bce790e812b1ecdf5626f5c0e2ecdfdd856908663cdd7dc8a94"),
    ("image.sixel 377", "5811aa986380a75e770fa445490d45057bf949b5d52d6108995a05d0b7c7c0f0"),
    ("inline.osc 461289", "06c2d2935c10dfc293a38ec38149c97a1e97c90501694c5e75cb14d904476f2c"),
    (
        "linked-history.txt 1443893",
        "5ced320f01d3e8aa0c77f067e7c1966e07098a8dc142ed9ee3a37627ccf1b4cf",
    ),
    ("scrollback.txt 960000", "80f85648d2537805c9ea6b138a622d3055972d18904232b35380473a8d4dd8a0"),
    ("search.txt 17500", "3b7beea9b0581d1dcb96b3d10a1c3d879b7189051c1c1aff3b4d3a5187b62eba"),
];

fn render_pins(pins: &BTreeMap<String, String>) -> String {
    pins.iter().map(|(key, value)| format!("    ({key:?}, {value:?}),\n")).collect()
}

fn plan_key(plan: &crate::scenarios::Plan) -> String {
    format!("{} {} {}", plan.scenario, plan.variant, if plan.short { "short" } else { "full" })
}

/// Every distinct fixture set across all plans: one file, or one ordered frame sequence.
fn fixture_sets() -> &'static BTreeMap<String, Vec<FixtureFile>> {
    static SETS: OnceLock<BTreeMap<String, Vec<FixtureFile>>> = OnceLock::new();
    SETS.get_or_init(|| {
        let mut sets = BTreeMap::new();
        for plan in all_plans().into_iter().chain(all_plans_on(Host::Windows)) {
            let (frames, singles): (Vec<_>, Vec<_>) = fixtures(&plan)
                .into_iter()
                .partition(|fixture| fixture.relative_path.starts_with("frames/"));
            for fixture in singles {
                let key = format!("{} {}", fixture.relative_path, fixture.byte_len());
                sets.entry(key).or_insert_with(|| vec![fixture]);
            }
            for role in &plan.roles {
                if let Workload::Frames { count, synchronized } = role {
                    let key = format!("frames count={count} synchronized={synchronized}");
                    sets.entry(key).or_insert_with(|| frames.clone());
                }
            }
        }
        sets
    })
}

#[test]
fn idle_role_script_is_pinned_line_for_line() {
    // The anchor, session record, acknowledgement and GO handshake are the cleanup contract.
    let idle = plan("S1", "default", false).unwrap();
    assert_eq!(role_script(&idle, SCRATCH, NONCE), IDLE_SCRIPT);
}

#[test]
fn every_role_script_is_pinned() {
    // A changed script changes what a scenario measures, so every plan's script is pinned.
    let actual: BTreeMap<String, String> = all_plans()
        .iter()
        .map(|plan| (plan_key(plan), sha256_hex(role_script(plan, SCRATCH, NONCE).as_bytes())))
        .collect();
    let expected: BTreeMap<String, String> =
        ROLE_SCRIPT_PINS.iter().map(|(key, value)| (key.to_string(), value.to_string())).collect();
    assert_eq!(actual, expected, "ROLE_SCRIPT_PINS:\n{}", render_pins(&actual));
}

#[test]
fn every_fixture_is_pinned() {
    // Fixtures are generated, never stored, so their bytes are pinned by hash.
    let actual: BTreeMap<String, String> =
        fixture_sets().iter().map(|(key, files)| (key.clone(), fixture_sha256(files))).collect();
    let expected: BTreeMap<String, String> =
        FIXTURE_PINS.iter().map(|(key, value)| (key.to_string(), value.to_string())).collect();
    assert_eq!(actual, expected, "FIXTURE_PINS:\n{}", render_pins(&actual));
}

#[test]
fn fixtures_never_contain_protocol_text() {
    // A fixture holding a sentinel, a READY line or the prompt would end a phase early.
    for (key, files) in fixture_sets() {
        for file in files {
            for needle in [&b"PERF_DONE"[..], b"READY", PROMPT.as_bytes()] {
                assert!(!file.contains(needle), "{key} {} holds {needle:?}", file.relative_path);
            }
        }
    }
}

#[test]
fn bulk_fixture_is_whole_printable_lines_of_the_requested_size() {
    // S3 counts its bytes, so the 50 MiB (5 MiB short) fixture must be exactly that long.
    for (short, mebibytes) in [(false, 50), (true, 5)] {
        let flood = plan("S3", "default", short).unwrap();
        let files = fixtures(&flood);
        let [bulk] = files.as_slice() else { panic!("S3 writes one fixture") };
        assert_eq!(bulk.relative_path, "bulk.txt");
        assert_eq!(bulk.byte_len(), mebibytes << 20);
        let FixtureBody::Repeated { block, count } = &bulk.body else { panic!("bulk repeats") };
        assert_eq!((block.len(), *count), (1 << 20, mebibytes));
        assert_eq!(block.last(), Some(&b'\n'));
        for line in block[..block.len() - 1].split(|byte| *byte == b'\n') {
            assert_eq!(line.len(), 127);
            assert!(line.iter().all(|byte| (0x20..=0x7e).contains(byte)));
        }
    }
}

#[test]
fn frame_fixtures_enter_and_leave_the_alternate_screen() {
    // S10 plays full-screen streams; sync wraps every frame in DEC 2026 brackets and nothing else does.
    for (variant, synchronized) in [("default", false), ("sync", true)] {
        let stream = plan("S10", variant, true).unwrap();
        let frames: Vec<_> = fixtures(&stream)
            .into_iter()
            .map(|fixture| {
                let FixtureBody::Bytes(bytes) = fixture.body else { panic!("frames are bytes") };
                (fixture.relative_path, bytes)
            })
            .collect();
        assert_eq!(frames.len(), 300);
        for (index, (path, bytes)) in frames.iter().enumerate() {
            assert_eq!(*path, format!("frames/{index}"));
            let bracketed = bytes.starts_with(b"\x1b[?2026h") && bytes.ends_with(b"\x1b[?2026l");
            assert_eq!(bracketed, synchronized, "{variant} frame {index}");
            let inner = if synchronized { &bytes[8..bytes.len() - 8] } else { &bytes[..] };
            assert!(!inner.windows(7).any(|window| window == b"\x1b[?2026"), "nested bracket");
        }
        let first = &frames[0].1[if synchronized { 8 } else { 0 }..];
        assert!(first.starts_with(b"\x1b[?1049h\x1b[?7l\x1b[2J"));
        let last = &frames[299].1;
        let last = &last[..last.len() - if synchronized { 8 } else { 0 }];
        assert!(last.ends_with(b"\x1b[?7h\x1b[?1049l"));
    }
}

#[test]
fn sixel_fixture_arrives_as_one_capture() {
    // S11 waits for a registered image, so its fixture must be one complete Sixel DCS.
    use sonicterm_grid::grid::Grid;
    use sonicterm_vt::vt::{CaptureStagingPool, MediaProtocol, Parser, VtEvent};
    let image = plan_for("S11", "default", false, Host::Posix).unwrap();
    let files = fixtures(&image);
    let [sixel] = files.as_slice() else { panic!("S11 writes one fixture") };
    let FixtureBody::Bytes(bytes) = &sixel.body else { panic!("sixel is bytes") };
    let mut parser =
        Parser::new_with_staging_pool(Grid::new(250, 70), None, CaptureStagingPool::new());
    let media: Vec<_> = parser
        .advance(bytes)
        .into_iter()
        .filter_map(|event| match event {
            VtEvent::Media(media) => Some(media),
            _ => None,
        })
        .collect();
    assert_eq!(media.len(), 1);
    assert_eq!(media[0].protocol, MediaProtocol::Sixel);
    assert!(media[0].data.contains(&b'#') && media[0].data.len() > 100);
}

#[test]
fn nonce_avoids_every_fixture_including_repeated_seams() {
    // The sentinel nonce must occur nowhere in a fixture, even across a repeated block's seam.
    let mut rng = SeededRng::new(42);
    let first = format!("{:016x}", rng.next_u64());
    let second = format!("{:016x}", rng.next_u64());
    let clean =
        FixtureFile { relative_path: "clean.txt".into(), body: FixtureBody::Bytes(b"x".to_vec()) };
    assert_eq!(choose_nonce(42, std::slice::from_ref(&clean)), first);
    let body = FixtureBody::Bytes(format!("a{first}b").into_bytes());
    let colliding = FixtureFile { relative_path: "collide.txt".into(), body };
    assert_eq!(choose_nonce(42, &[clean, colliding]), second);
    let (head, tail) = first.split_at(8);
    let block = format!("{tail}----{head}").into_bytes();
    let seam = FixtureFile {
        relative_path: "seam.txt".into(),
        body: FixtureBody::Repeated { block, count: 2 },
    };
    assert!(seam.contains(first.as_bytes()));
    assert_eq!(choose_nonce(42, &[seam]), second);
}

fn scratch_dir(label: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("perf-scenarios-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn written_fixtures_match_their_pins() {
    // `write_under` must lay out exactly the bytes the pins hash, including repeated blocks.
    let dir = scratch_dir("fixtures");
    for (scenario, variant, short) in
        [("S9", "default", false), ("S3", "default", true), ("S10", "sync", true)]
    {
        for fixture in fixtures(&plan(scenario, variant, short).unwrap()) {
            fixture.write_under(&dir).unwrap();
            let written = std::fs::read(dir.join(&fixture.relative_path)).unwrap();
            assert_eq!(sha256_hex(&written), fixture_sha256(std::slice::from_ref(&fixture)));
        }
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn scratch_config_loads_strictly_with_pinned_values() {
    // The run's config is explicit, so a later default change cannot alter a scenario.
    use sonicterm_logging::config::LogLevel;
    let dir = scratch_dir("config");
    let scratch = dir.to_str().unwrap();
    // S1 requests the status line comparisons' 40 rows; S7 keeps the harness's 70.
    for (scenario, laps, level, scrollback, rows) in
        [("S1", false, LogLevel::Info, 1_000, 40), ("S7", true, LogLevel::Debug, 10_000, 70)]
    {
        let path = dir.join("sonicterm.toml");
        std::fs::write(
            &path,
            config_toml(&plan(scenario, "default", false).unwrap(), scratch, laps),
        )
        .unwrap();
        let config = sonicterm_cfg::config::Config::load_strict(&path).unwrap();
        assert_eq!((config.window.cols, config.window.rows), (250, rows));
        assert_eq!((config.font.family.as_str(), config.font.size), (FONT_FAMILY, 13.0));
        assert_eq!(config.logging.level, level);
        assert_eq!(config.terminal.shell, Some(format!("{scratch}/workload/role.sh")));
        assert_eq!(config.terminal.scrollback, scrollback);
        assert_eq!(config.window.warm_window_pool, 1, "S12 measures the default pool of one");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn protocol_lines_match_what_the_probe_scans_for() {
    // The probe matches these exact lines in the grid; the script must print the same text.
    assert_eq!(PROMPT, "perf$ ");
    assert_eq!(sentinel_line(2, NONCE), "PERF_DONE 2 0123456789abcdef");
    assert_eq!(ready_line(1), "READY 1");
    let script = role_script(&plan("S12", "default", false).unwrap(), SCRATCH, NONCE);
    for role in 0..3 {
        assert!(script.contains(&format!("printf '{}\\n'", sentinel_line(role, NONCE))));
    }
}

#[cfg(unix)]
#[test]
fn every_role_script_parses_as_posix_sh() {
    // A script that does not parse would leave its pane without a role, so each one must parse.
    let dir = scratch_dir("scripts");
    for plan in all_plans() {
        let path = dir.join(format!("{}.sh", plan_key(&plan).replace(' ', "-")));
        std::fs::write(&path, role_script(&plan, SCRATCH, NONCE)).unwrap();
        let status = std::process::Command::new("/bin/sh").arg("-n").arg(&path).status().unwrap();
        assert!(status.success(), "{} does not parse as sh", plan_key(&plan));
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_laps_run_without_counters_keeps_render_timing_and_drops_frame_counters() {
    // --laps selects Debug for render_timing, but Debug also opens the App's counter gate; a run
    // without --counters must admit the laps targets and turn the frame_counters target off.
    let filter = logging_filter(true, false).expect("a laps run without counters needs a filter");
    assert!(filter.contains("render_timing=debug"), "{filter}");
    assert!(filter.contains("frame_counters=off"), "{filter}");
    assert!(!filter.contains("frame_counters=debug"), "{filter}");
    // With --counters the gate is forced on anyway, and without --laps Info never opens it.
    assert_eq!(logging_filter(true, true), None);
    assert_eq!(logging_filter(false, false), None);
    assert_eq!(logging_filter(false, true), None);
}

#[test]
fn program_steps_mirror_posix_workload_lines() {
    // The Windows program performs these steps, so they must be exactly what the POSIX script runs.
    for plan in all_plans() {
        let bound_s = plan.timeout_s + ANCHOR_MARGIN_S;
        for (role, workload) in plan.roles.iter().enumerate() {
            let mirrored: Vec<String> = program_steps(*workload)
                .iter()
                .flat_map(|step| posix_lines(step, role, NONCE, bound_s))
                .collect();
            let expected = workload_lines(*workload, role, NONCE, bound_s);
            assert_eq!(mirrored, expected, "{} role {role}", plan_key(&plan));
        }
    }
}

#[test]
fn program_yes_writes_what_yes_piped_to_head_writes() {
    // S3 credits two bytes a `yes` line, including across the program's 64 KiB buffer seam.
    for lines in [0_u32, 1, 32_769] {
        let mut written = Vec::new();
        write_yes(&mut written, lines).unwrap();
        assert_eq!(written, b"y\n".repeat(lines as usize), "{lines} lines");
    }
}

/// Renders a single-quoted `printf` format the way POSIX `printf` does for `%s` and `\n` only.
fn render_printf(format: &str, args: &[&str]) -> String {
    let mut args = args.iter();
    format
        .replace(r"\n", "\n")
        .split("%s")
        .enumerate()
        .map(|(index, piece)| {
            let arg = if index == 0 { "" } else { args.next().expect("an argument per %s") };
            format!("{arg}{piece}")
        })
        .collect()
}

/// The quoted format of a `printf '<format>' …` script line.
fn printf_format(line: &str) -> &str {
    let rest = line.trim().strip_prefix("printf '").expect("a printf line");
    &rest[..rest.find('\'').expect("a closing quote")]
}

#[test]
fn program_protocol_lines_match_the_role_script() {
    // The probe scans for the same READY and sentinel bytes whichever side printed them.
    let three_roles = plan("S12", "default", false).unwrap();
    let bound_s = three_roles.timeout_s + ANCHOR_MARGIN_S;
    let script = role_script(&three_roles, SCRATCH, NONCE);
    let ready = script.lines().find(|line| line.starts_with("printf 'READY")).unwrap();
    for role in 0..3 {
        let role_text = role.to_string();
        let printed = render_printf(printf_format(ready), &[role_text.as_str()]);
        assert_eq!(printed, format!("{}\n", ready_line(role)));
        let sentinel = posix_lines(&ProgramStep::Sentinel, role, NONCE, bound_s).remove(0);
        assert!(script.contains(&sentinel), "role {role} prints its sentinel");
        let printed = render_printf(printf_format(&sentinel), &[]);
        assert_eq!(printed, format!("{}\n", sentinel_line(role, NONCE)));
    }
    // The probe exports this name and the program reads it, so it is a cross-process contract.
    assert_eq!(SCRATCH_ENV, "SONICTERM_PERF_SCRATCH");
    let record: serde_json::Value = serde_json::from_str(&program_session_json(2, 4_242)).unwrap();
    assert_eq!(record, serde_json::json!({"role": 2, "program_pid": 4_242, "tty": "none"}));
}

#[test]
fn date_line_matches_posix_date_in_utc() {
    // S4 streams `date` output; the program's UTC line keeps the C-locale `date` shape.
    assert_eq!(date_line(0), "Thu Jan  1 00:00:00 UTC 1970");
    assert_eq!(date_line(1_709_210_096), "Thu Feb 29 12:34:56 UTC 2024");
    assert_eq!(date_line(2_147_483_648), "Tue Jan 19 03:14:08 UTC 2038");
    assert_eq!(date_line(4_102_444_800), "Fri Jan  1 00:00:00 UTC 2100");
}

#[test]
fn program_spec_round_trips_and_rebuilds_the_plan() {
    // The program rebuilds its plan from `program.json`, so the spec must name the same plan.
    for original in all_plans_on(BUILD_HOST) {
        let spec = parse_program_json(&program_json(&original, NONCE)).unwrap();
        assert_eq!(
            (spec.scenario.as_str(), spec.variant.as_str(), spec.short, spec.nonce.as_str()),
            (original.scenario, original.variant, original.short, NONCE)
        );
        let rebuilt = plan(&spec.scenario, &spec.variant, spec.short).unwrap();
        // `Plan` has no `PartialEq`; its derived `Debug` prints every field.
        assert_eq!(format!("{rebuilt:?}"), format!("{original:?}"));
    }
    // Each malformed document is refused with a reason naming what is wrong.
    for (text, reason) in [
        ("not json", "not JSON"),
        ("[]", "not an object"),
        (r#"{"scenario":"S1","variant":"default","short":false}"#, "nonce"),
        (r#"{"scenario":"S1","variant":"default","short":"no","nonce":"ab"}"#, "short"),
        (r#"{"scenario":1,"variant":"default","short":false,"nonce":"ab"}"#, "scenario"),
    ] {
        let error = parse_program_json(text).unwrap_err();
        assert!(error.contains(reason), "{text}: {error}");
    }
}

#[test]
fn windows_config_names_the_harness_as_the_shell() {
    // On Windows the harness binary is the shell, and the POSIX config's bytes stay as they were.
    let dir = scratch_dir("windows-config");
    let idle = plan("S1", "default", false).unwrap();
    let harness = r"C:\Temp\perf_scenarios.exe";
    let path = dir.join("sonicterm.toml");
    std::fs::write(&path, config_toml_with_shell(&idle, harness, false)).unwrap();
    let config = sonicterm_cfg::config::Config::load_strict(&path).unwrap();
    assert_eq!(config.terminal.shell.as_deref(), Some(harness));
    std::fs::remove_dir_all(&dir).unwrap();
    let posix = format!(
        "[window]\ncols = 250\nrows = 40\n\n\
         [font]\nfamily = \"{FONT_FAMILY}\"\nsize = 13.0\n\n\
         [logging]\nlevel = \"info\"\n\n\
         [terminal]\nshell = '/tmp/perf-test/workload/role.sh'\nscrollback = 1000\n"
    );
    assert_eq!(config_toml(&idle, SCRATCH, false), posix);
}

#[test]
fn cmd_prompt_renders_the_shared_prompt() {
    // S2 waits for `PROMPT`; cmd.exe expands `$$` to `$` and `$S` to a space.
    let mut rendered = String::new();
    let mut chars = CMD_PROMPT.chars();
    while let Some(next) = chars.next() {
        if next != '$' {
            rendered.push(next);
            continue;
        }
        match chars.next() {
            Some('$') => rendered.push('$'),
            Some('S' | 's') => rendered.push(' '),
            other => panic!("unexpected cmd prompt code {other:?}"),
        }
    }
    assert_eq!(rendered, PROMPT);
}

/// Unix seconds the fake host's clock starts at: 2024-02-29 12:34:56 UTC.
const FAKE_START_S: u64 = 1_709_210_096;

/// A host whose clock advances only by the sleeps it records; it never really sleeps.
struct FakeHost {
    /// Every sleep requested and granted, in order.
    sleeps: Vec<Duration>,
    /// Sleeps granted before the next one fails, which is how a test ends an endless step.
    sleeps_allowed: usize,
    /// How many times the idle shell ran.
    shells: usize,
}

impl FakeHost {
    fn new(sleeps_allowed: usize) -> Self {
        Self { sleeps: Vec::new(), sleeps_allowed, shells: 0 }
    }
}

impl ProgramHost for FakeHost {
    fn now_unix_s(&self) -> u64 {
        let slept: Duration = self.sleeps.iter().sum();
        FAKE_START_S + slept.as_secs()
    }

    fn sleep(&mut self, duration: Duration) -> std::io::Result<()> {
        if self.sleeps.len() >= self.sleeps_allowed {
            return Err(std::io::Error::other("the fake host stops here"));
        }
        self.sleeps.push(duration);
        Ok(())
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn run_shell(&mut self) -> std::io::Result<()> {
        self.shells += 1;
        Ok(())
    }
}

/// A writer whose every write fails, as a closed console would.
struct FailingWriter;

impl Write for FailingWriter {
    fn write(&mut self, _bytes: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("console closed"))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A scratch directory laid out as `prepare_scratch` lays it out on Windows, with `plan`'s
/// fixtures, `program.json` and every planned role's ack; `go_written` adds every go file too.
fn program_dir(label: &str, plan: &Plan, go_written: bool) -> std::path::PathBuf {
    let dir = scratch_dir(label);
    for sub in ["workload/fixtures", "roles", "sessions", "acks", "go", "done"] {
        std::fs::create_dir_all(dir.join(sub)).unwrap();
    }
    for fixture in fixtures(plan) {
        fixture.write_under(&dir.join("workload/fixtures")).unwrap();
    }
    std::fs::write(dir.join("workload/program.json"), program_json(plan, NONCE)).unwrap();
    for role in 0..plan.roles.len() {
        std::fs::write(dir.join(format!("acks/{role}")), "").unwrap();
        if go_written {
            std::fs::write(dir.join(format!("go/{role}")), "").unwrap();
        }
    }
    dir
}

/// What `role`'s POSIX script prints after it is acknowledged, derived from the workload alone:
/// READY, the workload's bytes with each fixture once and in order, then the sentinel.
fn expected_role_output(plan: &Plan, role: usize) -> Vec<u8> {
    let files = fixtures(plan);
    let file_bytes = |name: &str| -> Vec<u8> {
        let file = files.iter().find(|file| file.relative_path == name).expect("a planned fixture");
        match &file.body {
            FixtureBody::Bytes(bytes) => bytes.clone(),
            FixtureBody::Repeated { block, count } => block.repeat(*count),
        }
    };
    let mut expected = format!("{}\n", ready_line(role)).into_bytes();
    let body = match plan.roles[role] {
        Workload::IdleShell | Workload::ExitAfterGo => return expected,
        Workload::DateLoop | Workload::RestStream => {
            panic!("a date loop's output depends on the clock")
        }
        Workload::Flood { lines, .. } => {
            [b"y\n".repeat(lines as usize), file_bytes("bulk.txt")].concat()
        }
        Workload::PrintThenShell(fixture) | Workload::PrintThenSleep(fixture) => {
            file_bytes(fixture_file_name(fixture))
        }
        Workload::Frames { count, .. } => {
            (0..count).flat_map(|index| file_bytes(&format!("frames/{index}"))).collect()
        }
    };
    expected.extend(body);
    expected.extend(format!("{}\n", sentinel_line(role, NONCE)).bytes());
    expected
}

#[test]
fn run_steps_writes_nothing_between_ready_and_go() {
    // The probe writes GO only after it finds READY, so nothing may follow READY until GO exists.
    let flood = plan("S3", "default", true).unwrap();
    let waiting = program_dir("ready-waiting", &flood, false);
    let mut host = FakeHost::new(20);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(
        run_steps(&waiting, &mut host, &mut out, &mut err),
        1,
        "the fake host ends the wait"
    );
    assert_eq!(out, b"READY 0\n");
    assert_eq!(host.sleeps, vec![Duration::from_millis(50); 20], "go/0 is polled every 50 ms");
    // The session record names the role and this process, as the comparison script reads it.
    let record = std::fs::read_to_string(waiting.join("sessions/0.json")).unwrap();
    let record: serde_json::Value = serde_json::from_str(&record).unwrap();
    assert_eq!(record["role"], 0);
    assert_eq!(record["program_pid"], std::process::id());
    std::fs::remove_dir_all(&waiting).unwrap();
    // Once GO exists, the workload follows READY directly.
    let started = program_dir("ready-go", &flood, true);
    let mut host = FakeHost::new(usize::MAX);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(run_steps(&started, &mut host, &mut out, &mut err), 0);
    assert!(out.starts_with(b"READY 0\ny\ny\n"));
    assert!(out == expected_role_output(&flood, 0), "S3 output differs");
    std::fs::remove_dir_all(&started).unwrap();
}

#[test]
fn run_steps_plays_each_fixture_in_order() {
    // Every role prints what its POSIX script prints, each fixture once, in order, then its sentinel.
    let plans = all_plans_on(BUILD_HOST).into_iter().filter(|plan| {
        plan.short && !fixtures(plan).is_empty() && !plan.roles.contains(&Workload::DateLoop)
    });
    for plan in plans {
        let key = plan_key(&plan);
        let dir = program_dir(&format!("play-{}", key.replace(' ', "-")), &plan, true);
        for role in 0..plan.roles.len() {
            let expected = expected_role_output(&plan, role);
            let mut host = FakeHost::new(usize::MAX);
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let code = run_steps(&dir, &mut host, &mut out, &mut err);
            assert_eq!(code, 0, "{key} role {role}: {}", String::from_utf8_lossy(&err));
            // A writer that repeated a fixture would print more than one copy of each.
            assert_eq!(out.len(), expected.len(), "{key} role {role} length");
            assert!(out == expected, "{key} role {role} bytes differ");
            let finished = plan.roles[role] != Workload::IdleShell;
            assert_eq!(dir.join(format!("done/{role}")).exists(), finished, "{key} role {role}");
            if let Workload::PrintThenSleep(_) = plan.roles[role] {
                let bound = Duration::from_secs(plan.timeout_s + ANCHOR_MARGIN_S);
                assert_eq!(host.sleeps.last(), Some(&bound), "{key} role {role} sleeps the bound");
                assert_eq!(host.shells, 0);
            } else {
                assert_eq!(host.shells, 1, "{key} role {role} ends in the idle shell");
            }
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[test]
fn run_steps_writes_the_frames_of_both_s10_variants() {
    // S10 plays its frame files in order, each once and paced 16 ms; only `sync` brackets them.
    for (variant, synchronized) in [("default", false), ("sync", true)] {
        let stream = plan("S10", variant, true).unwrap();
        let dir = program_dir(&format!("frames-{variant}"), &stream, true);
        let mut host = FakeHost::new(usize::MAX);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(run_steps(&dir, &mut host, &mut out, &mut err), 0);
        let mut rest = out.strip_prefix(b"READY 0\n".as_slice()).expect("READY comes first");
        let frames = fixtures(&stream);
        assert_eq!(frames.len(), 300);
        for (index, frame) in frames.iter().enumerate() {
            let FixtureBody::Bytes(bytes) = &frame.body else { panic!("frames are bytes") };
            assert_eq!(frame.relative_path, format!("frames/{index}"));
            assert!(rest.len() >= bytes.len(), "{variant} output ends before frame {index}");
            let (head, tail) = rest.split_at(bytes.len());
            assert!(head == bytes.as_slice(), "{variant} frame {index} differs");
            let bracketed = head.starts_with(b"\x1b[?2026h") && head.ends_with(b"\x1b[?2026l");
            assert_eq!(bracketed, synchronized, "{variant} frame {index} brackets");
            rest = tail;
        }
        assert_eq!(rest, format!("{}\n", sentinel_line(0, NONCE)).as_bytes());
        assert_eq!(host.sleeps, vec![Duration::from_millis(16); 300]);
        assert_eq!(host.shells, 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[test]
fn run_steps_date_loop_ends_each_line() {
    // S4 streams one `date` line every 10 ms, each ending in LF; the fake host ends the loop.
    let stream = plan("S4", "default", true).unwrap();
    let dir = program_dir("date-loop", &stream, true);
    let mut host = FakeHost::new(3);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(run_steps(&dir, &mut host, &mut out, &mut err), 1, "the loop never ends by itself");
    let line = date_line(FAKE_START_S);
    let text = String::from_utf8(out).unwrap();
    assert_eq!(text, format!("READY 0\n{line}\n{line}\n{line}\n{line}\n"));
    assert_eq!(host.sleeps, vec![Duration::from_millis(10); 3]);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn run_steps_unplanned_role_only_sleeps_the_bound() {
    // An extra pane follows the handshake like the script's `*)` branch, then prints nothing more.
    let idle = plan("S1", "default", true).unwrap();
    let dir = program_dir("unplanned", &idle, true);
    std::fs::create_dir(dir.join("roles/0")).unwrap();
    std::fs::write(dir.join("acks/1"), "").unwrap();
    std::fs::write(dir.join("go/1"), "").unwrap();
    let mut host = FakeHost::new(usize::MAX);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(run_steps(&dir, &mut host, &mut out, &mut err), 0);
    assert_eq!(out, b"READY 1\n");
    assert_eq!(host.sleeps, vec![Duration::from_secs(idle.timeout_s + ANCHOR_MARGIN_S)]);
    assert_eq!(host.shells, 0);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn program_returns_1_on_a_write_error() {
    // A program that cannot write stops at once with one line naming the error, ending its role.
    let idle = plan("S1", "default", true).unwrap();
    let dir = program_dir("write-error", &idle, true);
    let mut host = FakeHost::new(usize::MAX);
    let mut err = Vec::new();
    assert_eq!(run_steps(&dir, &mut host, &mut FailingWriter, &mut err), 1);
    let text = String::from_utf8(err).unwrap();
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(text.ends_with('\n') && text.contains("console closed"), "{text}");
    assert_eq!(host.shells, 0, "nothing runs after the failed write");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Bitwise CRC-32 (IEEE), written apart from the fixture's, so the check is independent of it.
fn reference_crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

#[test]
fn inline_png_fixture_decodes_and_fits_one_osc() {
    // Windows S11 prints one OSC 1337 inline PNG, since Sixel never arrives through ConPTY.
    let image = plan_for("S11", "default", false, Host::Windows).unwrap();
    let files = fixtures(&image);
    let [inline] = files.as_slice() else { panic!("Windows S11 writes one fixture") };
    assert_eq!(inline.relative_path, "inline.osc");
    let FixtureBody::Bytes(bytes) = &inline.body else { panic!("the OSC is bytes") };
    assert!(bytes.len() <= 1 << 20, "{} bytes", bytes.len());
    // The name is base64 of `inline.png`.
    let prefix = b"\x1b]1337;File=name=aW5saW5lLnBuZw==;inline=1:";
    assert!(bytes.starts_with(prefix));
    assert!(bytes.ends_with(b"\x07\n"));
    assert_eq!(bytes.iter().filter(|byte| **byte == 0x1b).count(), 1, "one OSC");
    assert_eq!(bytes.iter().filter(|byte| **byte == 0x07).count(), 1, "one BEL");
    use base64::Engine;
    let png = base64::engine::general_purpose::STANDARD
        .decode(&bytes[prefix.len()..bytes.len() - 2])
        .unwrap();
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    // IHDR: 480 x 240, 8-bit RGB, deflate, the only filter method, no interlace.
    assert_eq!(&png[12..16], b"IHDR");
    assert_eq!(png[16..29], [0, 0, 1, 0xe0, 0, 0, 0, 0xf0, 8, 2, 0, 0, 0]);
    // Every chunk's CRC-32 is checked here with a separate implementation.
    let mut offset = 8;
    let mut names = Vec::new();
    while offset < png.len() {
        let length = u32::from_be_bytes(png[offset..offset + 4].try_into().unwrap()) as usize;
        let named = &png[offset + 4..offset + 8 + length];
        let crc_at = offset + 8 + length;
        let stored = u32::from_be_bytes(png[crc_at..crc_at + 4].try_into().unwrap());
        assert_eq!(reference_crc32(named), stored, "CRC of the chunk at {offset}");
        names.push(String::from_utf8_lossy(&named[..4]).into_owned());
        offset = crc_at + 4;
    }
    assert_eq!(names, ["IHDR", "IDAT", "IEND"]);
    // The app's own decoder checks the zlib stream and its Adler-32 as it inflates the bands.
    let decoded =
        image::load_from_memory_with_format(&png, image::ImageFormat::Png).unwrap().to_rgb8();
    assert_eq!(decoded.dimensions(), (480, 240));
    // Bands six rows tall cycle through the Sixel fixture's three colors.
    assert_eq!(decoded.get_pixel(0, 0).0, [230, 51, 51]);
    assert_eq!(decoded.get_pixel(479, 6).0, [51, 204, 77]);
    assert_eq!(decoded.get_pixel(240, 12).0, [51, 77, 230]);
    assert_eq!(decoded.get_pixel(0, 18).0, [230, 51, 51]);
    // The VT parser stages it as one complete iTerm2 inline file.
    use sonicterm_grid::grid::Grid;
    use sonicterm_vt::vt::{CaptureStagingPool, MediaProtocol, Parser, VtEvent};
    let mut parser =
        Parser::new_with_staging_pool(Grid::new(250, 70), None, CaptureStagingPool::new());
    let media: Vec<_> = parser
        .advance(bytes)
        .into_iter()
        .filter_map(|event| match event {
            VtEvent::Media(media) => Some(media),
            _ => None,
        })
        .collect();
    assert_eq!(media.len(), 1);
    assert_eq!(media[0].protocol, MediaProtocol::Iterm2File);
}

#[test]
fn presenter_variants_set_only_their_mode() {
    // gdi forces the software presenter, wgpu forbids it, and every other config stays byte-identical.
    use sonicterm_cfg::config::{Config, SoftwareRenderMode};
    let dir = scratch_dir("presenter-config");
    let harness = r"C:\Temp\perf_scenarios.exe";
    let default_text =
        config_toml_with_shell(&plan("S1", "default", false).unwrap(), harness, false);
    assert!(!default_text.contains("[appearance]"));
    for (variant, mode, text) in [
        ("default", SoftwareRenderMode::Auto, None),
        ("role-exit", SoftwareRenderMode::Auto, None),
        ("gdi", SoftwareRenderMode::Force, Some("force")),
        ("wgpu", SoftwareRenderMode::Off, Some("off")),
    ] {
        let toml = config_toml_with_shell(&plan("S1", variant, false).unwrap(), harness, false);
        let expected = text.map_or_else(
            || default_text.clone(),
            |mode| format!("{default_text}\n[appearance]\nsoftware_render_mode = \"{mode}\"\n"),
        );
        assert_eq!(toml, expected, "{variant}");
        let path = dir.join(format!("{variant}.toml"));
        std::fs::write(&path, &toml).unwrap();
        let config = Config::load_strict(&path).unwrap();
        assert_eq!(config.appearance.software_render_mode, mode, "{variant}");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn role_exit_program_exits_1_after_go() {
    // S1/role-exit's program exits 1 as soon as GO exists, printing nothing after READY, not even an error.
    let exiting = plan("S1", "role-exit", true).unwrap();
    let dir = program_dir("role-exit", &exiting, true);
    let mut host = FakeHost::new(usize::MAX);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(run_steps(&dir, &mut host, &mut out, &mut err), 1);
    assert_eq!(out, b"READY 0\n");
    assert!(err.is_empty(), "{}", String::from_utf8_lossy(&err));
    assert_eq!((host.shells, host.sleeps.len()), (0, 0));
    assert!(!dir.join("done/0").exists());
    assert_eq!(program_steps(Workload::ExitAfterGo), [ProgramStep::ExitAfterGo]);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The running platform's name as `START_SIZE_INPUTS` rows record it.
fn platform_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        // When: neither macos nor windows matches, the platform has no recorded rows.
        "linux"
    }
}

/// One working-set measurement as the parseable row CI output is recorded from: `key=value`
/// fields after a fixed prefix, in the order `START_SIZE_INPUTS` rows are keyed, with `source`
/// naming what measured it (`helper` or `real_renderer`). `fit` is `FitOutcome::label`, which
/// the helper and the renderer's `GlyphAtlasFacts::fit` both produce. `incomplete_glyphs`
/// counts required glyphs the measurement drew as tofu; a nonzero count makes the row ineligible
/// to select a start below the maximum.
#[allow(clippy::too_many_arguments)]
fn working_set_row(
    platform: &str,
    scale: u32,
    fixture: &str,
    source: &str,
    fit: &str,
    max_tile: [u32; 2],
    packed_pixels: u64,
    incomplete_glyphs: usize,
) -> String {
    format!(
        "glyph_atlas_working_set platform={platform} scale={scale} fixture={fixture} \
         source={source} fit={fit} max_tile={}x{} packed_pixels={packed_pixels} \
         incomplete_glyphs={incomplete_glyphs}",
        max_tile[0], max_tile[1]
    )
}

/// A helper measurement's sizing figures: its fit outcome, its largest tile and how many required
/// glyphs it drew as tofu after reviewed exceptions.
type HelperFigures = (sonicterm_text::glyph_atlas::FitOutcome, [u32; 2], usize);

/// How the working-set step judges one live helper measurement against its recorded row.
#[derive(Debug, Clone, PartialEq, Eq)]
enum HelperCheck {
    /// Complete, and equal to its recorded row (or no row is recorded).
    Matches,
    /// Complete, but its sizing figures differ from the recorded row; the message names both.
    Drifted(String),
    /// Some required glyph was not drawn, so the measurement cannot validate the recorded rows or a
    /// start below the maximum; the message names the refusal and the missing evidence.
    Refused(String),
    /// Some required glyph was not drawn, but there is no recorded row and the start is already the
    /// maximum, which the conservative rule selects for an incomplete measurement anyway.
    MaximumOnly,
}

/// Judge `live` against `recorded`, the helper row recorded for the same platform, scale and
/// fixture. `start_below_maximum` says whether the shipped start at this scale is below the maximum;
/// `evidence` describes the measurement's missing glyphs for a refusal.
fn check_helper(
    recorded: Option<HelperFigures>,
    live: HelperFigures,
    start_below_maximum: bool,
    evidence: &str,
) -> HelperCheck {
    let incomplete_glyphs = live.2;
    if incomplete_glyphs > 0 {
        // When: incomplete_glyphs is nonzero the figures describe less than the working set.
        if recorded.is_some() || start_below_maximum {
            // When: a recorded row or a smaller start depends on this measurement, it is refused.
            return HelperCheck::Refused(format!(
                "incomplete measurement: {incomplete_glyphs} required glyphs were not drawn, so it \
                 cannot validate the recorded helper row or a start below the maximum; {evidence}"
            ));
        }
        return HelperCheck::MaximumOnly;
    }
    match recorded {
        Some(row) if row != live => {
            HelperCheck::Drifted(format!("recorded {row:?}, live {live:?}"))
        }
        _ => HelperCheck::Matches,
    }
}

/// Whether fallback is complete: the frame presented, and neither its terminal rows nor its
/// chrome (tab titles, palette, footer) drew a character as tofu.
fn fallback_settled(presented: bool, terminal_missing: &[char], chrome_missing: &[char]) -> bool {
    presented && terminal_missing.is_empty() && chrome_missing.is_empty()
}

/// The palette footer's key-hint symbols the coverage test requires as `PaletteFooter` tiles.
const REQUIRED_FOOTER_SYMBOLS: &str = "·↑↓↵";
/// The tab title the coverage test requires as `TabTitle` tiles; short enough never to truncate.
const REQUIRED_TITLE_TEXT: &str = "zsh";

/// Each required chrome character with no real resident tile at its raster variant, as
/// `(variant, character)`. Missing sentinels are not resident, so a symbol drawn as tofu is listed.
fn missing_required_chrome(
    resident: &std::collections::HashSet<sonicterm_types::GlyphKey>,
) -> Vec<(sonicterm_types::GlyphRasterVariant, char)> {
    use sonicterm_types::GlyphRasterVariant;
    let required = REQUIRED_FOOTER_SYMBOLS
        .chars()
        .map(|symbol| (GlyphRasterVariant::PaletteFooter, symbol))
        .chain(REQUIRED_TITLE_TEXT.chars().map(|symbol| (GlyphRasterVariant::TabTitle, symbol)));
    required
        .filter(|&(variant, symbol)| {
            !resident.iter().any(|key| key.raster_variant == variant && key.ch == symbol)
        })
        .collect()
}

/// 12c, run by CI on each platform: each start constant passes `validate_table_start`, the same
/// validation the unit tests use, and equals `ruled_start` over the recorded rows once the sizing
/// oracle is complete. For S9's and S12's working sets at scale 1 and 2 the
/// constant is at least this platform's need, where a measurement that drew any glyph as tofu
/// needs the maximum, and the live helper measurement equals this platform's recorded `helper`
/// row when one exists. Prints each figure so CI's output can become the table's rows; every
/// failure is collected and the step fails once at the end, so one run prints every row.
#[test]
#[ignore = "measures with the real font stack; CI runs it in its own step"]
fn glyph_atlas_working_set() {
    use sonicterm_gpu::glyph_working_set::measure_glyph_working_set;
    use sonicterm_text::glyph_atlas::{ATLAS_DIM, START_ATLAS_DIM_1X, START_ATLAS_DIM_2X};
    use sonicterm_text::start_size_inputs::{
        normalize_raster_failures, ruled_start, start_rule, validate_table_start, InputSource,
        RuleInput, RASTER_EXCEPTIONS, SIZING_ORACLE_COMPLETE, START_SIZE_INPUTS,
    };
    let font_dirs =
        vec![std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts")];
    // Every check below records its failure here instead of stopping, so one CI run prints every
    // row; the step fails once, after the last row, naming all of them.
    let mut failures: Vec<String> = Vec::new();
    let size: f32 = FONT_SIZE.parse().unwrap_or_else(|_| {
        failures.push(format!("the scenario font size {FONT_SIZE} is not a number"));
        13.0
    });
    let fixtures = [("S9", Fixture::EmojiCjk), ("S12", Fixture::HistoryScreen)];
    let titles = ["zsh", "perf_scenarios", "S12 covered window"];
    let platform = platform_name();
    for (scale, dpi, constant) in [(1, 72, START_ATLAS_DIM_1X), (2, 144, START_ATLAS_DIM_2X)] {
        match validate_table_start(scale, constant) {
            Ok(table) => println!(
                "glyph_atlas_start scale={scale} constant={constant} verdict={}",
                table.verdict
            ),
            Err(error) => {
                failures.push(format!("{scale}x start {constant} fails validation: {error}"))
            }
        }
        if SIZING_ORACLE_COMPLETE {
            // When: SIZING_ORACLE_COMPLETE is set, the constant must equal the recorded rows' rule.
            match ruled_start(scale, START_SIZE_INPUTS, SIZING_ORACLE_COMPLETE) {
                Ok(ruled) => {
                    println!(
                        "glyph_atlas_start scale={scale} constant={constant} ruled={} validated={}",
                        ruled.dim,
                        ruled.dim == constant
                    );
                    if constant != ruled.dim {
                        // When: the constant differs from ruled.dim, the shipped start breaks the rule.
                        failures.push(format!(
                            "{scale}x constant {constant} against the rule {}: {}",
                            ruled.dim, ruled.verdict
                        ));
                    }
                }
                Err(error) => failures.push(format!("{scale}x has no ruled start: {error}")),
            }
        }
        let mut local = Vec::new();
        for (name, fixture) in fixtures {
            let bytes = fixture_bytes(fixture);
            let text = String::from_utf8_lossy(&bytes);
            let lines: Vec<&str> = text.lines().collect();
            let set = match measure_glyph_working_set(
                &lines,
                &titles,
                FONT_FAMILY,
                size,
                dpi,
                &font_dirs,
            ) {
                Ok(set) => set,
                Err(error) => {
                    // When: the measurement errs, this fixture prints no row; the others still do.
                    failures
                        .push(format!("{platform} {name} at {scale}x did not measure: {error}"));
                    continue;
                }
            };
            // Reviewed exceptions apply here, where each failure still names its face; unresolved
            // characters and oversize required tiles always count.
            let raster = normalize_raster_failures(platform, &set.raster_failed, RASTER_EXCEPTIONS);
            let incomplete_glyphs = sonicterm_text::start_size_inputs::incomplete_glyphs(
                set.unresolved_chars.len(),
                set.oversize_required.len(),
                &raster,
            );
            println!(
                "{}",
                working_set_row(
                    platform,
                    scale,
                    name,
                    "helper",
                    &set.fit_outcome.label(),
                    set.max_tile_dims,
                    set.packed_pixels,
                    incomplete_glyphs
                )
            );
            if incomplete_glyphs > 0 || !raster.raw.is_empty() {
                // When: a required tile is tofu on this host, say which, apart from the row: the raw
                // raster failures, those a reviewed exception approved with its reason, and the rest.
                println!(
                    "glyph_atlas_working_set_tofu platform={platform} scale={scale} fixture={name} \
                     unresolved={:?} oversize_required={:?} raster_failed={} matched={:?} \
                     unapproved={:?}",
                    set.unresolved_chars,
                    set.oversize_required,
                    raster.raw.len(),
                    raster.matched,
                    raster.unapproved
                );
            }
            let recorded = START_SIZE_INPUTS.iter().find(|row| {
                row.platform == platform
                    && row.scale == scale
                    && row.fixture == name
                    && row.source == InputSource::Helper
            });
            // An incomplete measurement is refused before the drift check, with the failed face,
            // glyph, style and strike of every unapproved raster failure; it never passes.
            let evidence = format!(
                "unresolved={:?} oversize_required={:?} unapproved={:?}",
                set.unresolved_chars, set.oversize_required, raster.unapproved
            );
            let check = check_helper(
                recorded.map(|row| (row.outcome, row.max_tile, row.incomplete_glyphs)),
                (set.fit_outcome, set.max_tile_dims, incomplete_glyphs),
                constant < ATLAS_DIM,
                &evidence,
            );
            let provenance =
                recorded.map_or_else(|| "none".to_owned(), |row| row.provenance.describe());
            match check {
                HelperCheck::Matches | HelperCheck::MaximumOnly => {}
                HelperCheck::Drifted(reason) => failures.push(format!(
                    "{name} at {scale}x drifted from its recorded helper row ({provenance}): {reason}"
                )),
                HelperCheck::Refused(reason) => failures.push(format!(
                    "glyph_atlas_working_set refused: {platform} {name} at {scale}x (recorded row: \
                     {provenance}): {reason}"
                )),
            }
            // CI seals these rows only after this test returns successfully, including every drift check.
            if let Err(error) = crate::workload::glyph_oracle::export(name, dpi, &set) {
                failures.push(format!("{name}@{scale}: oracle export: {error}"));
            }
            local.push(RuleInput {
                label: format!("{platform} {scale}x {name} helper"),
                outcome: set.fit_outcome,
                max_tile: set.max_tile_dims,
                incomplete_glyphs,
            });
        }
        match start_rule(&local) {
            Ok(need) => {
                println!(
                    "glyph_atlas_working_set platform={platform} scale={scale} need={} \
                     constant={constant} verdict={}",
                    need.dim, need.verdict
                );
                if constant < need.dim {
                    // When: constant is below need.dim, this platform's working set overflows it.
                    failures.push(format!("{scale}x needs {} but starts at {constant}", need.dim));
                }
            }
            Err(error) => failures.push(format!("{scale}x has no measured need: {error}")),
        }
    }
    if !failures.is_empty() {
        // When: any check failed, the step fails once with every failure after every row.
        panic!("glyph_atlas_working_set: {} failures:\n{}", failures.len(), failures.join("\n"));
    }
}

/// One run of the working-set step always prints every row: no check panics or asserts while the
/// fixtures and scales are still being measured; every failure is collected and the step panics
/// once, after the last row, naming all of them.
#[test]
fn the_working_set_step_prints_every_row_before_its_one_panic() {
    let source = include_str!("workload_tests.rs").replace("\r\n", "\n");
    let start = source.find("fn glyph_atlas_working_set() {").expect("the step");
    let body = &source[start..];
    let body = &body[..body.find("\n}\n").expect("its end")];
    for check in ["assert!(", "assert_eq!(", ".expect(", ".unwrap()"] {
        assert!(!body.contains(check), "{check} would stop the step before every row prints");
    }
    assert_eq!(body.matches("panic!(").count(), 1, "one panic");
    let panic = body.find("panic!(").unwrap();
    let last_row = body.rfind("println!(").expect("rows are printed");
    assert!(last_row < panic, "the panic follows every printed row");
}

/// The working-set step tells an incomplete live measurement apart from sizing drift. A complete
/// measurement equal to its row matches, and one whose figures changed is drift. A measurement that
/// drew a required glyph as tofu is refused before any comparison, because its fit and largest tile
/// describe a smaller set than the working set; it is never reported as drift and never passes.
#[test]
fn an_incomplete_helper_measurement_is_refused_before_the_drift_check() {
    use sonicterm_text::glyph_atlas::FitOutcome;
    let row = (FitOutcome::Fits(1024), [23, 20], 0);
    assert_eq!(check_helper(Some(row), row, true, "none"), HelperCheck::Matches);
    let drifted = (FitOutcome::Fits(2048), [23, 20], 0);
    let HelperCheck::Drifted(reason) = check_helper(Some(row), drifted, true, "none") else {
        panic!("a complete measurement with other figures is drift");
    };
    assert!(reason.contains("Fits(2048)"), "{reason}");
    // The figures also differ here, but the missing glyphs decide: refusal, not drift.
    let incomplete = (FitOutcome::Fits(1024), [23, 16], 60);
    for (recorded, below) in [(Some(row), true), (Some(row), false), (None, true)] {
        let HelperCheck::Refused(reason) = check_helper(recorded, incomplete, below, "U+1F600")
        else {
            panic!("an incomplete measurement was not refused ({recorded:?}, below {below})");
        };
        assert!(reason.contains("60 required glyphs") && reason.contains("U+1F600"), "{reason}");
    }
    // With no recorded row and the start already the maximum, the conservative rule decides.
    assert_eq!(check_helper(None, incomplete, false, "U+1F600"), HelperCheck::MaximumOnly);
}

/// Representative S9 emoji and CJK scalars, taken from the fixture's last line, which the real
/// renderer's viewport shows at both scales (the cursor sits on the row below it). Test 17 checks
/// that each resolves to a resident real tile.
const S9_REPRESENTATIVE_CODEPOINTS: [char; 6] = ['😀', '🍣', '🌏', '漢', '字', 'あ'];

/// Every representative codepoint is drawn by S9: each appears on the fixture's last line, so the
/// frozen list cannot drift from the fixture it stands for.
#[test]
fn the_s9_representative_codepoints_are_on_the_fixtures_last_line() {
    let bytes = fixture_bytes(crate::scenarios::Fixture::EmojiCjk);
    let text = String::from_utf8(bytes).expect("the S9 fixture is UTF-8");
    let last = text.lines().rev().find(|line| !line.trim().is_empty()).expect("a last line");
    let absent: Vec<char> = S9_REPRESENTATIVE_CODEPOINTS
        .iter()
        .copied()
        .filter(|codepoint| !last.contains(*codepoint))
        .collect();
    assert!(absent.is_empty(), "not on the last line {last:?}: {absent:?}");
}

/// The working-set step and the real-renderer coverage test print their measurements through one
/// formatter, so a single `key=value` parser reads both and the rows can become `START_SIZE_INPUTS`
/// entries: the same keys in the same order, told apart only by `source`. Both kinds carry
/// `incomplete_glyphs`, the count that makes a row ineligible to select a smaller start.
#[test]
fn working_set_rows_share_one_parseable_format() {
    use sonicterm_text::glyph_atlas::FitOutcome;
    let parse = |line: &str| -> Vec<(String, String)> {
        let fields = line.strip_prefix("glyph_atlas_working_set ").expect("the row prefix");
        fields
            .split(' ')
            .map(|field| {
                let (key, value) = field.split_once('=').expect("a key=value field");
                (key.to_owned(), value.to_owned())
            })
            .collect()
    };
    let helper = parse(&working_set_row(
        "windows",
        1,
        "S9",
        "helper",
        &FitOutcome::Fits(512).label(),
        [18, 24],
        9000,
        3,
    ));
    let real = parse(&working_set_row(
        "windows",
        2,
        "S12",
        "real_renderer",
        &FitOutcome::FitsWithoutHeadroom.label(),
        [40, 48],
        123_456,
        0,
    ));
    let keys =
        |row: &[(String, String)]| row.iter().map(|(key, _)| key.clone()).collect::<Vec<_>>();
    assert_eq!(
        keys(&helper),
        [
            "platform",
            "scale",
            "fixture",
            "source",
            "fit",
            "max_tile",
            "packed_pixels",
            "incomplete_glyphs"
        ]
    );
    assert_eq!(keys(&helper), keys(&real));
    assert_eq!(helper[3].1, "helper");
    assert_eq!(real[3].1, "real_renderer");
    assert_eq!((real[4].1.as_str(), real[5].1.as_str()), ("no_headroom", "40x48"));
    assert_eq!((helper[7].1.as_str(), real[7].1.as_str()), ("3", "0"));
}

/// Fallback is complete only on a presented frame whose terminal rows and chrome both drew no
/// tofu: a footer still waiting on its fallback face keeps the check waiting even though the
/// terminal rows are complete, which a terminal-only check would have accepted.
#[test]
fn fallback_settles_only_when_rows_and_chrome_drew_no_tofu() {
    assert!(fallback_settled(true, &[], &[]));
    assert!(!fallback_settled(true, &[], &['↵']), "a footer awaiting its face is not settled");
    assert!(!fallback_settled(true, &['界'], &[]));
    assert!(!fallback_settled(false, &[], &[]), "a frame that did not present proves nothing");
}

/// The coverage test compares tiles only after the chrome it exercises is resident: every footer
/// key-hint symbol under `PaletteFooter` and the short tab title under `TabTitle`, each as a real
/// tile (missing sentinels are not resident). A symbol drawn only at another variant still counts
/// as missing.
#[test]
fn required_chrome_symbols_are_checked_per_raster_variant() {
    use sonicterm_types::{GlyphKey, GlyphRasterVariant};
    // Any real shaped key stands in for a resident tile; only the character and variant matter.
    let key = |symbol: char, variant| {
        GlyphKey::shaped(symbol, 1, 7, false, false).with_raster_variant(variant)
    };
    let mut resident: std::collections::HashSet<GlyphKey> = REQUIRED_FOOTER_SYMBOLS
        .chars()
        .map(|symbol| key(symbol, GlyphRasterVariant::PaletteFooter))
        .chain(REQUIRED_TITLE_TEXT.chars().map(|symbol| key(symbol, GlyphRasterVariant::TabTitle)))
        .collect();
    assert!(missing_required_chrome(&resident).is_empty());
    resident.remove(&key('↵', GlyphRasterVariant::PaletteFooter));
    resident.insert(key('↵', GlyphRasterVariant::Normal));
    assert_eq!(missing_required_chrome(&resident), vec![(GlyphRasterVariant::PaletteFooter, '↵')]);
}

/// Open the command palette so the main frame draws it: no window is named frontmost, which routes
/// the palette to main (no attached window). Naming the seeded main key would misroute it, since the
/// attached native window carries another id. Fails when the palette is not open on main.
fn open_palette_on_main_frame(app: &mut sonicterm_app::app::App) -> Result<(), String> {
    app.__test_set_frontmost_window(None);
    if !app.run_action(&sonicterm_cfg::keymap::Action::OpenCommandPalette) {
        // When: run_action refuses the action, the palette cannot be measured.
        return Err("the palette open action was refused".to_owned());
    }
    if !app.__test_palette_open() {
        // When: the action ran but the palette is closed, nothing draws a footer.
        return Err("the palette is not open".to_owned());
    }
    match app.__test_palette_attached_window() {
        None => Ok(()),
        // When: an attached window is named, the main frame does not draw the palette.
        Some(window_id) => {
            Err(format!("the palette attached to {window_id:?}, not the main frame"))
        }
    }
}

/// The real-renderer coverage test seeds its main window under a synthetic key and then attaches a
/// native window with another id, so naming that key frontmost reads as a torn-out child and the
/// palette attaches there: the main frame never draws it, and no footer key becomes resident. The
/// shared setup must open the palette on the main frame (no attached window), on every host.
#[test]
fn the_coverage_setup_opens_the_palette_on_the_main_frame() {
    use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
    let fresh = || {
        let mut app =
            sonicterm_app::app::App::new(Theme::default(), Config::default(), Keymap::default());
        app.__test_seed_tab("zsh");
        app
    };
    // The mechanism: the seeded main key named frontmost routes the palette away from main.
    let mut misrouted = fresh();
    let main = misrouted.__test_main_window_id().expect("a seeded main window");
    misrouted.__test_set_frontmost_window(Some(main));
    assert!(misrouted.run_action(&sonicterm_cfg::keymap::Action::OpenCommandPalette));
    assert_eq!(misrouted.__test_palette_attached_window(), Some(main));
    // The setup the coverage test uses draws it on the main frame.
    let mut app = fresh();
    open_palette_on_main_frame(&mut app).expect("the palette opens on the main frame");
    assert!(app.__test_palette_open());
    assert_eq!(app.__test_palette_attached_window(), None);
}

/// The working-set helper against the real renderer, compared by tile identity rather than by
/// configuration-local glyph keys: the S9 and S12 fixtures drawn on a Windows
/// window with the tab bar and three titles, the cursor and the command palette open with its
/// footer and detail rows, at scale 1 and 2. It lives beside the fixtures it draws.
///
/// What it proves: the completion wait needs a presented frame whose terminal rows and chrome
/// report no missing glyph, and both reports now include a shaped glyph that drew nothing (refused,
/// rasterized nothing, or too large to place) and a tab title that never shaped. Every resident
/// tile is in the helper's set at the same raster size, and for S9 each representative emoji and
/// CJK codepoint is a resident real tile. It does not prove that every glyph the helper measures
/// was drawn, only the glyphs this frame requested. These checks passed on Windows CI and their
/// rows are recorded, which is what completes the sizing oracle and lets a start constant drop
/// below 2048.
#[cfg(target_os = "windows")]
mod real_renderer_coverage {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use sonicterm_app::app::App;
    use sonicterm_cfg::config::{Config, ScrollbarMode, SoftwareRenderMode};
    use sonicterm_cfg::keymap::Keymap;
    use sonicterm_cfg::theme::Theme;
    use sonicterm_gpu::core::{GlyphAtlasStart, GpuRenderer, RendererSettings, SurfaceAppearance};
    use sonicterm_gpu::glyph_working_set::{
        measure_glyph_working_set, representative_gaps, representative_identities,
    };
    use winit::application::ApplicationHandler;
    use winit::dpi::PhysicalSize;
    use winit::event::WindowEvent;
    use winit::event_loop::{ActiveEventLoop, EventLoop};
    use winit::platform::windows::EventLoopBuilderExtWindows;
    use winit::window::{Window, WindowId};

    use super::super::{fixture_bytes, FONT_FAMILY, FONT_SIZE};
    use crate::scenarios::Fixture;

    /// The tab titles the harness measures with.
    const TITLES: [&str; 3] = ["zsh", "perf_scenarios", "S12 covered window"];

    struct Probe {
        outcome: Option<Result<(), String>>,
    }

    impl ApplicationHandler for Probe {
        fn resumed(&mut self, active: &ActiveEventLoop) {
            // winit allows one event loop per process, so every case runs inside this one.
            self.outcome = Some(run_cases(active));
            active.exit();
        }

        fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
    }

    fn check(condition: bool, message: &str) -> Result<(), String> {
        if condition {
            Ok(())
        } else {
            Err(message.to_owned())
        }
    }

    /// Whether this host enumerates no wgpu adapter at all, established apart from renderer
    /// construction. That is the only limitation that turns a failed wgpu renderer into a skip.
    fn host_has_no_adapter() -> bool {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all())).is_empty()
    }

    /// The app's font directory, so the renderer and the helper load the same faces.
    fn font_dirs() -> Vec<PathBuf> {
        vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts")]
    }

    /// A wgpu renderer (WARP on the hosted runner) at `scale` on its own window, with the tab bar
    /// on. `Ok(None)` only when the host enumerates no adapter; any other error fails.
    fn renderer(
        active: &ActiveEventLoop,
        scale: f32,
        size_pt: f32,
    ) -> Result<Option<(Arc<Window>, GpuRenderer)>, String> {
        let window = Arc::new(
            active
                .create_window(
                    Window::default_attributes()
                        .with_visible(true)
                        .with_inner_size(PhysicalSize::new(1000, 640))
                        .with_title("SonicTerm working-set coverage"),
                )
                .map_err(|error| error.to_string())?,
        );
        let dirs = font_dirs();
        let created = GpuRenderer::new(
            window.clone(),
            active,
            &Theme::default(),
            RendererSettings {
                font_family: FONT_FAMILY,
                font_dirs: &dirs,
                font_size: size_pt,
                line_height_mult: 1.2,
                font_weight_scale: 1.0,
                subpixel_aa: Default::default(),
                padding: [0.0; 4],
                appearance: SurfaceAppearance {
                    backdrop: Default::default(),
                    opacity: 1.0,
                    scrollbar: ScrollbarMode::Never,
                    panel_padding: 0.0,
                    software_render_mode: SoftwareRenderMode::Off,
                },
                role: "working-set-coverage",
                glyph_atlas_start: GlyphAtlasStart::Normal,
            },
        );
        let mut renderer = match created {
            Ok(renderer) => renderer,
            Err(error) if host_has_no_adapter() => {
                // When: the host enumerates no adapter, report the capability, not a pass.
                println!("capability=HOST_INCAPABLE case=working-set-coverage reason={error}");
                return Ok(None);
            }
            Err(error) => {
                // When: an adapter exists, so the construction error is a defect.
                return Err(format!("wgpu renderer construction failed with an adapter: {error}"));
            }
        };
        renderer.set_scale_factor(scale);
        renderer.set_tab_bar_visible(true);
        renderer.set_cursor_blink(false);
        Ok(Some((window, renderer)))
    }

    /// How long a case may wait for a presented frame whose rows and chrome draw no tofu.
    const FALLBACK_DEADLINE: Duration = Duration::from_secs(15);

    /// Dispatch the real main-window redraw with pacing open until a dispatch presents a frame
    /// (the renderer's successful-frame count advances) and that frame drew no tofu in its terminal
    /// rows (`last_missing_tofu()`) or its chrome (`last_missing_chrome()`: titles, palette rows,
    /// query and footer), so every fallback face has landed. Fails at the deadline with what was
    /// still missing.
    fn settle(
        app: &mut App,
        active: &ActiveEventLoop,
        id: WindowId,
        case: &str,
    ) -> Result<(), String> {
        let started = Instant::now();
        let mut presented_any = false;
        let mut last_missing = Vec::new();
        let mut last_chrome_missing = Vec::new();
        while started.elapsed() < FALLBACK_DEADLINE {
            let before = app
                .__test_window_renderer_mut(id)
                .map(|renderer| renderer.successful_frame_count())
                .ok_or("the window has a renderer")?;
            app.__test_set_window_last_render(id, Instant::now() - Duration::from_secs(1));
            ApplicationHandler::window_event(app, active, id, WindowEvent::RedrawRequested);
            let renderer = app.__test_window_renderer_mut(id).ok_or("the window has a renderer")?;
            let presented = renderer.successful_frame_count() > before;
            presented_any |= presented;
            last_missing = renderer.last_missing_tofu().to_vec();
            last_chrome_missing = renderer.last_missing_chrome().to_vec();
            if super::fallback_settled(presented, &last_missing, &last_chrome_missing) {
                // When: the frame presented with no row or chrome tofu, fallback is complete.
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Err(format!(
            "{case}: no presented frame without tofu within {FALLBACK_DEADLINE:?} \
             (any frame presented: {presented_any}; terminal rows still missing: \
             {last_missing:?}; chrome still missing: {last_chrome_missing:?})"
        ))
    }

    /// Test 17 for one fixture at one scale: the renderer draws the fixture, three titles, the
    /// cursor and the open palette until a presented frame draws no tofu in its rows or chrome.
    /// It then holds every required footer symbol and title character as a real tile, prints its
    /// atlas as a `source=real_renderer` row, every resident tile resolves to an identity (face,
    /// glyph, strike, variant and flags), each identity is in the helper's set with the same
    /// raster size, and for S9 each representative codepoint resolves to a resident real tile.
    fn covered(
        active: &ActiveEventLoop,
        name: &str,
        fixture: Fixture,
        scale: f32,
    ) -> Result<(), String> {
        let case = format!("{name}@{scale}");
        let size_pt: f32 = FONT_SIZE.parse().map_err(|_| "the scenario font size")?;
        let Some((window, renderer)) = renderer(active, scale, size_pt)? else {
            return Ok(());
        };
        let mut config = Config::default();
        config.font.family = FONT_FAMILY.into();
        config.font.size = size_pt;
        config.appearance.software_render_mode = SoftwareRenderMode::Off;
        config.locale = "en".into();
        let mut app = App::new(Theme::default(), config, Keymap::default());
        let bytes = fixture_bytes(fixture);
        let text = String::from_utf8_lossy(&bytes).into_owned();
        // A pty's ONLCR turns each LF into CR LF before the parser sees it.
        let terminal_bytes = text.replace('\n', "\r\n").into_bytes();
        for title in TITLES {
            let pane = app.__test_seed_tab(title);
            check(app.__test_advance_pane_parser(pane, &terminal_bytes), "fixture written")?;
        }
        let id = app.__test_main_window_id().ok_or("no main window")?;
        check(app.__test_attach_window_renderer(id, window, renderer), "renderer attached")?;
        super::open_palette_on_main_frame(&mut app).map_err(|error| format!("{case}: {error}"))?;
        settle(&mut app, active, id, &case)?;
        let renderer = app.__test_window_renderer_mut(id).ok_or("the window has a renderer")?;
        let resident = renderer.__test_resident_tile_keys();
        let absent = super::missing_required_chrome(&resident);
        check(absent.is_empty(), &format!("{case}: required chrome not resident: {absent:?}"))?;
        let facts = renderer.glyph_atlas_facts();
        // The renderer's own missing-glyph reports, which include shaped glyphs that drew nothing.
        let incomplete_glyphs =
            renderer.last_missing_tofu().len() + renderer.last_missing_chrome().len();
        print_row(&super::working_set_row(
            super::platform_name(),
            scale as u32,
            name,
            "real_renderer",
            &facts.fit,
            facts.max_tile,
            facts.packed_pixels,
            incomplete_glyphs,
        ));
        let (identities, unresolved) = renderer.__test_resident_tile_identities();
        check(
            unresolved.is_empty(),
            &format!("{case}: resident keys with no face identity: {unresolved:?}"),
        )?;
        let lines: Vec<&str> = text.lines().collect();
        let dpi = (72.0 * scale).round() as usize;
        if name == "S9" {
            // When: the fixture is S9, its representative emoji and CJK must be real resident tiles.
            let resolved = representative_identities(
                &super::S9_REPRESENTATIVE_CODEPOINTS,
                FONT_FAMILY,
                size_pt,
                dpi,
                &font_dirs(),
            );
            let gaps = representative_gaps(&resolved, &identities);
            check(
                gaps.is_empty(),
                &format!("{case}: representative codepoints not drawn: {gaps:?}"),
            )?;
        }
        // Only helper work is shared; every suite still proves its own resident identities and sizes.
        let helper = match crate::workload::glyph_oracle::load(name, dpi)? {
            Some(tiles) => tiles,
            None => {
                let measured = measure_glyph_working_set(
                    &lines,
                    &TITLES,
                    FONT_FAMILY,
                    size_pt,
                    dpi,
                    &font_dirs(),
                )
                .map_err(|error| {
                    format!("{case}: the helper measurement is incomplete: {error}")
                })?;
                crate::workload::glyph_oracle::identities(&measured.tile_identities)?
            }
        };
        let identities = crate::workload::glyph_oracle::identities(&identities)?;
        let missing: Vec<&String> =
            identities.keys().filter(|identity| !helper.contains_key(*identity)).take(20).collect();
        check(
            missing.is_empty(),
            &format!("{case}: resident tiles the helper missed: {missing:?}"),
        )?;
        let resized: Vec<(&String, [u32; 2], [u32; 2])> = identities
            .iter()
            .filter_map(|(identity, size)| {
                let measured = helper[identity];
                (measured != *size).then_some((identity, *size, measured))
            })
            .take(20)
            .collect();
        check(
            resized.is_empty(),
            &format!("{case}: raster sizes differ (identity, renderer, helper): {resized:?}"),
        )
    }

    /// Write one measurement row straight to stdout. CI runs this test without `--nocapture`, and
    /// libtest captures only the print macros, so a direct write keeps the row in the job's log.
    fn print_row(row: &str) {
        use std::io::Write;
        let mut stdout = std::io::stdout().lock();
        // A row that cannot be written loses only evidence; the coverage assertions still run.
        let _ = writeln!(stdout, "{row}").and_then(|()| stdout.flush());
    }

    fn run_cases(active: &ActiveEventLoop) -> Result<(), String> {
        let mut failures = Vec::new();
        for (name, fixture) in [("S9", Fixture::EmojiCjk), ("S12", Fixture::HistoryScreen)] {
            for scale in [1.0, 2.0] {
                if let Err(error) = covered(active, name, fixture, scale) {
                    failures.push(error);
                }
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }

    /// Once fallback completes on a presented frame, the helper's identity set covers every tile
    /// the real renderer holds for S9 and S12 with the tab bar, titles, cursor and an open
    /// palette, at scale 1 and 2, with the same raster sizes, and S9's representative emoji and CJK
    /// are resident real tiles; the module comment states what this proves and what it does not.
    #[test]
    fn the_helper_covers_the_real_renderer() {
        let event_loop =
            EventLoop::builder().with_any_thread(true).build().expect("Windows event loop");
        let mut probe = Probe { outcome: None };
        event_loop.run_app(&mut probe).expect("working-set coverage event loop");
        probe.outcome.expect("resumed runs").unwrap_or_else(|error| panic!("{error}"));
    }
}

/// The grid width the row-run fixtures are written for.
const ROW_RUN_COLS: usize = 250;

/// Cells `text` occupies: every non-ASCII character in the row-run fixtures is a wide CJK
/// character, so it takes two cells.
fn fixture_cells(text: &str) -> usize {
    text.chars().map(|character| if character.is_ascii() { 1 } else { 2 }).sum()
}

/// One expected run: `(text, bold, italic, ascii_fast)`.
type ExpectedRun = (String, bool, bool, bool);

/// The runs body row `row` (1-based screen row) must cut into at update `update`, stated from
/// the fixture contract rather than from the generator: the shaped CJK segments, the plain-digit
/// field that stays ASCII-fast, then the default-style blank padding to column 250 as its own
/// normal ASCII-fast run.
fn expected_row_runs(workload: RowRunWorkload, row: u16, update: u32) -> Vec<ExpectedRun> {
    let mut runs: Vec<ExpectedRun> = match workload {
        RowRunWorkload::Powerline => vec![
            ("用户 主机 ".to_owned(), true, false, false),
            ("项目 终端 ".to_owned(), false, false, false),
            ("分支 主 ".to_owned(), true, false, false),
            (format!("{row:02} {update:06}"), false, true, true),
        ],
        RowRunWorkload::CjkTui => {
            let names = [
                "编译器",
                "数据库",
                "网络服务",
                "缓存",
                "调度器",
                "日志",
                "索引器",
                "监视器",
                "代理",
                "队列",
                "存储",
                "认证",
            ];
            vec![
                (names[usize::from(row) % 12].to_owned(), true, false, false),
                (" 运行中 ".to_owned(), false, false, false),
                (format!("{:05}", (update * 37 + u32::from(row)) % 100_000), false, true, true),
            ]
        }
        RowRunWorkload::Unique => {
            let mut segments: Vec<ExpectedRun> = (0..4)
                .map(|segment| {
                    (format!("段{update:06}行{row:02}节{segment}"), segment % 2 == 0, false, false)
                })
                .collect();
            segments.push((format!("{row:02}"), false, true, true));
            segments
        }
    };
    let used: usize = runs.iter().map(|(text, ..)| fixture_cells(text)).sum();
    runs.push((" ".repeat(ROW_RUN_COLS - used), false, false, true));
    runs
}

/// Each row-run workload's first ten updates, fed in order through a real VT parser on a
/// 250 x 70 grid, cut every body row into exactly the contract's runs: texts with padding, bold,
/// italic and ASCII-fast flags. For S10/unique, no shaped `(text, bold, italic)` repeats within
/// any nine consecutive updates; its digit and padding runs are asserted exactly above but are
/// not part of that check.
#[test]
fn row_run_workloads_cut_into_the_contract_runs() {
    use sonicterm_grid::grid::Grid;
    use sonicterm_vt::vt::{CaptureStagingPool, Parser};
    // The body is screen rows 2 to 69, stated here rather than taken from the generator's range.
    let body_rows = 2..=69u16;
    assert_eq!(ROW_RUN_BODY_ROWS, body_rows, "the generator writes the specified body rows");
    assert_eq!(body_rows.clone().count(), 68);
    for workload in [RowRunWorkload::Powerline, RowRunWorkload::CjkTui, RowRunWorkload::Unique] {
        let mut parser =
            Parser::new_with_staging_pool(Grid::new(250, 70), None, CaptureStagingPool::new());
        let mut shaped_by_update: Vec<Vec<(String, bool, bool)>> = Vec::new();
        for update in 0..10 {
            parser.advance(&workload.update_bytes(update));
            let mut shaped = Vec::new();
            for row in body_rows.clone() {
                let runs = sonicterm_gpu::__row_shape_runs(parser.grid().row(row - 1));
                assert_eq!(
                    runs,
                    expected_row_runs(workload, row, update),
                    "{workload:?} row {row} update {update}"
                );
                shaped.extend(
                    runs.into_iter()
                        .filter(|(.., ascii_fast)| !ascii_fast)
                        .map(|(text, bold, italic, _)| (text, bold, italic)),
                );
            }
            shaped_by_update.push(shaped);
        }
        if workload == RowRunWorkload::Unique {
            for window in shaped_by_update.windows(9) {
                let keys: Vec<&(String, bool, bool)> = window.iter().flatten().collect();
                let distinct: std::collections::BTreeSet<_> = keys.iter().collect();
                assert_eq!(distinct.len(), keys.len(), "a shaped run repeats within nine updates");
            }
        }
    }
}

/// The Windows program plays S6/rest as the POSIX script does: it creates the rest target, prints
/// the URL and the target's absolute path above the sentinel, then sets the scroll region and
/// streams date lines inside it until the host stops it.
#[test]
fn run_steps_prints_the_rest_fixture_then_streams_inside_its_region() {
    let rest = plan("S6", "rest", true).unwrap();
    let dir = program_dir("rest-stream", &rest, true);
    let mut host = FakeHost::new(2);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_ne!(run_steps(&dir, &mut host, &mut out, &mut err), 0, "the fake host ends the stream");
    let target = dir.join("workload").join(REST_TARGET_NAME);
    assert!(target.exists(), "the rest target file is created before its path is printed");
    let fixture = format!(
        "READY 0\n\x1b[H\x1b[2J{REST_URL}\n{}\n\n{}\n\x1b[{}r\x1b[999;1H",
        target.display(),
        sentinel_line(0, NONCE),
        crate::scenarios::REST_REGION_TOP
    );
    let text = String::from_utf8(out).unwrap();
    assert!(text.starts_with(&fixture), "{text:?}");
    let streamed = &text[fixture.len()..];
    assert_eq!(
        streamed.lines().count(),
        3,
        "one date line before each granted sleep and the failed one"
    );
    assert!(streamed.lines().all(|line| line.ends_with("UTC 1970") || line.contains("UTC")));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Only a `status-line` plan's scratch config turns the status line on.
#[test]
fn status_line_plans_turn_the_strip_on_in_the_config() {
    let strip = crate::scenarios::plan("S1", "status-line", true).unwrap();
    let base = crate::scenarios::plan("S1", "default", true).unwrap();
    let with_strip = config_toml_with_shell(&strip, "/bin/sh", false);
    let without = config_toml_with_shell(&base, "/bin/sh", false);
    let table =
        "\n[status_line]\nenabled = true\nright = [\"health\", \"glyphs\", \"history\", \"size\", \"mem\"]\n\
                 \n[status_line.segment.input]\nheld_after_ms = 250\n";
    assert!(with_strip.ends_with(table), "{with_strip}");
    assert!(!without.contains("[status_line]"));
    // S1/status-line also keeps 10,000 history rows, so its linked history passes the byte budget,
    // and turns cursor blink off, so `program`'s present bound needs no blink slack.
    let scrollback = with_strip
        .trim_end_matches(table)
        .replace("scrollback = 10000", "scrollback = 1000")
        .replace("cursor_blink = false\n", "");
    assert_eq!(scrollback, without);
    let covered = crate::scenarios::plan("S12", "status-line", true).unwrap();
    let covered = config_toml_with_shell(&covered, "/bin/sh", false);
    assert!(covered.ends_with("\n[status_line]\nenabled = true\n"), "{covered}");
}

/// The linked history passes the grid's byte budget at the requested 250 x 40 grid and at the
/// 237 x 40 grid the macOS runner measures, with the 10,000-row limit, so S1/status-line's `print`
/// really trims rows below the row limit.
#[test]
fn linked_history_fixture_is_trimmed_by_the_byte_budget() {
    let bytes = fixture_bytes(crate::scenarios::Fixture::LinkedHistory);
    let crlf = String::from_utf8(bytes).unwrap().replace('\n', "\r\n");
    for (cols, rows) in [(GRID_COLS as u16, crate::scenarios::STRIP_SAFE_ROWS), (237, 40)] {
        let mut grid = sonicterm_grid::grid::Grid::new(cols, rows);
        grid.set_scrollback_limit(10_000);
        let mut parser = sonicterm_vt::vt::Parser::new(grid);
        parser.advance(crlf.as_bytes());
        let grid = parser.grid();
        assert!(grid.budget_trimmed_rows() > 0, "{cols}x{rows}: never trimmed");
        assert!(grid.scrollback_len() < 10_000, "{cols}x{rows}: trimmed below the row limit");
    }
}

/// A status-line run turns cursor blink off, so `program`'s present bound needs no blink slack;
/// every other plan's config is unchanged.
#[test]
fn status_line_runs_turn_cursor_blink_off() {
    let host = if cfg!(windows) { Host::Windows } else { Host::Posix };
    let status = crate::scenarios::plan_for("S12", "status-line", true, host).unwrap();
    let text = config_toml_with_shell(&status, "/bin/sh", false);
    let dir = std::env::temp_dir().join(format!("sonicterm-blink-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sonicterm.toml");
    std::fs::write(&path, &text).unwrap();
    let config = sonicterm_cfg::config::Config::load_strict(&path).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(text.contains("cursor_blink = false"), "{text}");
    assert!(!config.terminal.cursor_blink, "{text}");
    let default = crate::scenarios::plan_for("S12", "default", true, host).unwrap();
    assert!(!config_toml_with_shell(&default, "/bin/sh", false).contains("cursor_blink"));
}
