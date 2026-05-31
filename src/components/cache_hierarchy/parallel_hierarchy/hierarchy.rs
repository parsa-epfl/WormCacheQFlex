// BSD 3-Clause License
//
// Copyright (c) 2024, Parallel Systems Architecture Laboratory (PARSA), EPFL.
// All rights reserved.
//
// Redistribution and use in source and binary forms, with or without
// modification, are permitted provided that the following conditions are met:
//
// 1. Redistributions of source code must retain the above copyright notice, this
//    list of conditions and the following disclaimer.
//
// 2. Redistributions in binary form must reproduce the above copyright notice,
//    this list of conditions and the following disclaimer in the documentation
//    and/or other materials provided with the distribution.
//
// 3. Neither the name of the PARSA, EPFL
//    nor the names of its contributors may be used to endorse or promote
//    products derived from this software without specific prior written
//    permission.
//
// THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
// AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
// IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
// DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE
// FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
// DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
// SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
// CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
// OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
// OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

use zstd::{Decoder, Encoder};

use crate::components::cache_hierarchy::common::SharedCacheAccessSource;
use crate::components::cache_hierarchy::mmu::AbstractMMU;
use crate::parameter;

use crate::debug::statistics::{EventType, Statistics};

use crate::debug::cache_line_history::{CacheLineCoherenceHistory, CacheOperationType};

use super::super::common::{Directory, DirectorySet, PrivateCache, SharedCache};

use std::cell::UnsafeCell;
use std::ops::DerefMut;

#[cfg(test)]
mod debug_tests;
// #[cfg(test)]
// mod harvard_reverse_order_tests;
#[cfg(test)]
mod finite_directory_eviction_tests;
#[cfg(test)]
mod harvard_tests;
#[cfg(test)]
mod reverse_order_tests;

mod access_logic;

pub struct ParallelMemoryHierarchy<
    MMU: AbstractMMU,
    PCache: PrivateCache,
    SCache: SharedCache,
    Dir: Directory,
    const FILL_SCACHE_ON_FILLING_PCACHE: bool,
    const FILL_SCACHE_ON_PCACHE_CLEAN_EVICTION: bool,
    const FILL_SCACHE_ON_PCACHE_DIRTY_EVICTION: bool,
    const FILL_SCACHE_ON_PCACHE_REPLICA_CREATION: bool,
    const CORE_COUNT: usize,
> {
    mmus: [UnsafeCell<MMU>; CORE_COUNT],

    private_caches: PCache,
    directory: Dir,

    shared_cache: SCache,
}

impl<
    MMU: AbstractMMU,
    PCache: PrivateCache,
    SCache: SharedCache,
    Dir: Directory,
    const FILL_SCACHE_ON_FILLING_PCACHE: bool,
    const FILL_SCACHE_ON_PCACHE_EVICTION: bool,
    const FILL_SCACHE_ON_PCACHE_WRITEBACK: bool,
    const FILL_SCACHE_ON_PCACHE_REPLICA_CREATION: bool,
    const CORE_COUNT: usize,
>
    ParallelMemoryHierarchy<
        MMU,
        PCache,
        SCache,
        Dir,
        FILL_SCACHE_ON_FILLING_PCACHE,
        FILL_SCACHE_ON_PCACHE_EVICTION,
        FILL_SCACHE_ON_PCACHE_WRITEBACK,
        FILL_SCACHE_ON_PCACHE_REPLICA_CREATION,
        CORE_COUNT,
    >
{
    pub fn new() -> Self {
        Self {
            mmus: std::array::from_fn(|_| UnsafeCell::new(MMU::new())),
            private_caches: PCache::new(),
            directory: Directory::new(),
            shared_cache: SCache::new(),
        }
    }

    pub fn handle_eviction(
        &self,
        directory_set_guard: &mut impl DerefMut<Target = Dir::TSet>,
        cache_id: usize,
        block_id: u64,
        ts: u64,
        modified: bool,
        is_os: bool,
    ) {
        let directory_entry = {
            let tmp = directory_set_guard.get_or_create(block_id);
            assert!(tmp.1.is_none());
            tmp.0
        };

        // we cancel the element of this block in the directory.
        let sharer = directory_entry.sharers;

        if sharer.get(cache_id).unwrap() == false {
            // Well, it is already invalid by other core.
            if parameter::ENABLE_CACHE_LINE_HISTORY {
                let his = CacheLineCoherenceHistory::global_get_block_history(block_id).unwrap();
                his.value().print_history();
                println!(
                    "Failed operation: {:?}, Cache ID: {}, Timestamp: {}, Refilled: false, Share List: {:?}",
                    CacheOperationType::Drop,
                    cache_id,
                    ts,
                    sharer.iter_ones().collect::<Vec<usize>>()
                );
            }
            panic!();
        }

        // we put the element back to the directory.
        directory_entry.update_lru_ts(ts);
        directory_entry.sharers.set(cache_id, false);

        CacheLineCoherenceHistory::global_record_history(
            block_id,
            CacheOperationType::Drop,
            cache_id,
            ts,
            false,
            directory_entry.sharers,
            line!(),
        );

        // before releasing the lock of the directory, we need to check whether we need to place this lock to the shared cache.
        if directory_entry.sharers.count_ones() == 0 {
            // we need to place this block to the shared cache.
            Statistics::global_record(
                PCache::find_cache_info_by_cache_id(cache_id).0,
                EventType::SharedCacheAccess,
                is_os,
            );

            let core_id = PCache::find_cache_info_by_cache_id(cache_id).0;

            if FILL_SCACHE_ON_PCACHE_EVICTION && !modified {
                self.shared_cache.insert(
                    SharedCacheAccessSource::Core(core_id),
                    block_id,
                    ts,
                    modified,
                    true,
                );
            }

            if FILL_SCACHE_ON_PCACHE_WRITEBACK && modified {
                self.shared_cache.insert(
                    SharedCacheAccessSource::Core(core_id),
                    block_id,
                    ts,
                    modified,
                    true,
                );
            }
        }
    }

    pub fn dump_access_counter(&self) {
        // self.shared_cache.dump_access_counter();
    }

    pub fn get_scache_warmed_set_count(&self) -> usize {
        self.shared_cache.warmed_sets_count()
    }

    pub fn get_scache_warmed_slots_count(&self) -> usize {
        self.shared_cache.warmed_slots_count()
    }

    pub fn information() -> String {
        format!(
            "Private Cache: {}\nDirectory: {}\nShared Cache: {}\nFill Shared Cache on Filling Private Cache: {} \nFill Shared Cache on Private Cache Clean Eviction: {} \nFill Shared Cache on Private Cache Dirty Eviction: {} \nFill Shared Cache on Private Cache Replica Creation: {}",
            PCache::information(),
            Dir::information(),
            SCache::information(),
            FILL_SCACHE_ON_FILLING_PCACHE,
            FILL_SCACHE_ON_PCACHE_EVICTION,
            FILL_SCACHE_ON_PCACHE_WRITEBACK,
            FILL_SCACHE_ON_PCACHE_REPLICA_CREATION
        )
    }

    pub fn dump_diagnose_information(&self) {
        self.private_caches.print_debug_info();

        // self.shared_cache
        //     .dump_access_frequency("shared_cache_access_frequency.csv");

        // if let Some(hist) = self.vts_violation_distribution.as_ref() {
        //     // we need to dump the distribution.
        //     for core_id in 0..parameter::CORE_COUNT {
        //         let hist = unsafe { &mut *hist[core_id].get() };
        //         let mut serializer = hdrhistogram::serialization::V2Serializer::new();
        //         let mut buffer = Vec::new();
        //         serializer.serialize(hist, &mut buffer).unwrap();
        //         let mut file = File::create(format!("vts_violation_{}.hist", core_id)).unwrap();
        //         file.write_all(&buffer).unwrap();
        //     }
        // }
    }

    fn serialize_mmus(&self, name: &str, numa_node_id: usize) {
        let file =
            std::fs::File::create(format!("{}/mmus-{}.json.zstd", name, numa_node_id)).unwrap();
        let mut file = Encoder::new(file, 0).unwrap();

        let multiple_mmus = self
            .mmus
            .iter()
            .map(|x| unsafe { (*x.get()).serialize() })
            .collect::<Vec<_>>();

        serde_json::to_writer(&mut file, &serde_json::Value::Array(multiple_mmus)).unwrap();
        file.finish().unwrap();
    }

    fn deserialize_mmus(&self, name: &str, numa_node_id: usize) {
        let file = std::fs::File::open(format!("{}/mmus-{}.json.zstd", name, numa_node_id));

        if file.is_err() {
            println!("Cannot load the MMU state. Error: {:?}", file.err());
            return;
        }

        let file = file.unwrap();
        let mut file = Decoder::new(file).unwrap();

        let multiple_mmus: serde_json::Value = serde_json::from_reader(&mut file).unwrap();

        match multiple_mmus {
            serde_json::Value::Array(mmus) => {
                for (i, mmu) in mmus.into_iter().enumerate() {
                    unsafe { (*self.mmus[i].get()).deserialize(mmu) };
                }
            }
            _ => panic!("Invalid format."),
        };
    }
}
