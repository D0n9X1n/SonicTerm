# Architecture Evolution / 架构演进

_Last reviewed against workspace version 1.1.1 on 2026-07-13._

## English

This is a living improvement roadmap, not a promise that every proposal will be
implemented. Each item is classified so observations are not mistaken for bugs:

- **Verified inconsistency** — code, tests, or maintained docs disagree.
- **Measured risk** — repository metrics or concentrated responsibility justify
  investigation, but do not prove a defect.
- **Design proposal** — a recommended direction that still needs a bounded spec,
  benchmark, or owner decision.

The current architecture already has strong seams: terminal protocol and grid
are independent of winit; the renderer consumes an explicit render model;
platform crates are relatively thin; PTY redraw handoff avoids native calls
under parser locks; and all crates have local guidance. The goal is to complete
and enforce those boundaries rather than replace them with a wholesale rewrite.

## Priority roadmap

| Priority | Status | Class | Opportunity | Evidence | Proposed boundary and migration | Risk | Success measure |
| --- | --- | --- | --- | --- | --- | --- | --- |
| P0 | Proposed | Verified inconsistency | Automate documentation consistency | Wiki had `.snoicterm`, log-level and shortcut drift; several current-state comments refer to removed paths/phases | Add a script that checks local wiki links, both language sections, known config paths/defaults, crate inventory from Cargo metadata, and stale forbidden doc paths | Low | CI fails on broken links, wrong `.sonicterm`, missing language section, or crate-count drift |
| P0 | Proposed | Design proposal | Add formatting and lint enforcement to PR CI | Local release guidance and the PR checklist ask contributors to run fmt/clippy; the current CI workflow focuses on tests and project scripts | Add a fast policy job for `cargo fmt --check` and `cargo clippy ... -D warnings`; keep platform tests separate | Low/medium because existing formatting drift may need cleanup | Formatting and clippy become required PR checks without changing runtime behavior |
| P0 | Proposed | Design proposal | Enforce supply-chain policy | `deny.toml` has explicit advisory/license/source policy but no job invokes it | Add `cargo deny check` as a separate job; review and time-bound every ignore rather than hiding it in general tests | Medium due to upstream advisory churn | Policy runs in CI; ignored advisories have owners/rationale/review dates |
| P0 | In progress | Verified inconsistency | Keep current-state source documentation accurate | This audit found app-core stub wording, removed `sonicterm-shared` paths, a nonexistent contracts doc, and inaccurate font-config/Windows local guides; the directly verified cases were corrected with the wiki update | Add a lightweight automated stale-path/history check and keep comments focused on behavior rather than completed migrations | Low | Grep and reviewer audit find no nonexistent canonical paths, removed crates, or completed migration phases presented as current behavior |
| P1 | Proposed | Design proposal | Make state ownership explicit and converge it | `AppState` mirrors scalar state while `WindowState`/`PaneTree` remain authoritative; many effects are record-only or use sentinel ids | First document an ownership matrix. Then migrate one pure topology slice at a time (for example tab identity/activation), with reducer tests and boundary adapters. Keep windows, GPU surfaces, PTYs, and native handles outside app-core | High | Each migrated slice has one authoritative model; no transient state machine for PTY writes; no sentinel window ids for migrated effects |
| P1 | Proposed | Design proposal | Decompose app orchestration by lifecycle responsibility | `sonicterm-app` is the largest crate; `app/mod.rs`, `child_window.rs`, and `window_event.rs` are large and high-churn | Preserve `App` facade, but extract cohesive owners for effect execution, window registry/lifecycle, pane process registry, and frame-input assembly. Move behavior with tests, not by cosmetic file splitting | High | Smaller change blast radius, explicit ownership, unchanged public behavior, integration tests remain green |
| P1 | Proposed | Design proposal | Decompose renderer into explicit frame stages | `gpu/core.rs` is ~6.9k lines and a major churn hotspot; it owns surface, frame assembly, caches, shaping dispatch, damage, overlays, and software branching | Keep `GpuRenderer` API stable; extract tested stages: frame snapshot/fingerprint, terminal row assembly, overlay assembly, damage plan, surface acquire/recovery, and presentation. Pass typed stage data rather than sharing a large mutable struct | High | Stage-level tests/benchmarks, smaller hot diffs, no new direct dependencies around render-model |
| P1 | Proposed | Measured risk | Define and instrument concurrency contracts | Parser locks, redraw-target locks, child locks, app state, and native callbacks interact; PTY resize errors are currently discarded because the callback returns `()`; known invariants are scattered through comments/tests | Add a concise lock/ownership table to canonical architecture; name lock-order rules; emit debug contention counters; preserve `try_lock` all-panes-or-defer semantics; evolve resize to return/log failures. Replace locks only where measurements justify it | Medium | No native calls under parser/redraw-target locks; contention and resize failures visible in debug logs; stress tests cover tear-out under output |
| P1 | Proposed | Design proposal | Expand deterministic test architecture | Strong unit tests exist, but native boundaries and Windows-only deterministic logic have uneven visibility | Add property/fuzz tests for VT/grid invariants and parser no-panic cases; golden tests for render-model/frame plans; Windows deterministic coverage or an equivalent report; explicit contract tests for state ownership migrations | Medium | Regressions fail at the lowest owning crate; platform-only pure logic is measured; coverage exclusions remain narrow |
| P1 | Proposed | Design proposal | Establish error and panic policy | `anyhow` and typed errors are mixed; production and tests contain many `unwrap`/`expect` sites, but counts alone do not identify defects | Use typed errors for reusable crate/public boundaries and contextual `anyhow` in app/binaries. Inventory only reachable production panic sites, classify startup-fatal versus recoverable, and migrate recoverable ones incrementally | Medium | Public APIs communicate failure classes; no panic on recoverable user/config/surface input; fatal startup expectations are documented |
| P1 | Proposed | Measured risk | Audit FFI and generated-code boundaries | Unsafe code is concentrated in font/native wrappers, which is appropriate but costly to review; generated bindings and vendored builds are large | Add safety checklists to FFI wrapper guides; test ownership/null/error paths; pin regeneration tool versions; compare generated diffs in dedicated PRs; keep wrapper API safe and small | Medium | Every unsafe block has local safety rationale; regeneration is reproducible; native handles remain RAII-owned |
| P2 | Decision needed | Design proposal | Decide the mux product direction | `sonicterm-mux` is a workspace member with raw replay/protocol tests, but no GUI consumer or release asset | Choose one: integrate through a documented attach transport and session model, or move/remove it from shipping workspace expectations. Do not let a future daemon silently become a second app core | High | Owner-approved product decision; if integrated, threat model/auth/session recovery tests; if retired, dependencies and policy exceptions removed |
| P2 | Decision needed | Design proposal | Decide SSH product readiness | SSH transport exists, but GUI live connection is incomplete; host-key and auth choices are not ready for a default user feature | Write a security/product spec before exposing it: known-hosts policy, agent/password/keyboard-interactive support, secret logging rules, reconnect/resize/exit semantics, and platform UX | High/security | Explicit authorization and host-key UX; end-to-end integration tests; feature status no longer ambiguous |
| P2 | Proposed | Measured risk | Reduce duplicated or vestigial seams | Render-model `Painter`, legacy GPU pipelines, two Windows software-present primitives, and duplicate block raster tile types can mislead maintainers | For each seam, prove consumers with code search, choose one canonical API, migrate tests, and remove only after platform builds. Avoid broad cleanup in one PR | Medium | One documented production path per responsibility; fewer dead abstractions without behavioral regression |
| P2 | Proposed | Design proposal | Improve release trust and asset reproducibility | macOS is ad-hoc signed/not notarized; MSI is unsigned; icon generation can remove an ungenerated taskbar asset | Add deterministic asset generation checks; preserve/generate every committed icon; evaluate Developer ID/notarization and Windows code signing as owner-controlled release work | Medium/high operational cost | Reproducible icons and packages; optional signing pipeline has secret isolation and verification |
| P2 | Proposed | Design proposal | Clarify ownership of high-risk areas | Font/FFI, renderer, native drag, and release paths have specialized constraints and churn | Add CODEOWNERS or a documented maintainer matrix and required verification checklist per boundary. Ownership is review routing, not architecture coupling | Low | Relevant specialists are requested automatically; boundary-specific checks appear in PRs |

## Recommended sequencing

### Phase 1 — make the current architecture truthful and enforceable

1. Correct stale comments and crate-local guidance.
2. Add documentation/link/config/crate-inventory checks.
3. Add fmt and clippy CI.
4. Add a separately owned cargo-deny job.
5. Record an explicit state-ownership and lock-order matrix in canonical architecture.

These changes are low-risk and make later refactors easier to review.

### Phase 2 — extract measured seams without changing behavior

1. Instrument redraw/parser contention and frame-stage timings.
2. Extract renderer frame stages behind the existing `GpuRenderer::render` API.
3. Extract app effect execution and window/pane registries behind the existing
   `App`/`ApplicationHandler` facade.
4. Add golden/property tests before moving authoritative state.

A file becoming smaller is not, by itself, success. Each extraction should
reduce shared mutable state or make an invariant mechanically testable.

### Phase 3 — converge state ownership

Move only pure, serializable topology and transition logic into `app-core`.
Recommended order:

```text
window identity registry
  -> tab identity/order/active tab
  -> pane-tree topology/focus/resize
  -> selection/search/palette summaries where useful
```

Native windows, renderers, PTY handles, parser mutexes, clipboard objects,
threads, and OS drag sessions should remain boundary resources referenced by
stable ids. Each migration removes the corresponding duplicate mirror and
record-only effect instead of adding a third state model.

### Phase 4 — decide optional subsystems

After the core boundaries are stable, make explicit product decisions for SSH
and mux. Both require more than wiring: security policy, failure semantics,
session ownership, and release support must be designed and tested.

## What not to do

- Do not rewrite all crates or merge them into one large app crate.
- Do not replace every `Mutex` because a grep count is high; measure contention.
- Do not split files without moving ownership and tests together.
- Do not move winit/wgpu/native handles into `sonicterm-app-core`.
- Do not let GPU depend directly on cfg/grid/UI around the render-model seam.
- Do not present optional SSH or mux internals as shipping features.
- Do not duplicate durable invariants between wiki pages and canonical docs;
  link to the canonical rule and explain implementation here.

## Decision record template

Before a P1/P2 architecture change, write an issue or PR design with:

1. current owner and invariant;
2. measured problem or verified inconsistency;
3. proposed owner and dependency direction;
4. migration slices and rollback point;
5. tests/benchmarks/platform checks;
6. docs and release impact;
7. completion metric.

Do not place standalone plans or audits under tracked `docs/`; durable accepted
invariants belong in `docs/ARCHITECTURE.md`, while this wiki remains the
bilingual explanatory roadmap.

## Evidence sources

| Area | Primary evidence |
| --- | --- |
| Actual dependency graph | root `Cargo.toml`, all crate `Cargo.toml`, `cargo metadata` |
| State ownership | `sonicterm-app-core/src/`, `sonicterm-app/src/app/` |
| Renderer concentration | `sonicterm-gpu/src/core.rs`, recent file churn |
| Concurrency invariants | `app/spawn_pane.rs`, `app/invariants.rs`, `docs/ARCHITECTURE.md` |
| Test/CI boundary | `.github/workflows/ci.yml`, `scripts/*.sh`, PR template |
| Supply-chain policy | `deny.toml`, Cargo.lock |
| Native/FFI boundary | font wrappers/build.rs, mac/windows crates |
| Optional features | app/io Cargo features, SSH/mux consumers, release workflow |

## 中文

_最后一次按 workspace 1.1.1 审查：2026-07-13。_

这是持续维护的改进路线图，并不承诺所有建议都会实施。每项都按证据分类，避免把观察误写成 bug：

- **已验证不一致** — 代码、测试或维护文档互相矛盾。
- **已测量风险** — 仓库指标或职责集中值得调查，但并未证明缺陷。
- **设计建议** — 推荐方向，仍需要有边界的 spec、benchmark 或 owner 决策。

当前架构已有很好的接缝：终端协议/grid 与 winit 分离；renderer 消费明确 render model；平台 crate
相对轻薄；PTY 重绘交接避免在 parser 锁内调用原生 API；每个 crate 都有本地说明。目标应是完成并
强制这些边界，而不是整体重写。

## 优先级路线图

| 优先级 | 状态 | 分类 | 机会 | 证据 | 建议边界与迁移 | 风险 | 成功指标 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| P0 | 建议 | 已验证不一致 | 自动化文档一致性 | Wiki 曾有 `.snoicterm`、日志级别和快捷键漂移；多处 current-state comment 引用已移除路径/阶段 | 新增脚本检查本地 wiki link、双语 section、已知 config 路径/default、Cargo metadata crate inventory、禁止的陈旧 doc 路径 | 低 | CI 会因 broken link、错误 `.sonicterm`、缺语言 section 或 crate 数量漂移失败 |
| P0 | 建议 | 设计建议 | 在 PR CI 中增加格式与 lint 强制 | 本地 release 指南和 PR checklist 要求 contributor 运行 fmt/clippy；当前 CI workflow 重点执行测试与项目脚本 | 新增快速 policy job 执行 `cargo fmt --check` 与 `cargo clippy ... -D warnings`；平台测试分开保留 | 低/中，现有格式漂移可能需先清理 | fmt 与 clippy 成为必需 PR check，且不改变 runtime behavior |
| P0 | 建议 | 设计建议 | 强制 supply-chain policy | `deny.toml` 有明确 advisory/license/source policy，但无 job 调用 | 新增独立 `cargo deny check` job；每个 ignore 都需要 rationale、owner、复查时间 | 中，上游 advisory 会变化 | CI 运行 policy；ignore 有明确维护责任 |
| P0 | 进行中 | 已验证不一致 | 保持 source current-state 文档准确 | 本次审查发现 app-core stub 文字、已删除 `sonicterm-shared` 路径、不存在的 contracts doc，以及错误的 font-config/Windows 本地指南；已随 Wiki 更新修正直接验证项 | 增加轻量陈旧路径/历史检查，并让 comment 描述行为而非已完成迁移 | 低 | grep/review 不再发现不存在的规范路径、已删除 crate，或把已完成迁移阶段写成当前行为 |
| P1 | 建议 | 设计建议 | 明确并收敛状态所有权 | `AppState` 镜像 scalar state，`WindowState`/`PaneTree` 仍权威；许多 effect 只记录或使用 sentinel id | 先记录 ownership matrix；然后每次迁移一个纯 topology slice，例如 tab identity/activation，并配 reducer test 与 boundary adapter；window/GPU surface/PTY/native handle 留在 app-core 外 | 高 | 每个迁移 slice 只有一个权威模型；PTY write 不再创建 transient state machine；已迁移 effect 无 sentinel window id |
| P1 | 建议 | 设计建议 | 按生命周期职责拆分 app | `sonicterm-app` 最大；`app/mod.rs`、`child_window.rs`、`window_event.rs` 大且高 churn | 保持 `App` facade，提取 effect executor、window registry/lifecycle、pane process registry、frame-input assembly；行为与测试一起移动，不做纯文件拆分 | 高 | 变更 blast radius 更小、ownership 明确、integration test 全绿 |
| P1 | 建议 | 设计建议 | 把 renderer 拆为明确帧阶段 | `gpu/core.rs` 约 6.9k 行且高 churn，同时负责 surface、组帧、cache、shaping dispatch、damage、overlay、software branch | 保持 `GpuRenderer` API，提取 frame snapshot/fingerprint、terminal row assembly、overlay assembly、damage plan、surface recovery、presentation；阶段间传 typed data | 高 | 有 stage-level test/benchmark；hot diff 变小；不绕过 render-model 添加直接依赖 |
| P1 | 建议 | 已测量风险 | 定义并观测并发契约 | parser/redraw-target/child/app/native callback 相互作用；PTY resize callback 返回 `()`，当前会丢弃 resize error；不变量散落在 comment/test | 在规范 architecture 加简短 lock/ownership 表；命名 lock order；记录 debug contention counter；保持 all-panes-or-defer `try_lock`；让 resize 返回/记录失败；只按测量结果换锁 | 中 | parser/redraw-target 锁内无 native call；debug 日志可见 contention 与 resize failure；stress test 覆盖繁忙输出下 tear-out |
| P1 | 建议 | 设计建议 | 扩展确定性测试架构 | 已有强单测，但 native boundary 与 Windows-only 确定性逻辑可见性不均 | 增加 VT/grid property/fuzz、parser no-panic、render-model/frame-plan golden、Windows deterministic coverage 或等价报告、state migration contract test | 中 | 回归在最低 owner crate 失败；平台纯逻辑有测量；coverage exclusion 保持窄 |
| P1 | 建议 | 设计建议 | 建立 error 与 panic policy | `anyhow` 与 typed error 混用；production/test 有许多 `unwrap/expect`，单纯计数不能证明问题 | reusable/public crate boundary 用 typed error，app/binary 用带上下文 `anyhow`；只 inventory 可达 production panic，区分 startup-fatal 与 recoverable，逐步迁移 | 中 | public API 表达 failure class；可恢复用户/config/surface 输入不 panic；fatal expectation 有文档 |
| P1 | 建议 | 已测量风险 | 审查 FFI 与生成代码边界 | unsafe 集中在字体/native wrapper，合理但审查成本高；生成 binding 与 vendored build 很大 | FFI guide 加 safety checklist；测试 ownership/null/error；固定 regeneration tool；生成 diff 用独立 PR；safe wrapper API 保持小 | 中 | 每个 unsafe block 有本地 safety rationale；生成可复现；native handle 由 RAII 管理 |
| P2 | 待决策 | 设计建议 | 决定 mux 产品方向 | `sonicterm-mux` 是 workspace member，有 replay/protocol test，但无 GUI consumer/release asset | 二选一：经明确 attach transport/session model 集成，或从发布预期中移动/删除；不要让 future daemon 变成第二 app core | 高 | owner 批准产品决策；若集成有 threat model/auth/session recovery test；若退役移除依赖和 policy exception |
| P2 | 待决策 | 设计建议 | 决定 SSH readiness | SSH transport 存在，但 GUI live connection 未完成；host-key/auth 不适合作默认功能 | 暴露前写 security/product spec：known-hosts、agent/password/keyboard-interactive、secret logging、reconnect/resize/exit、平台 UX | 高/安全 | 明确授权与 host-key UX；E2E integration test；feature 状态不再模糊 |
| P2 | 建议 | 已测量风险 | 减少重复/遗留 seam | render-model `Painter`、legacy GPU pipeline、两个 Windows software-present primitive、重复 block raster tile 会误导维护者 | 每个 seam 先 code search 证明 consumer，选择一个规范 API，迁移测试，再在平台 build 后删除；不要一次大清理 | 中 | 每项职责只有一条文档化生产路径，减少 dead abstraction 且无行为回归 |
| P2 | 建议 | 设计建议 | 提高 release trust 与资产可复现性 | macOS 仅 ad-hoc、未 notarize；MSI 未签名；icon generation 会删除无法重建的 taskbar asset | 增加确定性 asset generation check；保留/生成每个 committed icon；评估 owner 管理的 Developer ID/notarization 与 Windows signing | 中/高运营成本 | icon/package 可复现；可选 signing pipeline 有 secret 隔离与验证 |
| P2 | 建议 | 设计建议 | 明确高风险区域 owner | font/FFI、renderer、native drag、release 有专门约束且 churn 高 | 增加 CODEOWNERS 或 maintainer matrix，以及每边界必需验证 checklist；ownership 只用于 review routing，不制造架构耦合 | 低 | 自动请求相关 specialist；PR 包含边界专属检查 |

## 推荐顺序

### 第一阶段 — 让当前架构真实且可强制

1. 修正陈旧 comment 与 crate-local guide。
2. 添加文档/link/config/crate inventory 检查。
3. 添加 fmt 与 clippy CI。
4. 添加独立 owner 的 cargo-deny job。
5. 在规范 architecture 记录明确 state ownership 与 lock order matrix。

这些修改风险低，会显著降低后续 refactor 的审查成本。

### 第二阶段 — 不改变行为地提取可测接缝

1. 观测 redraw/parser contention 与 frame-stage timing。
2. 在现有 `GpuRenderer::render` API 后提取 renderer frame stage。
3. 在现有 `App`/`ApplicationHandler` facade 后提取 app effect executor 与 window/pane registry。
4. 移动权威状态前增加 golden/property test。

文件变小本身不是成功；每次提取都应减少共享可变状态，或使某个不变量可机械测试。

### 第三阶段 — 收敛状态所有权

只把纯、可序列化 topology 与 transition logic 移入 `app-core`。推荐顺序：

```text
window identity registry
  -> tab identity/order/active tab
  -> pane-tree topology/focus/resize
  -> 有价值的 selection/search/palette summary
```

原生 window、renderer、PTY handle、parser mutex、clipboard object、thread、OS drag session 应留作 boundary resource，
仅以稳定 id 引用。每次迁移应删除对应 duplicate mirror 与 record-only effect，而不是增加第三套 state model。

### 第四阶段 — 决定可选 subsystem

核心边界稳定后，再明确决定 SSH 与 mux。两者都不只是接线，还需要 security policy、failure semantics、
session ownership 和 release support 的设计与测试。

## 不要做什么

- 不要整体重写全部 crate，也不要合并为一个巨大 app crate。
- 不要因为 grep 到很多 `Mutex` 就全部替换，应先测 contention。
- 不要只拆文件而不一起移动 ownership 和 test。
- 不要把 winit/wgpu/native handle 移入 `sonicterm-app-core`。
- 不要让 GPU 绕过 render-model 直接依赖 cfg/grid/UI。
- 不要把可选 SSH 或 mux 内部实现写成已发布功能。
- 不要在 wiki 与规范 docs 之间重复 durable invariant；应链接规范规则，在 wiki 解释实现。

## 决策记录模板

每个 P1/P2 架构改动开始前，在 issue 或 PR design 中记录：

1. 当前 owner 与 invariant；
2. 测量问题或已验证不一致；
3. 建议 owner 与 dependency direction；
4. migration slice 与 rollback point；
5. test/benchmark/platform check；
6. docs 与 release 影响；
7. 完成指标。

不要把 standalone plan/audit 跟踪在 `docs/`；已接受的 durable invariant 写入 `docs/ARCHITECTURE.md`，
本 Wiki 保留双语解释性路线图。

## 证据来源

| 区域 | 主要证据 |
| --- | --- |
| 实际依赖图 | 根 `Cargo.toml`、各 crate `Cargo.toml`、`cargo metadata` |
| 状态所有权 | `sonicterm-app-core/src/`、`sonicterm-app/src/app/` |
| Renderer 职责集中 | `sonicterm-gpu/src/core.rs`、近期文件 churn |
| 并发不变量 | `app/spawn_pane.rs`、`app/invariants.rs`、`docs/ARCHITECTURE.md` |
| Test/CI 边界 | `.github/workflows/ci.yml`、`scripts/*.sh`、PR template |
| Supply-chain policy | `deny.toml`、Cargo.lock |
| Native/FFI 边界 | font wrapper/build.rs、mac/windows crate |
| 可选 feature | app/io Cargo feature、SSH/mux consumer、release workflow |
