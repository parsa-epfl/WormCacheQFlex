# MULTI_NODE.md — WormCacheQFlex/

This file documents what WormCacheQFlex contributes to QFlex multi-node and how it relies on the other submodules. The cross-cutting overview is in [../MULTI_NODE.md](../MULTI_NODE.md). Read that first if you haven't.

For this submodule's general context (Rust QEMU plugin, build, modes, checkpoint format) see [CLAUDE.md](CLAUDE.md).

## What this submodule contributes to PDES

**Almost nothing.** WormCache is multi-node-naive — and that's by design.

The one multi-node-relevant moment in the entire plugin is this: when WormCache decides a sampling-unit boundary has been hit, its `savevm_cb` fires and **signals "snapshot now"** to QEMU. From that point on, **WormCache yields scheduling control to PDES**. PDES (running in parallel-qemu's `net/pdes-checkpoint.c`) decides at which **quantum-aligned virtual time** the snapshot actually lands, drains in-flight messages with neighbours (`DRAIN_START` / `DRAIN_END`), and only then does the per-node checkpoint actually happen.

Said differently: WormCache picks the *moment to ask*; PDES picks the *moment it lands*. WormCache doesn't know about neighbours, virtual time, quanta, or sync — and shouldn't.

### What WormCache does NOT do for multi-node

- **NUMA in WormCache code (`numa_node_id` in cache hierarchy) is intra-chip, not multi-node.** Don't conflate. `numa_node_id` is a NUMA domain inside one simulated machine.
- It does not coordinate with WormCache instances on neighbour nodes — there is no cross-node coordination inside the plugin. Each multi-node QEMU process loads its own `libworm_cache.so` instance producing its own per-node checkpoints.
- It does not implement any PDES message types or care about WWT.
- It does not pause itself for sync — pause/resume coordination happens at the QEMU + Flexus level, not inside the plugin.

## How this submodule uses the other submodules

- **[../parallel-qemu/](../parallel-qemu/)** — loads WormCache as `-plugin lib/libworm_cache.so,…` during functional warming. The crucial multi-node interaction: WormCache's `savevm_cb` is the trigger for parallel-qemu's distributed-snapshot drain machinery in `net/pdes-checkpoint.c`. Without WormCache firing this hook, multi-node FW checkpoints would not be coherent.

- **[../qemu/](../qemu/)** — does not load WormCache. The timing fork only consumes the FW checkpoints WormCache produced (loaded via `snapvm-external` from [../qemu/middleware/](../qemu/middleware/)).

- **[../flexus/](../flexus/)** — consumes WormCache's per-sampling-unit checkpoints at the start of each timing-phase sampling unit (cache, TLB, BP state). No direct linkage; the parent's `checkpoint_conversion` binary folds WormCache JSON into `flexus_configuration.json`.

- **[../qemu/middleware/](../qemu/middleware/)** — not used directly. But its `snapvm-external` is the ingest path into the timing phase for the checkpoints WormCache produced.

## See also

- [../MULTI_NODE.md](../MULTI_NODE.md) — the comprehensive cross-cutting overview.
- [../parallel-qemu/MULTI_NODE.md](../parallel-qemu/MULTI_NODE.md) — the QEMU that loads this plugin and that implements the distributed-snapshot drain triggered by `savevm_cb`.
- [../qemu/MULTI_NODE.md](../qemu/MULTI_NODE.md) — the timing QEMU that loads the resulting checkpoints (does NOT load this plugin).
- [../qemu/middleware/MULTI_NODE.md](../qemu/middleware/MULTI_NODE.md) — `snapvm-external` ingest for the checkpoints this plugin produces.
- [../flexus/MULTI_NODE.md](../flexus/MULTI_NODE.md) — consumes the checkpoints this plugin produces.
