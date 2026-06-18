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

use crate::components::cache_hierarchy::common::{Directory, FiniteDirectory, InfiniteDirectory};
use crate::components::cache_hierarchy::mmu::{self, AbstractMMU};
/*
 * The purpose of this file is to provide a parser over the parameter.rs to generate the cache hierarchy at the compile time.
 * Basically, I want to select which ParallelMemoryHierarchy based on the paramter.rs at the compile time.
 * Well, this is not available in the Rust. So, I have to use the trait and its polymorphism to implement the logic, which is extremely dirty.
 *
 * Reference: https://willcrichton.net/notes/type-level-programming/
 *
 */
use crate::parameter::{self};
use crate::{arch::AArch64, components::cache_hierarchy::MemoryHierarchy};

pub trait CacheModelParser<const UNIFIED_CACHE_MODEL: bool> {
    type Output: MemoryHierarchy;
}

pub trait MMUParser<const USE_FULLY_ASSOCIATIVE_L1_TLB: bool> {
    type Output: AbstractMMU;
}

pub trait DirectoryParser<const USE_INFINITE_DIRECTORY: bool> {
    type Output: Directory;
}

pub struct DummyParser;

pub trait SharedCacheStatisticsParser<const ENABLE_STATISTICS: bool> {
    type Output;
}

impl SharedCacheStatisticsParser<true> for DummyParser {
    type Output = SharedCacheSetMissStatistics;
}

impl SharedCacheStatisticsParser<false> for DummyParser {
    type Output = ZeroSharedCacheSetStatistics;
}

pub type SharedCacheStatisticsWithPlugin = <DummyParser as SharedCacheStatisticsParser<
    { parameter::ENABLE_SHARED_CACHE_STATISTICS },
>>::Output;

use super::{
    super::common::{
        ParallelHarvardPrivateCache, ParallelLRUSharedCache, ParallelUnifiedPrivateCache,
        statistics::{SharedCacheSetMissStatistics, ZeroSharedCacheSetStatistics},
    },
    hierarchy,
};

pub const ALLOCATED_CORE_COUNT: usize = parameter::REAL_CORE_COUNT;

impl MMUParser<true> for DummyParser {
    type Output = mmu::FullyAssociativeL1MMU<
        AArch64,
        { parameter::ITLB_ASSO },
        { parameter::DTLB_ASSO },
        {
            if parameter::STLB_ENABLED {
                parameter::STLB_ASSO
            } else {
                0
            }
        },
        {
            if parameter::STLB_ENABLED {
                parameter::STLB_SET
            } else {
                0
            }
        },
        { parameter::NO_HUGE_PAGE },
    >;
}

impl MMUParser<false> for DummyParser {
    type Output = mmu::OrdinaryMMU<
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
}

type AArch64MMU = <DummyParser as MMUParser<{ parameter::USE_HIGHLY_ASSOCIATIVE_L1TLB }>>::Output;

impl DirectoryParser<true> for DummyParser {
    type Output = InfiniteDirectory<{ parameter::INFINITE_DIRECTORY_SHARED_COUNT }>;
}

impl DirectoryParser<false> for DummyParser {
    type Output =
        FiniteDirectory<{ parameter::FINITE_DIRECTORY_SET }, { parameter::FINITE_DIRECTORY_ASSO }>;
}

type DirectoryForPlugin =
    <DummyParser as DirectoryParser<{ parameter::USE_INFINITE_DIRECTORY }>>::Output;

#[allow(dead_code)]
type ParalleMemoryHierarchyUnified = hierarchy::ParallelMemoryHierarchy<
    AArch64MMU,
    ParallelUnifiedPrivateCache<
        { ALLOCATED_CORE_COUNT },
        { parameter::UNIFIED_PRI_CACHE_SET },
        { parameter::UNIFIED_PRI_CACHE_ASSO },
    >,
    ParallelLRUSharedCache<
        SharedCacheStatisticsWithPlugin,
        { parameter::SHARED_CACHE_SET },
        { parameter::SHARED_CACHE_ASSO },
        { parameter::SHARED_CACHE_EXCLUSIVE },
    >,
    DirectoryForPlugin,
    { parameter::SHARED_CACHE_FILL_WITH_PRIVATE_CACHE },
    { parameter::SHARED_CACHE_FILL_ON_CLEAN_EVICTION },
    { parameter::SHARED_CACHE_FILL_ON_DIRTY_EVICTION },
    { parameter::SHARED_CACHE_FILL_ON_REPLICA_CREATION },
    { ALLOCATED_CORE_COUNT },
>;

#[allow(dead_code)]
type ParallelMemoryHierarchyHarvard = hierarchy::ParallelMemoryHierarchy<
    AArch64MMU,
    ParallelHarvardPrivateCache<
        { ALLOCATED_CORE_COUNT },
        { parameter::HARVARD_PRI_I_CACHE_SET },
        { parameter::HARVARD_PRI_I_CACHE_ASSO },
        { parameter::HARVARD_PRI_D_CACHE_SET },
        { parameter::HARVARD_PRI_D_CACHE_ASSO },
    >,
    ParallelLRUSharedCache<
        SharedCacheStatisticsWithPlugin,
        { parameter::SHARED_CACHE_SET },
        { parameter::SHARED_CACHE_ASSO },
        { parameter::SHARED_CACHE_EXCLUSIVE },
    >,
    DirectoryForPlugin,
    { parameter::SHARED_CACHE_FILL_WITH_PRIVATE_CACHE },
    { parameter::SHARED_CACHE_FILL_ON_CLEAN_EVICTION },
    { parameter::SHARED_CACHE_FILL_ON_DIRTY_EVICTION },
    { parameter::SHARED_CACHE_FILL_ON_REPLICA_CREATION },
    { ALLOCATED_CORE_COUNT },
>;

impl CacheModelParser<true> for DummyParser {
    type Output = ParalleMemoryHierarchyUnified;
}

impl CacheModelParser<false> for DummyParser {
    type Output = ParallelMemoryHierarchyHarvard;
}

pub type HierarchyForPlugin =
    <DummyParser as CacheModelParser<{ parameter::USE_UNIFIED_CACHE }>>::Output;
