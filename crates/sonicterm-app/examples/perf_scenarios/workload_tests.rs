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
    ("S1 gdi full", "922fa48f7c0b81b136e107ede7f96feea700eaf36fb7cafe41e880ec913d4d5d"),
    ("S1 gdi short", "52697ce9881d23f19d5935e6e7f9b27d4b77ac6fcdacdb025045cb1dc22e7d67"),
    ("S1 role-exit full", "a5601180eb49ced355e02fa921b73910cd4cc32c19a5039a81614d446b5a6869"),
    ("S1 role-exit short", "0d41d14acc6768f69ad644df2a253a8eb87f4d470a4811d96212fd997b7622f5"),
    ("S1 wgpu full", "3229137b759764a6611e1940c8ed64e39b550d110d85962dce99ca4f23c1778d"),
    ("S1 wgpu short", "b1ca245149ddb1e11f30f8756495b95cea27e16b600e9cb7cca6622fd9ae5a39"),
    ("S10 default full", "cb07c1e367527e027e25f645d99ae0439812691e70f261ed83d15027dc028e3a"),
    ("S10 default short", "81166f7998126b6f6e542b38c2f35bc713e3d4160bacc1642fc39c6de0e56da6"),
    ("S10 sync full", "4f63a578b7af98adfb1119e792ce2df8f03887829f4df676d3102cd7f41b74fb"),
    ("S10 sync short", "52660b862b19d2e3b5f67e7fc1b3ca7622675dd9fdfe52d9ef71c9aa48dcb3f2"),
    ("S11 default full", "da59aee4c2d1ff1e729e54ec606c4f026a1b612937eb53d67a26a14e7fe1dfb9"),
    ("S11 default short", "34d256b25f0f1253a97b8df5e7aafc5fa8b64e9d5ad00ccc7acdd9d59b0ef63d"),
    ("S11 gdi full", "aa97aa1d01ffff0062e2f32c67169d1c405e0c77b00c1051e3ab0065387f625e"),
    ("S11 gdi short", "a30182709d9b7ed043797cf10123c302f807d41a1d5b76e8aa82af55bd1cbd76"),
    ("S11 release full", "925eaff1dcf090e5b38acbcdfc9b0e1c03068f00af9adaa6b2587dd2049f32d2"),
    ("S11 release short", "a33cfaee240d3655fcd53a45955586f285727978b4b0944e542164f4620be05c"),
    ("S11 wgpu full", "f97598ad46bf4a8e0c7459f94e84497b0ef6b0a7f135f6807bd4c85593ec1258"),
    ("S11 wgpu short", "a34fc1428ba1444d714aa603ed670d7c88e3c852a51fbc6bb1997b80392fb47d"),
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
    ("S5 gdi full", "fb1b8179379a270c46a216c871108568e5cbc20c2c74ae9dab28a5495d6626ee"),
    ("S5 gdi short", "5c693b8e732554e32b9e5b07966995e54202ffe351635d902c8b58e684cfe98d"),
    ("S5 wgpu full", "b7068fe3a6572e5a9832fd8c77fd16c9c2552683ef0b7f497f0d932369b7e686"),
    ("S5 wgpu short", "263c5c026fa8af5256a109b3bdc449e2cb84530b20c209f3a4d6bce7393c97f0"),
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
    ("inline.osc 461289", "06c2d2935c10dfc293a38ec38149c97a1e97c90501694c5e75cb14d904476f2c"),
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
        Workload::DateLoop => panic!("a date loop's output depends on the clock"),
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
/// the helper and the renderer's `GlyphAtlasFacts::fit` both produce.
fn working_set_row(
    platform: &str,
    scale: u32,
    fixture: &str,
    source: &str,
    fit: &str,
    max_tile: [u32; 2],
    packed_pixels: u64,
) -> String {
    format!(
        "glyph_atlas_working_set platform={platform} scale={scale} fixture={fixture} \
         source={source} fit={fit} max_tile={}x{} packed_pixels={packed_pixels}",
        max_tile[0], max_tile[1]
    )
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

/// 12c, run by CI on each platform: for S9's and S12's working sets at scale 1 and 2, the start
/// constant is at least this platform's need, and the live helper outcome equals this platform's
/// recorded `helper` row. With no recorded row the constant must be the maximum: no savings are
/// claimed without a measurement. Prints each figure so CI's output can become the table's rows.
#[test]
#[ignore = "measures with the real font stack; CI runs it in its own step"]
fn glyph_atlas_working_set() {
    use sonicterm_gpu::glyph_working_set::measure_glyph_working_set;
    use sonicterm_text::glyph_atlas::{ATLAS_DIM, START_ATLAS_DIM_1X, START_ATLAS_DIM_2X};
    use sonicterm_text::start_size_inputs::{
        start_rule, InputSource, RuleInput, START_SIZE_INPUTS,
    };
    let font_dirs =
        vec![std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts")];
    let size: f32 = FONT_SIZE.parse().expect("the scenario font size");
    let fixtures = [("S9", Fixture::EmojiCjk), ("S12", Fixture::HistoryScreen)];
    let titles = ["zsh", "perf_scenarios", "S12 covered window"];
    let platform = platform_name();
    for (scale, dpi, constant) in [(1, 72, START_ATLAS_DIM_1X), (2, 144, START_ATLAS_DIM_2X)] {
        let mut local = Vec::new();
        for (name, fixture) in fixtures {
            let bytes = fixture_bytes(fixture);
            let text = String::from_utf8_lossy(&bytes);
            let lines: Vec<&str> = text.lines().collect();
            let set =
                measure_glyph_working_set(&lines, &titles, FONT_FAMILY, size, dpi, &font_dirs)
                    .expect("the packaged scenario family loads");
            println!(
                "{}",
                working_set_row(
                    platform,
                    scale,
                    name,
                    "helper",
                    &set.fit_outcome.label(),
                    set.max_tile_dims,
                    set.packed_pixels
                )
            );
            if !set.unresolved_chars.is_empty() || !set.raster_failed.is_empty() {
                // When: a required tile is tofu on this host, say which, apart from the row.
                println!(
                    "glyph_atlas_working_set_tofu platform={platform} scale={scale} fixture={name} \
                     unresolved={:?} raster_failed={:?}",
                    set.unresolved_chars, set.raster_failed
                );
            }
            let recorded = START_SIZE_INPUTS.iter().find(|row| {
                row.platform == platform
                    && row.scale == scale
                    && row.fixture == name
                    && row.source == InputSource::Helper
            });
            match recorded {
                Some(row) => {
                    assert_eq!(
                        (row.outcome, row.max_tile),
                        (set.fit_outcome, set.max_tile_dims),
                        "{name} at {scale}x drifted from its recorded helper row ({})",
                        row.run_url
                    );
                }
                None => {
                    // When: no helper row is recorded for this input, savings need a measurement.
                    assert_eq!(constant, ATLAS_DIM, "{name} at {scale}x has no recorded row");
                    println!("  no recorded helper row: maximum selected, no savings");
                }
            }
            local.push(RuleInput {
                label: format!("{platform} {scale}x {name} helper"),
                outcome: set.fit_outcome,
                max_tile: set.max_tile_dims,
            });
        }
        let need = start_rule(&local).expect("two measured inputs");
        println!(
            "glyph_atlas_working_set platform={platform} scale={scale} need={} constant={constant} \
             verdict={}",
            need.dim, need.verdict
        );
        assert!(constant >= need.dim, "{scale}x needs {} but starts at {constant}", need.dim);
    }
}

/// The working-set step and the real-renderer coverage test print their measurements through one
/// formatter, so a single `key=value` parser reads both and the rows can become `START_SIZE_INPUTS`
/// entries: the same keys in the same order, told apart only by `source`.
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
    ));
    let real = parse(&working_set_row(
        "windows",
        2,
        "S12",
        "real_renderer",
        &FitOutcome::FitsWithoutHeadroom.label(),
        [40, 48],
        123_456,
    ));
    let keys =
        |row: &[(String, String)]| row.iter().map(|(key, _)| key.clone()).collect::<Vec<_>>();
    assert_eq!(
        keys(&helper),
        ["platform", "scale", "fixture", "source", "fit", "max_tile", "packed_pixels"]
    );
    assert_eq!(keys(&helper), keys(&real));
    assert_eq!(helper[3].1, "helper");
    assert_eq!(real[3].1, "real_renderer");
    assert_eq!((real[4].1.as_str(), real[5].1.as_str()), ("no_headroom", "40x48"));
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
    use sonicterm_gpu::glyph_working_set::{measure_glyph_working_set, TileIdentity};
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

    /// The packaged scenario fonts, so the renderer and the helper load the same faces.
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
    /// glyph, strike, variant and flags), and each identity is in the helper's set with the same
    /// raster size.
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
        print_row(&super::working_set_row(
            super::platform_name(),
            scale as u32,
            name,
            "real_renderer",
            &facts.fit,
            facts.max_tile,
            facts.packed_pixels,
        ));
        let (identities, unresolved) = renderer.__test_resident_tile_identities();
        check(
            unresolved.is_empty(),
            &format!("{case}: resident keys with no face identity: {unresolved:?}"),
        )?;
        let lines: Vec<&str> = text.lines().collect();
        let dpi = (72.0 * scale).round() as usize;
        let helper =
            measure_glyph_working_set(&lines, &TITLES, FONT_FAMILY, size_pt, dpi, &font_dirs())
                .map_err(|error| {
                    format!("{case}: the helper measurement is incomplete: {error}")
                })?;
        let missing: Vec<&TileIdentity> = identities
            .keys()
            .filter(|identity| !helper.tile_identities.contains_key(*identity))
            .take(20)
            .collect();
        check(
            missing.is_empty(),
            &format!("{case}: resident tiles the helper missed: {missing:?}"),
        )?;
        let resized: Vec<(&TileIdentity, [u32; 2], [u32; 2])> = identities
            .iter()
            .filter_map(|(identity, size)| {
                let measured = helper.tile_identities[identity];
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
    /// palette, at scale 1 and 2, with the same raster sizes.
    #[test]
    fn the_helper_covers_the_real_renderer() {
        let event_loop =
            EventLoop::builder().with_any_thread(true).build().expect("Windows event loop");
        let mut probe = Probe { outcome: None };
        event_loop.run_app(&mut probe).expect("working-set coverage event loop");
        probe.outcome.expect("resumed runs").unwrap_or_else(|error| panic!("{error}"));
    }
}
