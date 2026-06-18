---
name: multi-node
description: QFlex multi-node from the WormCacheQFlex perspective — WormCache is multi-node-naive; its only multi-node-relevant moment is firing savevm_cb to signal "snapshot now" and yielding scheduling control to PDES, which lands the actual checkpoint at a quantum-aligned virtual time. NUMA in WormCache (numa_node_id) is intra-chip, NOT multi-node. Use when the user asks about WormCache and multi-node, NUMA vs multi-node, or how FW snapshots become coherent across nodes.
---

This skill's content lives in `MULTI_NODE.md` next to this directory's `CLAUDE.md`. Read it now: [../../../MULTI_NODE.md](../../../MULTI_NODE.md).

That doc explains why WormCache does almost nothing for multi-node directly, the one moment it does interact with PDES (`savevm_cb`), and why NUMA inside the cache code must not be conflated with multi-node identity.
