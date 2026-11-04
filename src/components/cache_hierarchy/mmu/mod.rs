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

// Desc: Memory Management Unit
// This file is highly related to the ISA.

pub mod gem5_tlb;
pub mod l1_fully_associative;
pub mod no_mmu;
pub mod ordinary_mmu;
pub mod tlb;

use serde::{Deserialize, Serialize};
use tlb::AddressSpaceID;

#[derive(Debug, Serialize, Deserialize, Clone, Copy)]
pub enum MMUFlushMode {
    All,
    ByASID(AddressSpaceID),
    ByVPN(u64, u64),                        // (VPN, Page number)
    ByVPNAndASID(u64, u64, AddressSpaceID), // (VPN, Page number, ASID)
}

#[derive(Debug, Serialize, Deserialize, Copy, Clone)]
pub enum MMUTranslationResult {
    Hit(u64, u32),         // PA
    Miss(u64, [u64; 4]),   // PA, walk traces
    MissNotCacheable(u64), // PA
}

pub trait AbstractMMU {
    fn new() -> Self;
    fn translate_and_refill(
        &mut self,
        core_id: u32,
        va: u64,
        ts: u64,
        is_instruction: bool,
    ) -> MMUTranslationResult;

    // Currently, this interface is for debugging. It reuses QEMU's PTW result.
    fn lookup(&mut self, vpn: u64, ts: u64, is_instruction: bool) -> Option<u64>;

    fn flush(&mut self, mode: MMUFlushMode);

    fn serialize(&self) -> serde_json::Value;
    fn deserialize(&mut self, value: serde_json::Value);

    fn serialize_gem5_tlbs(&self) -> serde_json::Value {
        serde_json::Value::Null
    }
}

pub use l1_fully_associative::FullyAssociativeL1MMU;
pub use no_mmu::NoMMU;
pub use ordinary_mmu::OrdinaryMMU;
pub use tlb::FullyAssociativeTLB;
pub use tlb::TLB;

#[cfg(test)]
mod test;
