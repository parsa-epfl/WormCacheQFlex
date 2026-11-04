# CLAUDE.md — WormCacheQFlex/

This is a **submodule** of the QFlex simulator. The parent repo lives at `..`; its
[CLAUDE.md](../CLAUDE.md) explains the four-phase pipeline and `ExperimentContext`.
This file covers only what's specific to *WormCacheQFlex/*.

## What this submodule is

A **Rust QEMU plugin** (`libworm_cache.so`, cdylib+rlib) that models long-term
microarchitectural state — caches, TLBs, branch predictor — during the
**functional warming phase**. It periodically serializes that warm state to disk
as JSON checkpoints; those are later consumed on the gem5/QPoints conversion path
and, historically, by Flexus at the start of the timing phase.

Loaded by [../parallel-qemu/](../parallel-qemu/) via the standard QEMU `-plugin`
flag during `fw`/`initialize`/`test-worm`. **Not** loaded into the timing `qemu/`
and not part of Flexus itself.

## Role in QFlex, including gem5

```
parallel-qemu  --plugin lib/libworm_cache.so,mode=...   → warms cache/TLB/BP
                                                         → emits checkpoint JSON per snapshot
                                                         → gem5 path: QPoints conversion reads it directly
                                                         → Flexus path: folded into flexus_configuration.json
```

This repo is well ahead of generic "long-term state warmer" — it now carries real
gem5-restore-oriented logic, which is the reason this submodule matters to the
gem5-integration work described in `../docs/gem5_conversion/`:

- `src/components/bp/fetch/bbl_btb.rs` implements a **basic-block-granularity BTB
  view** (`BblBTBEntry { bbl_start, branch_pc, ... }`) alongside the normal
  per-PC BTB (`src/components/bp/fetch/btb.rs`). This is the "gem5-oriented
  basic-block BTB view" referenced in `../docs/gem5_conversion/project_progress.md`
  and exposed on the qflex CLI as `--collect-gem5-bbl-btb` (on `initialize`, `fw`,
  `test-worm`).
- `src/checkpoint/frontend.rs::process_frontend` folds both BTB views into the
  checkpoint output (`FlexusBTBEntry`, using `bbl_bytes_by_pc` derived from the
  BBL-BTB data).
- gem5 mode is the compile-time constant `GEM5_COMPATIBILITY` in `parameter.rs`,
  rendered from the experiment's `--emit-gem` option. When on, the plugin emits
  the `.gem` bundle at each periodic snapshot (`src/chronic/mod.rs`), and
  `OrdinaryMMU` runs a separate L1 ITLB/DTLB model with real page sizes
  (`mmu/gem5_tlb.rs`), written to `gem5-tlbs-N.json.zstd` for gem5 TLB restore.

## Layout

| Path | Purpose |
|---|---|
| `src/lib.rs` | Plugin entry points: `qemu_plugin_version`, `vcpu_tb_trans`, `savevm_cb`. |
| `src/parameter.rs` | Compile-time constants (core count, cache/TLB dims). **Regenerated per experiment** from `../templates/parameter.rs.j2` by `ExperimentContext.get_wormcache_build_commands()` (`../commands/config.py`) — the checked-in file is just a default. |
| `src/plugin/` | Wrappers around QEMU's plugin C API. |
| `src/components/` | Functional models: `cache_hierarchy/` (with `parallel_hierarchy/` and `single_cache_hierarchy/` variants, MMU/TLB in `cache_hierarchy/mmu/`), `bp/` (branch predictor: `fetch/btb.rs`, `fetch/tage.rs`, `fetch/bbl_btb.rs`), `wfi/`, `touch_once/`, `trace/`, `instruction_frequency/`, `pw_log/` (page-walk logging). |
| `src/checkpoint/` | Serializes warm state to JSON: `cache_hierarchy/` (incl. `shared_cache.rs`), `frontend.rs` (BTB/TAGE/BBL-BTB), `mmu.rs`. `FlexusParameter`/`FlexusDirectoryType`/`FlexusSTLBInclusion` in `checkpoint/mod.rs` define the checkpoint's machine-contract shape. |
| `src/chronic/` | Periodic-snapshot driver. `mod.rs::chronic_behavior_init` parses plugin args; `snapshot.rs` handles the QEMU external-snapshot format selection. |
| `src/qemu_api.rs` | bindgen wrappers around QEMU's plugin C API + snapshot-format constants. |
| `src/arch/` | Arch-specific logic — **aarch64 only**. |
| `src/debug/statistics.rs` | Runtime stats/debug output. |
| `plugin_helper/` | Cargo workspace member (plugin registration/IPC helpers). |
| `src/bin/` | Auxiliary binaries (e.g. `checkpoint_conversion`, referenced by the parent's per-experiment build/copy step). |

Cargo edition 2024. Key deps: `serde`/`serde_json`/`serde_with` (checkpoint
serialization), `dashmap` (concurrent per-core state), `bitvec`, `zstd`
(checkpoint compression), `perf-event`, `hdrhistogram`, `rustc-hash`. `profile.release`
keeps debug symbols (`debug = true`).

## Build

`cargo build --release`, run **per-experiment**, not at repo-build time — see
`../CLAUDE.md` and `ExperimentContext.get_wormcache_build_commands()` in
`../commands/config.py`. Output `libworm_cache.so` is copied into the
experiment's `lib/`.

## Public interface: plugin arg format

```
-plugin lib/libworm_cache.so,mode={normal|warm|ff},init_threshold=N,interval=N,count=N,prefix="...",init_index=N
```

- `mode` (read in `src/chronic/mod.rs`): `normal` (default, no periodic
  snapshots) vs `warm`/`ff` (periodic snapshots at `interval`-tick granularity,
  up to `count` snapshots, named `<prefix>_N`).
- Separately, `src/components/cache_hierarchy/parallel_hierarchy/mod.rs` reads its
  own `mode=pure_fill` value (cold-fill warming from a previous snapshot,
  controlled by `prefix`/`warm_ratio`/`fallback_cycles`) — **this is a different
  arg namespace read by a different component**, not an alternate value of the
  chronic `mode`. When in doubt, grep the value in both places.
- Snapshot format constants live in `src/qemu_api.rs`
  (`QEMU_PLUGIN_SNAPSHOT_FORMAT_{EXTERNAL_ZSTD,EXTERNAL_INCREMENTAL_BASE,EXTERNAL_INCREMENTAL_DELTA}`),
  selected in `src/chronic/snapshot.rs`.

### Checkpoint output

JSON, optionally zstd-compressed, one file per sampling unit. Carries cache
hierarchy state (`checkpoint/cache_hierarchy/`), TLB/MMU state (`checkpoint/mmu.rs`),
and frontend state (`checkpoint/frontend.rs`: per-PC BTB, TAGE, and the
BBL-granularity BTB view used by gem5 restore).

## Conventions and gotchas

- **`parameter.rs` is regenerated per experiment** — don't treat the checked-in
  values as authoritative; `../templates/parameter.rs.j2` is the source.
- **Broken symlink:** `qemu-plugin.h` at the repo root is a symlink to
  `/home/xusine/paraflex/qemu/include/qemu/qemu-plugin.h` — a specific
  developer's home directory, which does not exist on this machine (confirmed
  dangling). bindgen reads through it for `src/qemu_api.rs` generation. If you
  need to regenerate bindings, repoint the symlink at this repo's actual QEMU
  plugin header (`../parallel-qemu/include/qemu/qemu-plugin.h`) first.
- **Two unrelated things are both spelled `mode=`** — the chronic driver's
  `mode={normal,warm,ff}` and the cache hierarchy's `mode=pure_fill` are read by
  different modules from the same plugin-arg string. Don't assume one controls
  the other.
- **aarch64 only** (`src/arch/`) — nothing here is portable to other guest ISAs
  without new arch-specific work.

## See also

- [../CLAUDE.md](../CLAUDE.md) — qflex root: four-phase pipeline, `ExperimentContext`, `--collect-gem5-bbl-btb`/`--emit-gem` CLI flags.
- [../docs/gem5_conversion/project_progress.md](../docs/gem5_conversion/project_progress.md) — why the BBL-BTB frontend view exists and how it fits gem5 restore.
- [../parallel-qemu/CLAUDE.md](../parallel-qemu/CLAUDE.md) — the fast QEMU that loads this plugin via `-plugin` during functional warming.
- [../flexus/CLAUDE.md](../flexus/CLAUDE.md) — historically consumed this plugin's checkpoints at the start of the timing phase.
- [../qemu/CLAUDE.md](../qemu/CLAUDE.md) / [../qemu/middleware/CLAUDE.md](../qemu/middleware/CLAUDE.md) — timing QEMU and the QEMU↔Flexus IPC shim (does not load this plugin).
