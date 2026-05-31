use rustc_hash::FxHashMap as HashMap;
use serde::{Deserialize, Serialize};

use crate::arch::MiscRegs;
use crate::debug::statistics::{EventType, Statistics};
use crate::{arch, parameter};

use super::{AbstractMMU, MMUFlushMode, MMUTranslationResult};
use super::tlb::{self, AddressSpaceID, TLB};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[repr(align(64))]
pub struct OrdinaryMMU<
    ARCH: arch::ISA,
    const I_T_ASSO: usize = 64,
    const I_T_SETS: usize = 1,
    const D_T_ASSO: usize = 64,
    const D_T_SETS: usize = 1,
    const S_TLB_ENABLED: bool = true,
    const S_T_ASSO: usize = 16,
    const S_T_SETS: usize = 1024,
    const NO_HUGE_PAGE: bool = false,
> {
    itlb: TLB<I_T_SETS, I_T_ASSO>,
    dtlb: TLB<D_T_SETS, D_T_ASSO>,
    stlb: TLB<S_T_SETS, S_T_ASSO>,
    htbl_2mb: HashMap<u64, (AddressSpaceID, u64)>, // Currently, we just use a simple hashmap to store the 2MB page table.
    htlb_1gb: HashMap<u64, (AddressSpaceID, u64)>, // Same to the 2MB page table.
    arch: std::marker::PhantomData<ARCH>,
    // other MMU caches can be also added here as well.
}

impl<
    ARCH: arch::ISA,
    const I_T_A: usize,
    const I_T_S: usize,
    const D_T_A: usize,
    const D_T_S: usize,
    const S_ENABLED: bool,
    const S_T_A: usize,
    const S_T_S: usize,
    const NO_HUGE_PAGE: bool,
> OrdinaryMMU<ARCH, I_T_A, I_T_S, D_T_A, D_T_S, S_ENABLED, S_T_A, S_T_S, NO_HUGE_PAGE>
{
    fn refill_4k_tlb(
        &mut self,
        vpn: u64,
        asid: AddressSpaceID,
        ppn: u64,
        ts: u64,
        is_instruction: bool,
        misc_regs: MiscRegs,
    ) {
        if parameter::L1TLB_ENABLED {
            if is_instruction {
                self.itlb.insert(vpn, asid, ppn, ts, is_instruction, misc_regs.clone());
            } else {
                self.dtlb.insert(vpn, asid, ppn, ts, is_instruction, misc_regs.clone());
            }
        }

        if S_ENABLED {
            self.stlb.insert(vpn, asid, ppn, ts, is_instruction, misc_regs);
        }
    }
}

impl<
    ARCH: arch::ISA,
    const I_T_A: usize,
    const I_T_S: usize,
    const D_T_A: usize,
    const D_T_S: usize,
    const S_ENABLED: bool,
    const S_T_A: usize,
    const S_T_S: usize,
    const NO_HUGE_PAGE: bool,
> AbstractMMU
    for OrdinaryMMU<ARCH, I_T_A, I_T_S, D_T_A, D_T_S, S_ENABLED, S_T_A, S_T_S, NO_HUGE_PAGE>
{
    fn new() -> Self {
        Self {
            itlb: TLB::new(),
            dtlb: TLB::new(),
            stlb: TLB::new(),
            htbl_2mb: HashMap::default(),
            htlb_1gb: HashMap::default(),
            arch: std::marker::PhantomData,
        }
    }

    fn flush(&mut self, mode: MMUFlushMode) {
        // Huge TLB currently are just blindly flushed.
        if !NO_HUGE_PAGE {
            self.htbl_2mb.clear();
            self.htlb_1gb.clear();
        }

        if parameter::L1TLB_ENABLED {
            // Forward the flush to each TLB.
            self.itlb.flush(mode);
            self.dtlb.flush(mode);
        }

        if S_ENABLED {
            self.stlb.flush(mode);
        }
    }

    fn translate_and_refill(
        &mut self,
        core_id: u32,
        va: u64,
        ts: u64,
        is_instruction: bool,
    ) -> MMUTranslationResult {
        // First, we try 4KB page.
        let vpn = va >> 12;
        let asid = tlb::AddressSpaceID::NonGlobal(ARCH::get_asid()); // We will start with a non-global ASID. It can still match the global ASID.

        let misc_regs = ARCH::get_misc_regs();

        let is_kernel = (vpn >> 51) == 1;

        Statistics::global_record(core_id, EventType::TLBAccess, is_kernel);

        if is_instruction {
            Statistics::global_record(core_id, EventType::TLBAccessDueToInstruction, is_kernel);
        } else {
            Statistics::global_record(core_id, EventType::TLBAccessDueToData, is_kernel);
        }

        if parameter::L1TLB_ENABLED {
            // First, check the L1 TLB.
            if is_instruction {
                if let Some(ppn) = self.itlb.lookup(vpn, asid, ts, is_instruction) {
                    let pa = ppn << 12 | (va & 0xfff);

                    if parameter::COMPARE_TRANSLATION_RESULT_WITH_WALKER {
                        assert_eq!(pa, ARCH::translate_in_pt(va));
                    }

                    return MMUTranslationResult::Hit(pa, 1);
                }
            } else if let Some(ppn) = self.dtlb.lookup(vpn, asid, ts, is_instruction) {
                let pa = ppn << 12 | (va & 0xfff);

                if parameter::COMPARE_TRANSLATION_RESULT_WITH_WALKER {
                    assert_eq!(pa, ARCH::translate_in_pt(va));
                }

                return MMUTranslationResult::Hit(pa, 1);
            }
        }

        if S_ENABLED {
            // Then, we try L2 TLB.
            if let Some(ppn) = self.stlb.lookup(vpn, asid, ts, is_instruction) {
                let pa = ppn << 12 | (va & 0xfff);

                if parameter::COMPARE_TRANSLATION_RESULT_WITH_WALKER {
                    assert_eq!(pa, ARCH::translate_in_pt(va));
                }

                if parameter::L1TLB_ENABLED {
                    // Insert the result into L1 TLB.
                    if is_instruction {
                        self.itlb.insert(vpn, asid, ppn, ts, is_instruction, misc_regs);
                    } else {
                        self.dtlb.insert(vpn, asid, ppn, ts, is_instruction, misc_regs);
                    }
                }

                return MMUTranslationResult::Hit(pa, 2);
            }
        }

        if !NO_HUGE_PAGE {
            // Then, we try 2MB page.
            let vpn_2mb = vpn >> 9;
            if let Some(ppn) = self.htbl_2mb.get(&vpn_2mb) {
                if ppn.0.check(&asid) {
                    let pa = ppn.1 << 21 | (va & 0x1fffff);

                    Statistics::global_record(core_id, EventType::HugeTLBHit, is_kernel);

                    if is_instruction {
                        Statistics::global_record(
                            core_id,
                            EventType::HugeTLBHitDueToInstruction,
                            is_kernel,
                        );
                    } else {
                        Statistics::global_record(
                            core_id,
                            EventType::HugeTLBHitDueToData,
                            is_kernel,
                        );
                    }

                    if parameter::COMPARE_TRANSLATION_RESULT_WITH_WALKER {
                        assert_eq!(pa, ARCH::translate_in_pt(va));
                    }

                    return MMUTranslationResult::Hit(pa, 3);
                }
            }

            // Then, we try 1GB page.
            let vpn_1gb = vpn >> 18;
            if let Some(ppn) = self.htlb_1gb.get(&vpn_1gb) {
                if ppn.0.check(&asid) {
                    Statistics::global_record(core_id, EventType::HugeTLBHit, is_kernel);

                    if is_instruction {
                        Statistics::global_record(
                            core_id,
                            EventType::HugeTLBHitDueToInstruction,
                            is_kernel,
                        );
                    } else {
                        Statistics::global_record(
                            core_id,
                            EventType::HugeTLBHitDueToData,
                            is_kernel,
                        );
                    }

                    let pa = ppn.1 << 30 | (va & 0x3fffffff);

                    if parameter::COMPARE_TRANSLATION_RESULT_WITH_WALKER {
                        assert_eq!(pa, ARCH::translate_in_pt(va));
                    }

                    return MMUTranslationResult::Hit(pa, 3);
                }
            }

            Statistics::global_record(core_id, EventType::TLBMiss, is_kernel);
            if is_instruction {
                Statistics::global_record(core_id, EventType::TLBMissDueToInstruction, is_kernel);
            } else {
                Statistics::global_record(core_id, EventType::TLBMissDueToData, is_kernel);
            }

            let ptw_result = ARCH::ptw(va);

            let asid = if ptw_result.is_global {
                AddressSpaceID::Global
            } else {
                asid
            };

            if is_kernel {
                // kernel space has to be global address.
                assert!(matches!(asid, AddressSpaceID::Global));
            }

            // based on the ptw_result, we refill each TLB correspondingly.
            match ptw_result.page_size {
                arch::PageSize::_4KB => {
                    self.refill_4k_tlb(vpn, asid, ptw_result.paddr >> 12, ts, is_instruction, misc_regs);
                }
                arch::PageSize::_2MB => {
                    self.htbl_2mb
                        .insert(vpn_2mb, (asid, ptw_result.paddr >> 21));
                }
                arch::PageSize::_1GB => {
                    self.htlb_1gb
                        .insert(vpn_1gb, (asid, ptw_result.paddr >> 30));
                }
            }

            if ptw_result.cacheable {
                MMUTranslationResult::Miss(ptw_result.paddr, ptw_result.traces)
            } else {
                MMUTranslationResult::MissNotCacheable(ptw_result.paddr)
            }
        } else {
            Statistics::global_record(core_id, EventType::TLBMiss, is_kernel);
            if is_instruction {
                Statistics::global_record(core_id, EventType::TLBMissDueToInstruction, is_kernel);
            } else {
                Statistics::global_record(core_id, EventType::TLBMissDueToData, is_kernel);
            }

            let ptw_result = ARCH::ptw(va);

            let asid = if ptw_result.is_global {
                AddressSpaceID::Global
            } else {
                asid
            };

            if is_kernel {
                // kernel space has to be global address.
                assert!(matches!(asid, AddressSpaceID::Global));
            }

            // based on the ptw_result, we refill each TLB correspondingly.
            self.refill_4k_tlb(vpn, asid, ptw_result.paddr >> 12, ts, is_instruction, misc_regs);

            if ptw_result.cacheable {
                MMUTranslationResult::Miss(ptw_result.paddr, ptw_result.traces)
            } else {
                MMUTranslationResult::MissNotCacheable(ptw_result.paddr)
            }
        }
    }

    fn lookup(&mut self, vpn: u64, ts: u64, is_instruction: bool) -> Option<u64> {
        let asid = tlb::AddressSpaceID::NonGlobal(ARCH::get_asid());

        self.stlb.lookup(vpn, asid, ts, is_instruction)
    }

    fn serialize(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap()
    }

    fn deserialize(&mut self, value: serde_json::Value) {
        *self = serde_json::from_value(value).unwrap();
    }
}
