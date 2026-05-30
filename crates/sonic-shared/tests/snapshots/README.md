# visual_snapshot baselines

Per-scene dHash baselines for the renderer regression test in
`crates/sonic-shared/tests/visual_snapshot.rs`.

Each `.hash` file holds the hex dHash of the rendered RGBA for one
payload (ASCII, CJK, emoji, ligature, powerline). The test fails if a
current render's dHash differs from baseline by more than the allowed
hamming-bit threshold.

## Refresh history

| Date       | Commit SHA (parent) | Reason                                                    |
|------------|---------------------|-----------------------------------------------------------|
| 2026-05-29 | 4ca26d2             | Re-bake after #284 P0 glyph-blur fix (closes #283).       |

## How to refresh

If a rendering change is intentional:

```bash
UPDATE_SNAPSHOTS=1 cargo test -p sonic-shared --test visual_snapshot
```

Then commit the changed `.hash` files in the same PR that changes the
renderer, and append a row to the table above with the date + parent
SHA + reason.

If the test fails unexpectedly, treat it as a render regression first —
see `scripts/check-visual-snapshots.sh` for guidance and PR #284 for the
P0-class bug this gate exists to catch.
