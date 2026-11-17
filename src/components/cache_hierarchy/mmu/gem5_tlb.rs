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

// Separate L1 ITLB/DTLB model for gem5, enabled by parameter::GEM5_COMPATIBILITY.
// Unlike the default model, each entry keeps its real page size, and a hit compares
// only the VPN at that page size and the ASID (as gem5's TlbEntry::match does).

use serde::{Deserialize, Serialize};

use super::{MMUFlushMode, tlb::AddressSpaceID};
use crate::arch::{self, MiscRegs};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Gem5TLBEntry {
    pub valid: bool,
    pub ts: u64,
    pub asid: AddressSpaceID,
    pub vpn: u64,
    pub ppn: u64,
    pub n: u8,
    pub misc_regs: MiscRegs,
}

impl Gem5TLBEntry {
    fn covers(&self, va: u64) -> bool {
        (va >> self.n) == self.vpn
    }

    fn covers_any_4k_page(&self, start_vpn: u64, page_count: u64) -> bool {
        (0..page_count).any(|p| self.covers((start_vpn + p) << 12))
    }
}

// A single set, nested like the TLBs in mmus-N.json so that gem5's
// ArmVATranslator::parseJSON reads this file without changes.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
struct Gem5TLBSet {
    entries: Vec<Gem5TLBEntry>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Gem5L1TLB<const ENTRIES: usize> {
    entries: Vec<Gem5TLBSet>,
}

impl<const ENTRIES: usize> Default for Gem5L1TLB<ENTRIES> {
    fn default() -> Self {
        Self {
            entries: vec![Gem5TLBSet::default()],
        }
    }
}

impl<const ENTRIES: usize> Gem5L1TLB<ENTRIES> {
    fn set(&mut self) -> &mut Vec<Gem5TLBEntry> {
        &mut self.entries[0].entries
    }

    /// The valid entries, in no particular order (recency is in `ts`).
    pub fn entries(&self) -> &[Gem5TLBEntry] {
        &self.entries[0].entries
    }

    // gem5 scans from its most recent entry and stops at the first match, so
    // when two entries cover the address (e.g. a global and a non-global
    // one), the most recent one wins. Do the same.
    fn lookup(&mut self, va: u64, asid: AddressSpaceID, ts: u64) -> bool {
        let hit = self
            .set()
            .iter_mut()
            .filter(|e| e.covers(va) && e.asid.check(&asid))
            .max_by_key(|e| e.ts);
        match hit {
            Some(entry) => {
                entry.ts = ts;
                true
            }
            None => false,
        }
    }

    fn insert(&mut self, entry: Gem5TLBEntry) {
        let set = self.set();
        if set.len() < ENTRIES {
            set.push(entry);
        } else {
            let victim = (0..set.len()).min_by_key(|&i| set[i].ts).unwrap();
            set[victim] = entry;
        }
    }

    fn access<ARCH: arch::ISA>(&mut self, va: u64, ts: u64) {
        let asid = AddressSpaceID::NonGlobal(ARCH::get_asid());
        if self.lookup(va, asid, ts) {
            return;
        }

        let walk = ARCH::ptw(va);
        let n: u8 = match walk.page_size {
            arch::PageSize::_4KB => 12,
            arch::PageSize::_2MB => 21,
            arch::PageSize::_1GB => 30,
        };
        let asid = if walk.is_global {
            AddressSpaceID::Global
        } else {
            asid
        };

        self.insert(Gem5TLBEntry {
            valid: true,
            ts,
            asid,
            vpn: va >> n,
            ppn: walk.paddr >> n,
            n,
            misc_regs: ARCH::get_misc_regs(),
        });
    }

    pub fn flush(&mut self, mode: MMUFlushMode) {
        let set = self.set();
        match mode {
            MMUFlushMode::All => set.clear(),
            MMUFlushMode::ByASID(asid) => set.retain(|e| e.asid != asid),
            MMUFlushMode::ByVPN(vpn, page_count) => {
                set.retain(|e| !e.covers_any_4k_page(vpn, page_count))
            }
            // As in ARM (TLBI VAE1) and gem5: global entries for the page are
            // removed too, so match the ASID the way a lookup does.
            MMUFlushMode::ByVPNAndASID(vpn, page_count, asid) => {
                set.retain(|e| !(e.asid.check(&asid) && e.covers_any_4k_page(vpn, page_count)))
            }
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Gem5L1TLBs<const I: usize, const D: usize> {
    pub itlb: Gem5L1TLB<I>,
    pub dtlb: Gem5L1TLB<D>,
}

impl<const I: usize, const D: usize> Gem5L1TLBs<I, D> {
    pub fn access<ARCH: arch::ISA>(&mut self, va: u64, ts: u64, is_instruction: bool) {
        if is_instruction {
            self.itlb.access::<ARCH>(va, ts);
        } else {
            self.dtlb.access::<ARCH>(va, ts);
        }
    }

    pub fn flush(&mut self, mode: MMUFlushMode) {
        self.itlb.flush(mode);
        self.dtlb.flush(mode);
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use crate::arch::{ISA, PageSize, TranslationResult};

    thread_local! {
        static CPSR: Cell<u64> = const { Cell::new(0) };
    }

    // Identity-mapped 2 MiB pages.
    struct HugePageISA;

    impl ISA for HugePageISA {
        fn get_asid() -> u16 {
            1
        }

        fn get_misc_regs() -> MiscRegs {
            MiscRegs {
                cpsr: CPSR.with(|c| c.get()),
                ..Default::default()
            }
        }

        fn ptw(va: u64) -> TranslationResult {
            TranslationResult {
                paddr: va,
                is_global: false,
                page_size: PageSize::_2MB,
                traces: [0; 4],
                cacheable: true,
            }
        }
    }

    #[test]
    fn huge_page_is_one_entry_even_when_cpsr_changes() {
        let mut tlb: Gem5L1TLB<64> = Gem5L1TLB::default();
        for i in 0..512u64 {
            CPSR.with(|c| c.set(i << 28));
            tlb.access::<HugePageISA>(0x4000_0000 + i * 4096, i + 1);
        }
        assert_eq!(tlb.set().len(), 1);
        let e = &tlb.set()[0];
        assert_eq!((e.n, e.vpn, e.ppn), (21, 0x4000_0000 >> 21, 0x4000_0000 >> 21));
    }

    #[test]
    fn flush_of_one_4k_page_removes_covering_huge_entry() {
        let mut tlb: Gem5L1TLB<64> = Gem5L1TLB::default();
        tlb.access::<HugePageISA>(0x4000_0000, 1);
        tlb.flush(MMUFlushMode::ByVPN((0x4000_0000 + 5 * 4096) >> 12, 1));
        assert!(tlb.set().is_empty());
    }

    #[test]
    fn full_tlb_evicts_least_recently_used() {
        let mut tlb: Gem5L1TLB<2> = Gem5L1TLB::default();
        tlb.access::<HugePageISA>(0x0000_0000, 1);
        tlb.access::<HugePageISA>(0x0020_0000, 2);
        tlb.access::<HugePageISA>(0x0000_0000, 3);
        tlb.access::<HugePageISA>(0x0040_0000, 4);
        let vpns: Vec<u64> = tlb.set().iter().map(|e| e.vpn).collect();
        assert!(vpns.contains(&0) && vpns.contains(&2) && !vpns.contains(&1));
    }

    fn entry(vpn: u64, asid: AddressSpaceID, ts: u64) -> Gem5TLBEntry {
        Gem5TLBEntry {
            valid: true,
            ts,
            asid,
            vpn,
            ppn: vpn,
            n: 12,
            misc_regs: MiscRegs::default(),
        }
    }

    #[test]
    fn flush_by_va_and_asid_removes_global_entry() {
        let mut tlb: Gem5L1TLB<64> = Gem5L1TLB::default();
        tlb.insert(entry(0x10, AddressSpaceID::Global, 1));
        tlb.insert(entry(0x10, AddressSpaceID::NonGlobal(7), 2));
        tlb.insert(entry(0x10, AddressSpaceID::NonGlobal(8), 3));
        tlb.flush(MMUFlushMode::ByVPNAndASID(0x10, 1, AddressSpaceID::NonGlobal(7)));
        let left: Vec<AddressSpaceID> = tlb.set().iter().map(|e| e.asid).collect();
        assert_eq!(left, vec![AddressSpaceID::NonGlobal(8)]);
    }

    #[test]
    fn flush_by_asid_keeps_global_entry() {
        let mut tlb: Gem5L1TLB<64> = Gem5L1TLB::default();
        tlb.insert(entry(0x10, AddressSpaceID::Global, 1));
        tlb.insert(entry(0x20, AddressSpaceID::NonGlobal(7), 2));
        tlb.flush(MMUFlushMode::ByASID(AddressSpaceID::NonGlobal(7)));
        let left: Vec<AddressSpaceID> = tlb.set().iter().map(|e| e.asid).collect();
        assert_eq!(left, vec![AddressSpaceID::Global]);
    }

    #[test]
    fn lookup_updates_most_recent_matching_entry() {
        let mut tlb: Gem5L1TLB<64> = Gem5L1TLB::default();
        tlb.insert(entry(0x10, AddressSpaceID::Global, 5));
        tlb.insert(entry(0x10, AddressSpaceID::NonGlobal(7), 1));
        assert!(tlb.lookup(0x10 << 12, AddressSpaceID::NonGlobal(7), 9));
        let ts: Vec<u64> = tlb.set().iter().map(|e| e.ts).collect();
        assert_eq!(ts, vec![9, 1]);
    }
}
