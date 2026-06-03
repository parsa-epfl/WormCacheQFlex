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

use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use serde_with::serde_as;
use std::collections::VecDeque;

use crate::arch::MiscRegs;
use super::MMUFlushMode;

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Copy, Eq, Hash)]
pub enum AddressSpaceID {
    Global,
    NonGlobal(u16),
}

impl AddressSpaceID {
    #[inline]
    pub fn check(&self, other: &AddressSpaceID) -> bool {
        match self {
            // The hit condition is calculated from the following rule:
            // - If the entry is global, it is a hit.
            // - If the entry is not global, it is a hit if the ASID matches.
            AddressSpaceID::Global => true,
            AddressSpaceID::NonGlobal(_) => self == other,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TLBEntry {
    pub valid: bool,
    pub ts: u64,
    pub asid: AddressSpaceID,
    pub vpn: u64,
    pub ppn: u64,
    pub is_instruction: bool,
    #[serde(default)]
    pub misc_regs: MiscRegs,
}

#[serde_as]
#[derive(Debug, Serialize, Deserialize, Clone)]
struct TLBSet<const ASSO: usize> {
    #[serde_as(as = "[_; ASSO]")]
    entries: [TLBEntry; ASSO],
    current_pointer: usize,
}

impl<const ASSO: usize> TLBSet<ASSO> {
    pub fn new() -> Self {
        Self {
            entries: std::array::from_fn(|_| TLBEntry {
                valid: false,
                ts: 0,
                asid: AddressSpaceID::Global,
                vpn: 0,
                ppn: 0,
                is_instruction: false,
                misc_regs: MiscRegs::default(),
            }),
            current_pointer: 0,
        }
    }

    pub fn lookup(
        &mut self,
        vpn: u64,
        asid: AddressSpaceID,
        ts: u64,
        is_instruction: bool,
        misc_regs: &MiscRegs,
    ) -> Option<u64> {
        // TODO: This function is badly implemented. Currently its algorithm complexity is O(n).
        // This will be a problem for 64 entry TLB sets, but whatever. A good design will be implemented later.
        for entry in self.entries.iter_mut() {
            if entry.valid && entry.vpn == vpn && entry.asid.check(&asid)
                && entry.misc_regs == *misc_regs
            {
                assert!(
                    entry.ts <= ts,
                    "TLB entry is older than the current timestamp.",
                );
                // assert!(entry.is_instruction == is_instruction);
                entry.ts = ts;
                entry.is_instruction = is_instruction;
                return Some(entry.ppn);
            }
        }
        None
    }

    pub fn insert(
        &mut self,
        vpn: u64,
        asid: AddressSpaceID,
        ppn: u64,
        ts: u64,
        is_instruction: bool,
        misc_regs: MiscRegs,
    ) {
        if self.current_pointer < ASSO {
            self.entries[self.current_pointer].valid = true;
            self.entries[self.current_pointer].ts = ts;
            self.entries[self.current_pointer].asid = asid;
            self.entries[self.current_pointer].vpn = vpn;
            self.entries[self.current_pointer].ppn = ppn;
            self.entries[self.current_pointer].is_instruction = is_instruction;
            self.entries[self.current_pointer].misc_regs = misc_regs;
            self.current_pointer += 1;
        } else {
            // find a victim.
            let mut victim_idx = 0;
            for i in 0..ASSO {
                if self.entries[i].ts < self.entries[victim_idx].ts {
                    victim_idx = i;
                }
            }
            self.entries[victim_idx].valid = true;
            self.entries[victim_idx].ts = ts;
            self.entries[victim_idx].asid = asid;
            self.entries[victim_idx].vpn = vpn;
            self.entries[victim_idx].ppn = ppn;
            self.entries[victim_idx].is_instruction = is_instruction;
            self.entries[victim_idx].misc_regs = misc_regs;
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TLB<const SET_COUNT: usize, const ASSO: usize> {
    entries: Vec<TLBSet<ASSO>>,
}

impl<const SET_COUNT: usize, const ASSO: usize> TLB<SET_COUNT, ASSO> {
    pub fn new() -> Self {
        Self {
            entries: (0..SET_COUNT).map(|_| TLBSet::new()).collect(),
        }
    }

    pub fn peek(
        &mut self,
        vpn: u64,
        asid: AddressSpaceID,
        misc_regs: &MiscRegs,
    ) -> Option<(u64, AddressSpaceID, &mut u64)> {
        let set_index = vpn % SET_COUNT as u64;
        let set = &mut self.entries[set_index as usize];
        for entry in set.entries.iter_mut() {
            if entry.valid && entry.vpn == vpn && entry.asid.check(&asid)
                && entry.misc_regs == *misc_regs
            {
                return Some((entry.ppn, entry.asid, &mut entry.ts));
            }
        }
        None
    }

    pub fn lookup(
        &mut self,
        vpn: u64,
        asid: AddressSpaceID,
        ts: u64,
        is_instruction: bool,
        misc_regs: &MiscRegs,
    ) -> Option<u64> {
        let set_index = vpn % SET_COUNT as u64;
        let set = &mut self.entries[set_index as usize];
        set.lookup(vpn, asid, ts, is_instruction, misc_regs)
    }

    pub fn insert(
        &mut self,
        vpn: u64,
        asid: AddressSpaceID,
        ppn: u64,
        ts: u64,
        is_instruction: bool,
        misc_regs: MiscRegs,
    ) {
        let set_index = vpn % SET_COUNT as u64;
        let set = &mut self.entries[set_index as usize];
        set.insert(vpn, asid, ppn, ts, is_instruction, misc_regs);
    }

    pub fn flush(&mut self, mode: MMUFlushMode) {
        match mode {
            MMUFlushMode::All => {
                // clean all TLB entries.
                for set in self.entries.iter_mut() {
                    for entry in set.entries.iter_mut() {
                        entry.valid = false;
                        entry.ts = 0;
                    }
                }
            }
            MMUFlushMode::ByASID(address_space_id) => {
                // clean all TLB entries with the given ASID.
                for set in self.entries.iter_mut() {
                    for entry in set.entries.iter_mut() {
                        if entry.asid == address_space_id {
                            entry.valid = false;
                            entry.ts = 0;
                        }
                    }
                }
            }
            MMUFlushMode::ByVPN(vpn, page_count) => {
                for each_page in 0..page_count {
                    let vpn = vpn + each_page;
                    let set_index = vpn % SET_COUNT as u64;
                    let set = &mut self.entries[set_index as usize];
                    for entry in set.entries.iter_mut() {
                        if entry.vpn == vpn {
                            entry.valid = false;
                            entry.ts = 0;
                            break;
                        }
                    }
                }
            }
            MMUFlushMode::ByVPNAndASID(vpn, page_count, address_space_id) => {
                for each_page in 0..page_count {
                    let vpn = vpn + each_page;
                    let set_index = vpn % SET_COUNT as u64;
                    let set = &mut self.entries[set_index as usize];
                    for entry in set.entries.iter_mut() {
                        if entry.vpn == vpn && entry.asid == address_space_id {
                            entry.valid = false;
                            entry.ts = 0;
                            break;
                        }
                    }
                }
            }
        }
    }
}

impl<const SET_COUNT: usize, const ASSO: usize> Default for TLB<SET_COUNT, ASSO> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tlbset_new() {
        let tlbset: TLBSet<4> = TLBSet::new();
        assert_eq!(tlbset.current_pointer, 0);
        assert_eq!(tlbset.entries.len(), 4);
    }

    #[test]
    fn test_tlbset_insert_and_lookup() {
        let mut tlbset: TLBSet<4> = TLBSet::new();
        tlbset.insert(1, AddressSpaceID::NonGlobal(1), 1, 1, false, MiscRegs::default());
        assert_eq!(
            tlbset.lookup(1, AddressSpaceID::NonGlobal(1), 2, false, &MiscRegs::default()),
            Some(1)
        );
    }

    #[test]
    fn test_tlb_new() {
        let tlb: TLB<4, 4> = TLB::new();
        assert_eq!(tlb.entries.len(), 4);
    }

    #[test]
    fn test_tlb_insert_and_lookup() {
        let mut tlb: TLB<4, 4> = TLB::new();
        tlb.insert(1, AddressSpaceID::NonGlobal(1), 1, 1, false, MiscRegs::default());
        assert_eq!(
            tlb.lookup(1, AddressSpaceID::NonGlobal(1), 2, false, &MiscRegs::default()),
            Some(1)
        );
    }

    #[test]
    fn test_tlb_lookup_requires_matching_misc_regs() {
        let mut tlb: TLB<1, 4> = TLB::new();
        let mut inserted_regs = MiscRegs::default();
        inserted_regs.ttbr0_el1 = 0x1000;
        let mut probed_regs = MiscRegs::default();
        probed_regs.ttbr0_el1 = 0x2000;

        tlb.insert(
            1,
            AddressSpaceID::NonGlobal(1),
            1,
            1,
            false,
            inserted_regs,
        );

        assert_eq!(
            tlb.lookup(1, AddressSpaceID::NonGlobal(1), 2, false, &probed_regs),
            None
        );
    }

    #[test]
    fn test_tlbset_replacement_policy() {
        let mut tlbset: TLBSet<4> = TLBSet::new();
        tlbset.insert(1, AddressSpaceID::NonGlobal(1), 1, 1, false, MiscRegs::default());
        tlbset.insert(2, AddressSpaceID::NonGlobal(2), 2, 2, false, MiscRegs::default());
        tlbset.insert(3, AddressSpaceID::NonGlobal(3), 3, 3, false, MiscRegs::default());
        tlbset.insert(4, AddressSpaceID::NonGlobal(4), 4, 4, false, MiscRegs::default());
        tlbset.insert(5, AddressSpaceID::NonGlobal(5), 5, 5, false, MiscRegs::default()); // This should replace the first entry

        // The first entry should be replaced, so the lookup should return None
        assert_eq!(
            tlbset.lookup(1, AddressSpaceID::NonGlobal(1), 2, false, &MiscRegs::default()),
            None
        );
    }

    #[test]
    fn test_tlb_replacement_policy() {
        let mut tlb: TLB<2, 4> = TLB::new();

        // Insert 16 entries, causing multiple replacements
        for i in 0..16 {
            tlb.insert(i, AddressSpaceID::NonGlobal(i as u16), i, i, false, MiscRegs::default());
        }

        // The first 4 entries should have been replaced in each set, so their lookups should return None
        for i in 0..4 {
            assert_eq!(
                tlb.lookup(i, AddressSpaceID::NonGlobal(i as u16), 100 + 1, false, &MiscRegs::default()),
                None
            );
        }

        // The last 4 entries in each set should still be in the TLB, so their lookups should return their values
        for i in 12..16 {
            assert_eq!(
                tlb.lookup(i, AddressSpaceID::NonGlobal(i as u16), 200 + i, false, &MiscRegs::default()),
                Some(i)
            );
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FullyAssociativeTLBEntry {
    pub ts: u64,
    pub ppn: u64,
    pub misc_regs: MiscRegs,
}

#[serde_as]
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FullyAssociativeTLB {
    pub elements: FxHashMap<u64, FullyAssociativeTLBEntry>,
    associativity: usize,
    deferred_elements_exist: bool,
}

impl FullyAssociativeTLB {
    pub fn new(asso: usize) -> Self {
        Self {
            elements: FxHashMap::default(),
            associativity: asso,
            deferred_elements_exist: false,
        }
    }

    #[inline]
    pub fn pack_hash(vpn: u64, asid: AddressSpaceID) -> u64 {
        // We have 48-bit VA and 16-bit ASID.
        // The VPN has been removed from the page offset, which is 12 bits. As a result, the VPN is 36 bits.
        // We need one additional bit for the kernel, so 37 bits in the VPN
        // ASID has to be 16 bits. One additional bit for the global state, so 17 bits.
        // So in total, we have 54 bits.
        // Hash[0]: global
        // Hash[37:1]: VPN
        // Hash[53:38]: ASID

        let vpn = vpn & 0x1FFFFFFFFF;
        match asid {
            AddressSpaceID::Global => (vpn << 1) | 0x1,
            AddressSpaceID::NonGlobal(asid) => (vpn << 1) | ((asid as u64) << 38),
        }
    }

    #[inline]
    pub fn unpack_hash(hash: u64) -> (u64, AddressSpaceID) {
        let vpn = (hash >> 1) & 0x1FFFFFFFFF;
        // run sign extension of 37th bit
        let vpn = if (vpn & (1 << 36)) != 0 {
            vpn | 0xF_FFFF_FFFF_0000 // stay 48 bits
        } else {
            vpn
        };

        let asid = match hash & 0x1 {
            0 => AddressSpaceID::NonGlobal((hash >> 38) as u16),
            1 => AddressSpaceID::Global,
            _ => unreachable!(),
        };
        (vpn, asid)
    }

    #[inline]
    pub fn lookup(&mut self, vpn: u64, asid: u16, ts: u64, misc_regs: &MiscRegs) -> Option<u64> {
        assert!(!self.deferred_elements_exist);
        let is_os = vpn >> 51 == 1;

        let hash = if is_os {
            Self::pack_hash(vpn, AddressSpaceID::Global)
        } else {
            Self::pack_hash(vpn, AddressSpaceID::NonGlobal(asid))
        };

        if let Some(entry) = self.elements.get_mut(&hash) {
            if entry.misc_regs == *misc_regs {
                entry.ts = ts;
                return Some(entry.ppn);
            }
        }

        if !is_os {
            // We try the global ASID
            let hash = Self::pack_hash(vpn, AddressSpaceID::Global);
            if let Some(entry) = self.elements.get_mut(&hash) {
                if entry.misc_regs == *misc_regs {
                    entry.ts = ts;
                    return Some(entry.ppn);
                }
            }
        }

        None
    }

    // conservative insertion. It should be only used for testing.
    #[inline]
    pub fn insert(
        &mut self,
        vpn: u64,
        asid: AddressSpaceID,
        ts: u64,
        ppn: u64,
        misc_regs: MiscRegs,
    ) {
        assert!(!self.deferred_elements_exist);
        let hash = Self::pack_hash(vpn, asid);
        if let Some(entry) = self.elements.get_mut(&hash) {
            entry.ts = ts;
            entry.misc_regs = misc_regs;
            return;
        }

        self.elements
            .insert(hash, FullyAssociativeTLBEntry { ts, ppn, misc_regs });

        self.run_lru();
    }

    #[inline]
    pub fn deferred_insert(
        &mut self,
        vpn: u64,
        asid: AddressSpaceID,
        ts: u64,
        ppn: u64,
        misc_regs: MiscRegs,
    ) -> bool {
        let hash = Self::pack_hash(vpn, asid);
        if self
            .elements
            .insert(hash, FullyAssociativeTLBEntry { ts, ppn, misc_regs })
            .is_none()
        {
            self.deferred_elements_exist = true;
            return false;
        }
        true
    }

    #[inline]
    pub fn run_lru(&mut self) {
        self.deferred_elements_exist = false;

        if self.elements.len() <= self.associativity {
            return;
        }

        // Keep the most recent `asso` entries.
        let mut ts_stack = VecDeque::new();
        for (_, entry) in self.elements.iter() {
            ts_stack.push_back(entry.ts);
        }

        ts_stack.make_contiguous().sort_unstable();

        let threshold = ts_stack[ts_stack.len() - self.associativity];

        self.elements.retain(|_, entry| entry.ts >= threshold);

        assert!(self.elements.len() <= self.associativity);
    }

    pub fn flush(&mut self, mode: MMUFlushMode) {
        match mode {
            MMUFlushMode::All => {
                self.elements.clear();
            }
            MMUFlushMode::ByASID(required_asid) => {
                self.elements.retain(|hash, _| {
                    let asid = match hash & 0x3FFFFF0000000 {
                        0 => AddressSpaceID::Global,
                        _ => AddressSpaceID::NonGlobal((hash >> 38) as u16),
                    };
                    asid != required_asid
                });
            }
            MMUFlushMode::ByVPN(vpn, page_count) => {
                for each_page in 0..page_count {
                    let vpn = vpn + each_page;
                    let hash = Self::pack_hash(vpn, AddressSpaceID::Global);
                    self.elements.remove(&hash);
                    let hash = Self::pack_hash(vpn, AddressSpaceID::NonGlobal(0));
                    self.elements.remove(&hash);
                }
            }
            MMUFlushMode::ByVPNAndASID(vpn, page_count, address_space_id) => {
                let asid = match address_space_id {
                    AddressSpaceID::Global => AddressSpaceID::Global,
                    AddressSpaceID::NonGlobal(asid) => AddressSpaceID::NonGlobal(asid),
                };
                for each_page in 0..page_count {
                    let vpn = vpn + each_page;
                    let hash = Self::pack_hash(vpn, asid);
                    self.elements.remove(&hash);
                }
            }
        }
    }
}

#[cfg(test)]
mod fa_tlb_tests {
    use super::*;

    #[test]
    fn test_fa_tlbset_insert_and_lookup() {
        let mut tlbset: FullyAssociativeTLB = FullyAssociativeTLB::new(4);
        tlbset.insert(1, AddressSpaceID::NonGlobal(1), 1, 1, MiscRegs::default());
        assert_eq!(tlbset.lookup(1, 1, 2, &MiscRegs::default()), Some(1));
    }

    #[test]
    fn test_fa_tlb_lookup_requires_matching_misc_regs() {
        let mut tlbset: FullyAssociativeTLB = FullyAssociativeTLB::new(4);
        let mut inserted_regs = MiscRegs::default();
        inserted_regs.ttbr0_el1 = 0x1000;
        let mut probed_regs = MiscRegs::default();
        probed_regs.ttbr0_el1 = 0x2000;

        tlbset.insert(1, AddressSpaceID::NonGlobal(1), 1, 1, inserted_regs);

        assert_eq!(tlbset.lookup(1, 1, 2, &probed_regs), None);
    }

    #[test]
    fn test_fa_tlbset_replacement_policy() {
        let mut tlbset: FullyAssociativeTLB = FullyAssociativeTLB::new(4);
        tlbset.insert(1, AddressSpaceID::NonGlobal(1), 1, 1, MiscRegs::default());
        tlbset.insert(2, AddressSpaceID::NonGlobal(2), 2, 2, MiscRegs::default());
        tlbset.insert(3, AddressSpaceID::NonGlobal(3), 3, 3, MiscRegs::default());
        tlbset.insert(4, AddressSpaceID::NonGlobal(4), 4, 4, MiscRegs::default());
        tlbset.insert(5, AddressSpaceID::NonGlobal(5), 5, 5, MiscRegs::default()); // This should replace the first entry

        // The first entry should be replaced, so the lookup should return None
        assert_eq!(tlbset.lookup(1, 1, 2, &MiscRegs::default()), None);
    }

    #[test]
    fn test_deferred_insertion() {
        let mut tlbset: FullyAssociativeTLB = FullyAssociativeTLB::new(4);
        tlbset.deferred_insert(1, AddressSpaceID::NonGlobal(1), 1, 1, MiscRegs::default());
        tlbset.deferred_insert(2, AddressSpaceID::NonGlobal(2), 2, 2, MiscRegs::default());
        tlbset.deferred_insert(3, AddressSpaceID::NonGlobal(3), 3, 3, MiscRegs::default());
        tlbset.deferred_insert(4, AddressSpaceID::NonGlobal(4), 4, 4, MiscRegs::default());
        tlbset.deferred_insert(5, AddressSpaceID::NonGlobal(5), 5, 5, MiscRegs::default());

        tlbset.run_lru();

        assert_eq!(tlbset.lookup(1, 1, 2, &MiscRegs::default()), None);

        assert_eq!(tlbset.lookup(2, 2, 2, &MiscRegs::default()), Some(2));
    }

    #[test]
    fn test_fa_tlb_lru_promotion() {
        let mut tlbset: FullyAssociativeTLB = FullyAssociativeTLB::new(4);

        tlbset.insert(1, AddressSpaceID::NonGlobal(1), 1, 1, MiscRegs::default());
        tlbset.insert(2, AddressSpaceID::NonGlobal(2), 2, 2, MiscRegs::default());
        tlbset.insert(3, AddressSpaceID::NonGlobal(3), 3, 3, MiscRegs::default());
        tlbset.insert(4, AddressSpaceID::NonGlobal(4), 4, 4, MiscRegs::default());

        // Access the first entry to promote it to the MRU position
        tlbset.lookup(1, 1, 5, &MiscRegs::default());

        // Insert a new entry, causing the first entry to be replaced
        tlbset.insert(5, AddressSpaceID::NonGlobal(5), 6, 5, MiscRegs::default());

        // The second entry should have been replaced, so the lookup should return None
        assert_eq!(tlbset.lookup(2, 2, 2, &MiscRegs::default()), None);
    }

    #[test]
    fn test_fa_tlb_hash() {
        // We start with a simple 48-bit VPN and 16-bit ASID.
        // It is a userspace address, so the global bit is 0.
        let hash = FullyAssociativeTLB::pack_hash(0x0FFFFFFFFF, AddressSpaceID::NonGlobal(0x3333));
        let (vpn, asid) = FullyAssociativeTLB::unpack_hash(hash);
        assert_eq!(vpn, 0x0FFFFFFFFF);
        assert_eq!(asid, AddressSpaceID::NonGlobal(0x3333));

        // Then, test an kernel address, with high 16bit set to 1.
        let hash = FullyAssociativeTLB::pack_hash(0xFFFFFFFFFFFFF, AddressSpaceID::Global);
        let (vpn, asid) = FullyAssociativeTLB::unpack_hash(hash);
        assert_eq!(vpn, 0xFFFFFFFFFFFFF);
        assert_eq!(asid, AddressSpaceID::Global);
    }

    #[test]
    fn test_fa_tlb_hit_insertion() {
        let mut tlbset = FullyAssociativeTLB::new(4);

        tlbset.insert(1, AddressSpaceID::NonGlobal(1), 1, 1, MiscRegs::default());
        tlbset.insert(2, AddressSpaceID::NonGlobal(2), 2, 2, MiscRegs::default());
        tlbset.insert(3, AddressSpaceID::NonGlobal(3), 3, 3, MiscRegs::default());
        tlbset.insert(4, AddressSpaceID::NonGlobal(4), 4, 4, MiscRegs::default());

        // Insert an existing entry.
        tlbset.insert(2, AddressSpaceID::NonGlobal(2), 5, 5, MiscRegs::default());

        // Is vpn 1 still a hit?
        assert_eq!(tlbset.lookup(1, 1, 6, &MiscRegs::default()), Some(1));
    }
}
