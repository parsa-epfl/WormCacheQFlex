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

use std::cell::UnsafeCell;



use crate::{
    arch::AArch64,
    components::cache_hierarchy::{
        CacheBlockRequest, MemoryHierarchy,
        common::{
            CacheAccessType, CacheHierarchyAccessResult, SharedCache, SharedCacheAccessRequest,
            SharedCacheAccessSource, SharedCacheLookupResult,
        },
        mmu::{self, AbstractMMU, MMUTranslationResult},
    },
    debug::statistics::{EventType, Statistics},
    parameter,
};

use super::super::common::{ParallelLRUSharedCache, statistics::ZeroSharedCacheSetStatistics};

pub struct SingleCacheHierarchy<MMU: AbstractMMU> {
    pub shared_cache: ParallelLRUSharedCache<
        ZeroSharedCacheSetStatistics,
        { parameter::SHARED_CACHE_SET },
        { parameter::SHARED_CACHE_ASSO },
        { parameter::SHARED_CACHE_EXCLUSIVE },
    >,

    mmus: [UnsafeCell<MMU>; parameter::CORE_COUNT],
}

impl<MMU: AbstractMMU> SingleCacheHierarchy<MMU> {
    pub fn new() -> Self {
        SingleCacheHierarchy {
            shared_cache: ParallelLRUSharedCache::new(),
            mmus: std::array::from_fn(|_| UnsafeCell::new(MMU::new())),
        }
    }

    fn serialize_mmus(&self, name: &str, numa_node_id: usize) {
        let mut file =
            std::fs::File::create(format!("{}/mmus-{}.json", name, numa_node_id)).unwrap();

        let multiple_mmus = self
            .mmus
            .iter()
            .map(|x| unsafe { (*x.get()).serialize() })
            .collect::<Vec<_>>();

        serde_json::to_writer(&mut file, &serde_json::Value::Array(multiple_mmus)).unwrap();
    }

    fn deserialize_mmus(&self, name: &str, numa_node_id: usize) {
        let file = std::fs::File::open(format!("{}/mmus-{}.json", name, numa_node_id));

        if file.is_err() {
            println!("Cannot load the MMU state. Error: {:?}", file.err());
            return;
        }

        let file = file.unwrap();

        let multiple_mmus: serde_json::Value = serde_json::from_reader(file).unwrap();

        match multiple_mmus {
            serde_json::Value::Array(mmus) => {
                for (i, mmu) in mmus.into_iter().enumerate() {
                    unsafe { (*self.mmus[i].get()).deserialize(mmu) };
                }
            }
            _ => panic!("Invalid format."),
        };
    }

    pub fn get_scache_warmed_set_count(&self) -> usize {
        self.shared_cache.warmed_sets_count()
    }

    pub fn get_scache_warmed_slots_count(&self) -> usize {
        self.shared_cache.warmed_slots_count()
    }
}

impl<MMU: AbstractMMU> MemoryHierarchy for SingleCacheHierarchy<MMU> {
    fn access_memory_pblock_id(
        &self,
        request: &CacheBlockRequest,
        ts: u64,
    ) -> CacheHierarchyAccessResult {
        let is_store = request.is_store();
        let is_ptw = request.is_page_walk();
        let is_fetch = request.is_instruction();
        let is_os = request.is_os();
        let core_id = request.core_id;
        let block_id = request.block_id;

        Statistics::global_record(core_id, EventType::DataAccess, is_os);
        Statistics::global_record(core_id, EventType::SharedCacheAccess, is_os);

        let res = self.shared_cache.lookup_and_insert_on_miss(
            &SharedCacheAccessRequest {
                source: SharedCacheAccessSource::Core(core_id),
                block_id,
                access_type: CacheAccessType::DataRead, // Read does not have impact on the tag array.
                is_os,
            },
            ts,
            true,
        );

        Statistics::global_record(
            core_id,
            match res {
                SharedCacheLookupResult::Hit(_) => EventType::SharedCacheAccess,
                SharedCacheLookupResult::Miss => EventType::SharedCacheMiss,
                SharedCacheLookupResult::ColdMiss => EventType::SharedCacheColdMiss,
                SharedCacheLookupResult::LookupLate(_, _) => {
                    EventType::SharedCacheAccessCausalityViolation
                }
                SharedCacheLookupResult::EvictedLate(_) => {
                    EventType::SharedCacheEvictionCausalityViolation
                }
            },
            is_os,
        );

        if matches!(res, SharedCacheLookupResult::Miss) {
            if is_store {
                Statistics::global_record(core_id, EventType::SharedCacheMissDueToDataWrite, is_os);
            } else if is_fetch {
                Statistics::global_record(
                    core_id,
                    EventType::SharedCacheMissDueToInstructionFetch,
                    is_os,
                );
            } else if is_ptw {
                Statistics::global_record(core_id, EventType::SharedCacheMissDueToPTW, is_os);
            } else {
                Statistics::global_record(core_id, EventType::SharedCacheMissDueToDataRead, is_os);
            }
        }

        match res {
            SharedCacheLookupResult::Hit(_) => CacheHierarchyAccessResult::HitInSharedCache,
            SharedCacheLookupResult::Miss => CacheHierarchyAccessResult::Miss,
            SharedCacheLookupResult::ColdMiss => CacheHierarchyAccessResult::Miss,
            SharedCacheLookupResult::LookupLate(_, _) => CacheHierarchyAccessResult::Unknown,
            SharedCacheLookupResult::EvictedLate(_) => CacheHierarchyAccessResult::Miss,
        }
    }

    fn translate(
        &self,
        r: &crate::components::cache_hierarchy::MemoryAccessRequest,
        ts: u64,
    ) -> MMUTranslationResult {
        unsafe {
            self.mmus[r.core_id as usize]
                .get()
                .as_mut()
                .unwrap()
                .translate_and_refill(r.core_id, r.va, ts, r.is_instruction())
        }
    }

    fn flush_mmu(&self, core_id: u32, info: crate::components::cache_hierarchy::mmu::MMUFlushMode) {
        unsafe {
            self.mmus[core_id as usize]
                .get()
                .as_mut()
                .unwrap()
                .flush(info);
        }
    }

    fn serialize(&self, name: &str, numa_node_id: usize) {
        println!("Serializing private caches.");
        self.shared_cache.serialize(name, numa_node_id);
        println!("Serialize MMUs");
        self.serialize_mmus(name, numa_node_id);
    }

    fn deserialize(&mut self, name: &str, numa_node_id: usize) {
        println!("Deserializing private caches.");
        self.shared_cache.deserialize(name, numa_node_id);
        println!("Deserialize MMUs");
        self.deserialize_mmus(name, numa_node_id);
    }

    fn access_from_device_with_pa(
        &self,
        _paddr: u64,
        _access_type: CacheAccessType,
        _ts: u64,
    ) -> CacheHierarchyAccessResult {
        todo!()
    }
}

type AArch64MMU = mmu::OrdinaryMMU<
    AArch64,
    { parameter::ITLB_ASSO },
    { parameter::ITLB_SET },
    { parameter::DTLB_ASSO },
    { parameter::DTLB_SET },
    { parameter::STLB_ENABLED },
    { parameter::STLB_ASSO },
    { parameter::STLB_SET },
    { parameter::NO_HUGE_PAGE },
>;

pub type PluginSingleCacheHierarchy = SingleCacheHierarchy<AArch64MMU>;
