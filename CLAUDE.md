# CLAUDE.md — WormCacheQFlex/

@MULTI_NODE.md

This is a **submodule** of the QFlex simulator. The parent repo lives at `..`; its [CLAUDE.md](../CLAUDE.md) explains the four-phase pipeline, sampling vocabulary (population / sample / sampling unit), and `ExperimentContext`. Read it for context; this file only describes what's specific to *WormCacheQFlex/*.

## What this submodule is

A **Rust QEMU plugin** (`libworm_cache.so`, cdylib) that models long-term microarchitectural state — caches, TLBs, branch predictor — during the **functional warming phase**. On savevm events, it serialises that warm state to disk; those checkpoints are later read by [Flexus](../flexus/) at the start of the timing phase so long-lived µarch state survives the QEMU swap.

It is loaded by [parallel-qemu](../parallel-qemu/) (the "fast" QEMU) via the standard QEMU `-plugin` flag during phases 2–3. It is **not** loaded into the timing QEMU and is **not** part of Flexus.

## Role in QFlex

```
parallel-qemu  --plugin lib/libworm_cache.so,...   →  warms cache/TLB/BP
                                                  →  emits per-sampling-unit checkpoints
                                                  →  ingested into flexus_configuration.json
                                                  →  loaded by Flexus in the timing phase
```

WormCacheQFlex captures the **long-term** state. Per-sampling-unit short-term state (pipeline, store buffers) is warmed separately by Flexus during the per-unit "detailed warming" prefix.

## Layout

| Path | Purpose |
|---|---|
| `src/lib.rs` | Plugin entry points: `qemu_plugin_version`, `vcpu_tb_trans`, `savevm_cb`, `init_plugin` FFI stubs. |
| `src/parameter.rs` | Compile-time constants (`CORE_COUNT`, cache/TLB dimensions, feature flags). **Regenerated per experiment** by [../commands/jinja_loaders/](../commands/jinja_loaders/) from `templates/parameter.rs.j2` before each rebuild — the checked-in file is just a template default. |
| `src/plugin/` | Wrappers around QEMU's plugin C API (per-core instrumentation, TB translation hooks, the `QEMUPlugin` trait). |
| `src/components/` | Functional models: `cache_hierarchy/` (LRU L1d/L1i/L2/shared, MSI w/ directory), `bp/` (branch predictor), `wfi/`, `touch_once/`, `trace/`, `instruction_frequency/`, `page_walk_logger/`. |
| `src/checkpoint/` | Serialisation of warm state to JSON (caches, TLB entries, directory state, frontend state, vtime per core). Also produces a `checkpoint_conversion` binary the parent uses to fold these into Flexus configuration. |
| `src/chronic/` | Periodic-snapshot driver (the "chronic" component). Mode/init_threshold/interval/count parameters; supports multiple snapshot formats. |
| `src/qemu_api.rs` | Auto-generated bindgen wrappers around `qemu/include/qemu/qemu-plugin.h`. |
| `src/arch/` | Arch-specific logic. **aarch64 only currently.** |
| `plugin_helper/` | Cargo workspace member; helpers for plugin registration and IPC. |
| `Cargo.toml` | Rust 2024 edition. Key deps: `serde_json`, `dashmap` (concurrent per-core state), `bitvec`, `zstd` (checkpoint compression), `perf-event`, `hdrhistogram`. |
| `tests/`, `benches/`, `misc/` | Test fixtures, benchmarks, helpers. |
| `qemu-plugin.h` | A symlink — see "stale path" gotcha below. |

## Build

`cargo build --release` (the parent Makefile does this). Output:
- `libworm_cache.so` (cdylib + rlib) — the actual plugin.
- `checkpoint_conversion` binary — used in result aggregation.

The parent's `set_up_folders()` in [../commands/config.py](../commands/config.py) copies the entire `WormCacheQFlex/` directory into per-experiment `lib/WormCacheQFlex/` (see ~line 312–315), and `libworm_cache.so` is loaded from there.

Release profile keeps debug symbols (`debug = true` in Cargo.toml's `profile.release`).

## Public interface to the rest of QFlex

### How parallel-qemu loads it

Loaded as a standard QEMU plugin from QFlex's run scripts:

```
-plugin lib/libworm_cache.so,mode={normal|warm|ff},init_threshold=N,interval=N,count=N,prefix="...",init_index=N,no_qemu_snapshot=bool,snapshot_format={zstd|incremental|incremental_first_base}
```

Args are parsed from the plugin arg string (key=value, comma-separated) — see `src/plugin/`.

### Modes

The `mode=...` string is consumed in two places:

- **`src/chronic/mod.rs:40`** — selects the periodic-snapshot driver behaviour:
  - `normal` (default) — no periodic snapshots.
  - `warm` — sampling phase. Periodic snapshots on the FW timeline at `interval`-tick granularity, stop after `count` snapshots.
  - `ff` — fast-forward variant (similar to warm; differences are localised in the chronic module).
- **`src/components/cache_hierarchy/parallel_hierarchy/mod.rs:183`** — separately, `mode=pure_fill` switches the cache hierarchy into an initial cold-fill warming run that loads the working set from a previous snapshot (controlled by `prefix` and `warm_ratio`). This is independent of the chronic mode.

So *"mode"* overloads two semantics depending on which component reads it.

### Snapshot format

`snapshot_format` (see `src/chronic/snapshot.rs:84`) maps to QEMU plugin snapshot formats:
- `zstd` → `QEMU_PLUGIN_SNAPSHOT_FORMAT_EXTERNAL_ZSTD`
- `incremental` → `QEMU_PLUGIN_SNAPSHOT_FORMAT_EXTERNAL_INCREMENTAL_DELTA`
- `incremental_first_base` (default) → `QEMU_PLUGIN_SNAPSHOT_FORMAT_EXTERNAL_INCREMENTAL_BASE`

### Plugin hooks observed

- `vcpu_tb_trans(plugin_id, tb)` — called per translation block; instruments memory and execution callbacks.
- `savevm_cb(name)` — called on QEMU savevm events; serialises warm state and (recently) checks if all cores are "warmed" so the run can terminate early. Recent commit history is centred on getting the early-termination behaviour right.

### Checkpoint output

JSON (optionally zstd-compressed). One file per sampling unit. Carries:
- Cache hierarchy: per-core L1i/L1d, per-core or shared L2, sliced directory in MSI states.
- TLBs: per-core ITLB/DTLB (L1), shared STLB (L2).
- Branch predictor: per-core BTB.
- Per-core virtual-time (`vtime`) timestamp.

These are read by Flexus via the parent's `checkpoint_conversion` binary, which folds them into `flexus_configuration.json`.

## Conventions and gotchas

- **`parameter.rs` is regenerated per experiment.** Don't treat the checked-in `CORE_COUNT` etc. as authoritative — `templates/parameter.rs.j2` is the source.
- **Stale absolute path:** `qemu-plugin.h` at the repo root is a **symlink to `/home/xusine/paraflex/qemu/include/qemu/qemu-plugin.h`** (a developer's home directory — not a portable path). bindgen reads through it. If you're not on that developer's machine, repoint the symlink at `../parallel-qemu/include/qemu/qemu-plugin.h` (or the equivalent in the QEMU you're building against) before `cargo build`.
- **Vtime per core** is used to align checkpoints with timing-phase measurement timestamps; don't assume wall-clock or instruction count.
- **Mode overloading** — `mode=` is read by both the chronic driver and the cache hierarchy with different value sets. When in doubt, grep the value in both places.
- **No top-level README.** The closest thing is `src/chronic/README.md` and `src/components/cache_hierarchy/README.md` (terse).
- The submodule lives on branch `no-quitting-after-saving-checkpointing`. Recent work: avoid quit loop after checkpointing; warmed-notification guards.

## See also

- [../CLAUDE.md](../CLAUDE.md) — qflex root: four-phase pipeline, sampling vocabulary (population / sample / sampling unit), `ExperimentContext`.
- [../parallel-qemu/CLAUDE.md](../parallel-qemu/CLAUDE.md) — the fast QEMU that loads this plugin via `-plugin` during phases 2–3.
- [../flexus/CLAUDE.md](../flexus/CLAUDE.md) — the timing model that consumes the checkpoints this plugin emits.
- [../qemu/CLAUDE.md](../qemu/CLAUDE.md) — timing QEMU (the binary Flexus runs in; **does not** load this plugin).
- [../qemu/middleware/CLAUDE.md](../qemu/middleware/CLAUDE.md) — QEMU↔Flexus IPC shim used in the timing phase, after the FW checkpoints from this plugin are loaded.
