//! Pins the generated role scripts, fixture bytes, nonce choice and scratch configuration.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use super::*;
use crate::scenarios::{all_plans, plan, Workload};

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
while [ ! -e "$scratch/go/$role" ]; do sleep 0.05; done
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
    ("S1 default full", "edbe606ddd384910eb1da472407c41ea22dfbe76e4328017a831cc47042f83c6"),
    ("S1 default short", "e0f2554d5d2cd7cd9db6a15f9fa3086652357ac3088181b86996174afe2e1359"),
    ("S10 default full", "cb07c1e367527e027e25f645d99ae0439812691e70f261ed83d15027dc028e3a"),
    ("S10 default short", "81166f7998126b6f6e542b38c2f35bc713e3d4160bacc1642fc39c6de0e56da6"),
    ("S10 sync full", "4f63a578b7af98adfb1119e792ce2df8f03887829f4df676d3102cd7f41b74fb"),
    ("S10 sync short", "52660b862b19d2e3b5f67e7fc1b3ca7622675dd9fdfe52d9ef71c9aa48dcb3f2"),
    ("S11 default full", "da59aee4c2d1ff1e729e54ec606c4f026a1b612937eb53d67a26a14e7fe1dfb9"),
    ("S11 default short", "34d256b25f0f1253a97b8df5e7aafc5fa8b64e9d5ad00ccc7acdd9d59b0ef63d"),
    ("S12 default full", "7a95a0b1b682cc2c75c9f198861af487908e966af93abfc1ee800e375b3ee433"),
    ("S12 default short", "12e58739e0e173d693595f72b1915631fe532349fbb3e33b726d372e36c88a77"),
    ("S2 default full", "910c0fc83d8cdae5a3feb69d07809cfceeda5cd3501371fd89f2be6e654d5e23"),
    ("S2 default short", "cda27e9cdf2161c513c001158abe51c1206e5a309e5e1872d697a242c7b0de68"),
    ("S2 flood full", "62ac538f1cf8f4356e8a874bdef2347ac5171181548e5103616249c109ea4930"),
    ("S2 flood short", "288286f34676da844e89fed17db41eae96928d2ae6f347771bc5b5ff0e9795d4"),
    ("S3 default full", "513dafbfef040419cdfa6e2209b6744ce83eb8d4336aabcce7754c308c7860d5"),
    ("S3 default short", "d9d2342dd97d3ea62ed9bd70bc81fee24f1f3f5321a5b0bff57c55bfdc488898"),
    ("S4 default full", "81626dd9100d6ec6b28cd9e9d11802114cd3e34b4cf8a76ecdaf7fcded2f5869"),
    ("S4 default short", "288fd54a708ac09e0950a62e3d4f9c1728aa97cf05b358343e80f1a8e9aa7c1c"),
    ("S5 default full", "485406d66b686f62a675e60b6a564af1afc580680dc113be284307a92dac8616"),
    ("S5 default short", "2c05c242f02ca5c2499847c9563daeef4d330f4bb0f60089ea40bf5950b75498"),
    ("S6 default full", "ddb6609874ec7c01eb39d22445ef809132e49673ffd098adaab478f70372aa83"),
    ("S6 default short", "263ae81928978b6938dac22c16822ade0f26812fcc9012649fe35c5fb72d2bd9"),
    ("S6 flood full", "ae12beb3c10a444d36001f20ac30ed973bdd3a2f9f9de740cb7983dc1b9a79cd"),
    ("S6 flood short", "7b4d3320fd201f1cca9e93a9545b59ad8aab8fcc02db8138e2868c629704bfeb"),
    ("S6 selection-drag full", "a0d20c1c6bb05037839b82ddeb24362373923b0498628bc922ae523856ac7e85"),
    ("S6 selection-drag short", "febef4c45ea35262cf1f37bd9a1e6dea37310d985b80f8ceb2472f6c73edaf1b"),
    ("S7 default full", "f253827bf744454cb31bbe9bb2587b2df13f4887073ecfcb197505e6c229e6ac"),
    ("S7 default short", "cd05c9831c7270f57d413360092d364de9906227dc9591095f4788bcfda4c1f2"),
    ("S8 default full", "334db98046ae2d64f3f93fa052c0b0b5c7aa1b5e4ee851f27522b12342138099"),
    ("S8 default short", "a1f91422f69ef712ffb2a41b425eefa93d21f05476212070d23db99c7acc0c76"),
    ("S9 default full", "b3c97a61dabacae2112ab69765ca76a873321684f47ebc81ef2a20cfd5d320bb"),
    ("S9 default short", "e94f518867d49a6bdeaf72a020549c15cda569e58319562c48f61aca51cb5035"),
];

/// SHA-256 of every distinct fixture, keyed by file and length or by frame set.
const FIXTURE_PINS: &[(&str, &str)] = &[
    ("bulk.txt 5242880", "1c45218ab8edc05eb5964253bffd8809540e4b0f0e19c3918b821be59818a5c7"),
    ("bulk.txt 52428800", "6329aefe7c851b9410cf38309a0c5279e508b928f5cbb86610c8767f0b5f6d7e"),
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
        for plan in all_plans() {
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
    let image = plan("S11", "default", false).unwrap();
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
    for (scenario, laps, level, scrollback) in
        [("S1", false, LogLevel::Info, 1_000), ("S7", true, LogLevel::Debug, 10_000)]
    {
        let path = dir.join("sonicterm.toml");
        std::fs::write(
            &path,
            config_toml(&plan(scenario, "default", false).unwrap(), scratch, laps),
        )
        .unwrap();
        let config = sonicterm_cfg::config::Config::load_strict(&path).unwrap();
        assert_eq!((config.window.cols, config.window.rows), (250, 70));
        assert_eq!((config.font.family.as_str(), config.font.size), ("Rec Mono St.Helens", 13.0));
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
    for original in all_plans() {
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
    let posix = "[window]\ncols = 250\nrows = 70\n\n\
                 [font]\nfamily = \"Rec Mono St.Helens\"\nsize = 13.0\n\n\
                 [logging]\nlevel = \"info\"\n\n\
                 [terminal]\nshell = '/tmp/perf-test/workload/role.sh'\nscrollback = 1000\n";
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
