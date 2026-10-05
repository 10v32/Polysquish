# Regression corpus

`polysquish bench` squishes every model in `manifest.json` with every preset the
entry lists, records timings, output size and geometric fidelity, and compares the
numbers against `baseline.json`. It is how we notice that a change made the
pipeline slower, less accurate, or changed its output when it should not have.

```
cargo run --release -- bench --quick                       # everything tagged in the manifest
cargo run --release -- bench --quick --filter bunny,fast   # ids / tags containing these tokens
cargo run --release -- bench --preset hero --out bench_out # one preset only
cargo run --release -- bench --quick --update-baseline     # accept the new numbers
```

Results land in `bench_out/bench.json` (machine readable) and `bench_out/bench.md`
(a table), with the squished models under `bench_out/<id>/<preset>/`. Exit code 1
means at least one row regressed.

## What counts as a regression

For every `(id, preset)` row that exists in both the run and the baseline:

| metric                       | rule                                    |
| ---------------------------- | --------------------------------------- |
| `total_s` (load + pipeline)  | grew by more than `--tolerance` (15 %) and by more than 0.5 s |
| `deviation.mean`, `.p95`     | grew by more than `--tolerance` (15 %)  |
| `output.triangles`           | changed at all                          |
| pipeline error               | always a regression                     |

Deviation is the distance from every LOD0 vertex and triangle centroid to the
closest point on the *source* surface (`Bvh::closest_point`), as a fraction of the
source bounding diagonal. `source_to_result_p95` (source vertices projected onto
the result) is recorded for information but not gated, because floater and
hidden-face removal legitimately move source geometry far from the result.

Timings are noisy: CI runners differ, laptops throttle, and the UV unwrap (which
dominates `hero` runs) varies by ±25 % between identical runs. Compare like with
like (same `--quick` flag, same machine class), treat a lone timing regression with
a grain of salt, and re-run before believing it. The scheduled workflow uses
`--tolerance 0.3` for that reason. Triangle-count and deviation changes are
deterministic and are what to look at first.

## Manifest format

`manifest.json` is a JSON array. Each entry:

```json
{
  "id": "unique_snake_case",
  "source": { "url": "https://...", "sha256": "<hex>" },
  "tags": ["scan", "ai", "textured", "fast", "stress"],
  "presets": ["hero", "prop", "mobile"],
  "notes": "Where it came from and what it stresses."
}
```

A synthetic entry derives from another entry instead of a URL:

```json
"source": { "synth": { "from": "bunny", "levels": 2, "noise": 0.003, "floaters": 25 } }
```

It is built with `polysquish::synth`: `levels` midpoint subdivisions (each ×4),
noise displacement of `noise × diagonal` along the normals, procedural vertex
colours, then floaters, duplicate vertices, degenerate and flipped triangles
(`seed` defaults to 42, `paint` to true). Synthetic inputs guarantee the corpus
always has multi-million-triangle models even though we only commit a few MB.

Downloads and generated inputs are cached in `corpus/cache/` (git-ignored). Files
already present in `testdata/` are hard-linked or copied instead of re-downloaded.
A sha256 mismatch is an error, never a warning.

## Contributing real AI-generator outputs

The public scans above are stand-ins. What we really want are meshes straight out
of **Meshy, Tripo, Hunyuan3D, TRELLIS** and friends, because they have the
defects the pipeline exists to fix: floaters, inner shells, non-manifold fans,
baked-in lighting, 2M-triangle marching-cubes surfaces.

To add one:

1. **Consent and rights.** You must be the person who generated the asset and
   hold the rights the generator's terms grant you, and you must be happy for it to
   be redistributed under **CC-BY 4.0** (or a more permissive licence) as test
   data. Do not submit anything produced from a prompt that references a living
   person, a trademarked character, or someone else's artwork. Mention the
   generator, its version, and the prompt in `notes`.
2. **Host it somewhere stable** with a permanent URL (a GitHub release asset on
   your own fork or on this repository is ideal). Do not commit models to this
   repository; the corpus is a manifest, not a model store.
3. **Add the entry** to `manifest.json` with the sha256 (`sha256sum file`), tags
   (`ai`, the generator name, `textured` if it ships textures, `rigged` if it
   has a skin), the presets it should run under, and notes.
4. **Run it** once with `polysquish bench --filter <id>` and look at the output.
   If the squish is wrong, that is a bug report, not a reason to leave the entry
   out: add it and open an issue referencing the id.

Entries that take more than a few minutes on a laptop should carry the `slow`
tag so people can exclude them with `--filter fast`.

## Updating the baseline

`baseline.json` is a `bench.json` produced with `--quick`. It is **merged**, not
replaced: `--update-baseline` rewrites the rows the run produced and leaves other
rows alone, so a filtered run can refresh a single model.

Update it when:

- a deliberate change alters triangle counts or fidelity (say so in the commit),
- a new entry is added (its rows show as `NEW` until then),
- performance work makes the old timings meaninglessly pessimistic.

Do **not** update it to make a red run green without understanding why it is red.
Commit the baseline together with the code change that justifies it, and generate
it on a quiet machine with `cargo run --release -- bench --quick --update-baseline`.

The committed baseline was produced on the `fast` entries only
(`--filter fast`), so the heavier entries show as `NEW` until someone with time on
a big machine runs them.
