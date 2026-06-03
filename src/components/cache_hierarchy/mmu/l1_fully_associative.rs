use serde::{Deserialize, Serialize};

use crate::arch::MiscRegs;
use crate::{arch, parameter};

use rustc_hash::FxHashMap as HashMap;

use super::{
    AbstractMMU, MMUTranslationResult,
    tlb::{self, AddressSpaceID, FullyAssociativeTLB, TLB},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[repr(align(64))]
pub struct FullyAssociativeL1MMU<
    ARCH: arch::ISA,
    const I_T_ASSO: usize = 64,
    const D_T_ASSO: usize = 64,
    const S_T_ASSO: usize = 16,
    const S_T_SETS: usize = 64,
    const NO_HUGE_PAGE: bool = false,
> {
    l0_itlb: (u64, AddressSpaceID, u64, MiscRegs),
    stlb: TLB<S_T_SETS, S_T_ASSO>,
    itlb: FullyAssociativeTLB,
    dtlb: FullyAssociativeTLB,
    htbl_2m: HashMap<u64, (AddressSpaceID, u64)>,
    htbl_1g: HashMap<u64, (AddressSpaceID, u64)>,
    arch: std::marker::PhantomData<ARCH>,
}

impl<
    ARCH: arch::ISA,
    const I_T_A: usize,
    const D_T_A: usize,
    const S_T_A: usize,
    const S_T_S: usize,
    const NO_HUGE_PAGE: bool,
> FullyAssociativeL1MMU<ARCH, I_T_A, D_T_A, S_T_A, S_T_S, NO_HUGE_PAGE>
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
        self.stlb.insert(vpn, asid, ppn, ts, is_instruction, misc_regs.clone());

        if parameter::L1TLB_ENABLED {
            if is_instruction {
                self.itlb.deferred_insert(vpn, asid, ts, ppn, misc_regs);
            } else {
                self.dtlb.deferred_insert(vpn, asid, ts, ppn, misc_regs);
            }
        }
    }
}

impl<
    ARCH: arch::ISA,
    const I_T_A: usize,
    const D_T_A: usize,
    const S_T_A: usize,
    const S_T_S: usize,
    const NO_HUGE_PAGE: bool,
> AbstractMMU for FullyAssociativeL1MMU<ARCH, I_T_A, D_T_A, S_T_A, S_T_S, NO_HUGE_PAGE>
{
    fn new() -> Self {
        Self {
            l0_itlb: (0, AddressSpaceID::NonGlobal(0), 0, MiscRegs::default()),
            stlb: TLB::new(),
            itlb: FullyAssociativeTLB::new(I_T_A),
            dtlb: FullyAssociativeTLB::new(D_T_A),
            htbl_2m: HashMap::default(),
            htbl_1g: HashMap::default(),
            arch: std::marker::PhantomData,
        }
    }

    fn translate_and_refill(
        &mut self,
        _core_id: u32,
        va: u64,
        ts: u64,
        is_instruction: bool,
    ) -> MMUTranslationResult {
        let vpn = va >> 12;
        let raw_asid = ARCH::get_asid();
        let misc_regs = ARCH::get_misc_regs();

        let trial_asid = tlb::AddressSpaceID::NonGlobal(raw_asid); // We will start with a non-global ASID. It can still match the global ASID.

        if is_instruction {
            // check the L0 ITLB.
            if self.l0_itlb.0 == vpn && self.l0_itlb.1 == trial_asid
                && self.l0_itlb.3 == misc_regs
            {
                let pa = self.l0_itlb.2 << 12 | (va & 0xfff);
                if parameter::COMPARE_TRANSLATION_RESULT_WITH_WALKER {
                    assert_eq!(pa, ARCH::translate_in_pt(va));
                }

                return MMUTranslationResult::Hit(pa, 0);
            }
        }

        // First, we check the L2 TLB.
        if let Some((ppn, asid, stlb_ts)) = self.stlb.peek(vpn, trial_asid, &misc_regs) {
            if parameter::L1TLB_ENABLED {
                if is_instruction {
                    self.itlb.deferred_insert(vpn, asid, ts, ppn, misc_regs.clone())
                } else {
                    self.dtlb.deferred_insert(vpn, asid, ts, ppn, misc_regs.clone())
                };
            }

            *stlb_ts = ts;

            let pa = ppn << 12 | (va & 0xfff);

            if parameter::COMPARE_TRANSLATION_RESULT_WITH_WALKER {
                assert_eq!(pa, ARCH::translate_in_pt(va));
            }

            if is_instruction {
                self.l0_itlb = (vpn, trial_asid, ppn, misc_regs.clone());
            }

            return MMUTranslationResult::Hit(pa, 2);
        }

        if parameter::L1TLB_ENABLED {
            // Alrignt. Then we have to check the L1 TLB, which has higher associativity.
            if is_instruction {
                self.itlb.run_lru();
                if let Some(ppn) = self.itlb.lookup(vpn, raw_asid as u16, ts, &misc_regs) {
                    let pa = ppn << 12 | (va & 0xfff);

                    if parameter::COMPARE_TRANSLATION_RESULT_WITH_WALKER {
                        assert_eq!(pa, ARCH::translate_in_pt(va));
                    }

                    if is_instruction {
                        self.l0_itlb = (vpn, trial_asid, ppn, misc_regs.clone());
                    }

                    return MMUTranslationResult::Hit(pa, 1);
                }
            } else {
                self.dtlb.run_lru();
                if let Some(ppn) = self.dtlb.lookup(vpn, raw_asid as u16, ts, &misc_regs) {
                    let pa = ppn << 12 | (va & 0xfff);

                    if parameter::COMPARE_TRANSLATION_RESULT_WITH_WALKER {
                        assert_eq!(pa, ARCH::translate_in_pt(va));
                    }

                    return MMUTranslationResult::Hit(pa, 1);
                }
            }
        }

        let ptw_result = ARCH::ptw(va);

        let asid = if ptw_result.is_global {
            AddressSpaceID::Global
        } else {
            trial_asid
        };

        if ptw_result.cacheable {
            // based on the ptw_result, we refill each TLB correspondingly.
            self.refill_4k_tlb(
                vpn,
                asid,
                ptw_result.paddr >> 12,
                ts,
                is_instruction,
                misc_regs.clone(),
            );
            if is_instruction {
                self.l0_itlb = (vpn, trial_asid, ptw_result.paddr >> 12, misc_regs);
            }
            MMUTranslationResult::Miss(ptw_result.paddr, ptw_result.traces)
        } else {
            MMUTranslationResult::MissNotCacheable(ptw_result.paddr)
        }
    }

    fn lookup(&mut self, _vpn: u64, _ts: u64, _is_instruction: bool) -> Option<u64> {
        todo!()
    }

    fn flush(&mut self, mode: super::MMUFlushMode) {
        // invalid l0_itlb
        self.l0_itlb = (0, AddressSpaceID::NonGlobal(0), 0, MiscRegs::default());
        self.itlb.flush(mode);
        self.dtlb.flush(mode);
        self.stlb.flush(mode);
    }

    fn serialize(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap()
    }

    fn deserialize(&mut self, value: serde_json::Value) {
        *self = serde_json::from_value(value).unwrap();
    }
}
