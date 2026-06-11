use rand::SeedableRng;

use crate::{arch::ISA, components::cache_hierarchy::mmu::AbstractMMU, parameter};

struct FakeISA {}

impl ISA for FakeISA {
    fn get_asid() -> u16 {
        0
    }

    fn get_misc_regs() -> crate::arch::MiscRegs {
        crate::arch::MiscRegs::default()
    }

    fn ptw(va: u64) -> crate::arch::TranslationResult {
        crate::arch::TranslationResult {
            paddr: va,
            is_global: false,
            page_size: crate::arch::PageSize::_4KB,
            traces: [0; 4],
            cacheable: true,
        }
    }
}

#[test]
fn test_equivalence_of_two_tlbs() {
    let mut ordinary_tlb = super::TLB::<1, 64>::new();
    let mut fw_tlb = super::FullyAssociativeTLB::new(64);
    use rand::Rng;

    let mut rng = rand::thread_rng();

    for ts in 1..6536 {
        // generate request.
        let request_type = rng.gen_range(0..3);
        if request_type <= 2 {
            // Insertion
            let vpn = rng.gen_range(1..66);
            let asid = super::AddressSpaceID::NonGlobal(0);
            let ppn = rng.gen_range(1..66);

            ordinary_tlb.insert(vpn, asid, ppn, ts, false, crate::arch::MiscRegs::default());
            fw_tlb.deferred_insert(vpn, asid, ts, ppn, crate::arch::MiscRegs::default());
        } else {
            // Lookup
            let vpn = rng.gen_range(1..66);
            let raw_asid = 0;
            let asid = super::AddressSpaceID::NonGlobal(raw_asid);

            let ordinary_result =
                ordinary_tlb.lookup(vpn, asid, ts, false, &crate::arch::MiscRegs::default());

            fw_tlb.run_lru();
            let fw_result = fw_tlb.lookup(vpn, raw_asid, ts, &crate::arch::MiscRegs::default());

            assert_eq!(ordinary_result, fw_result);
        }
    }
}

// The following test should fail because the two MMUs are not equivalent.

#[test]
fn test_equivalence_of_two_mmus() {
    if !parameter::L1TLB_ENABLED {
        return;
    }

    // There are two MMUs in this project:
    // - OrdinaryMMU
    // - FunctionalWarmingMMU
    // They should be equivalent in terms of the translation result.
    // This test checks the equivalence of the two MMUs.
    type OrdinaryMMU = super::OrdinaryMMU<FakeISA, 64, 1, 64, 1, true, 4, 128, true>;

    type FWMMU = super::FullyAssociativeL1MMU<FakeISA, 64, 64, 4, 128, true>;

    let mut o_mmu = OrdinaryMMU::new();
    let mut fw_mmu = FWMMU::new();

    // generate translation request to two MMUs and compare their translaton results
    use rand::Rng;
    // create a random number generator with a fixed seed.
    let mut rng = rand::rngs::StdRng::seed_from_u64(0);

    for ts in 1..65536 {
        // 80% test 0 to 67, and 15 for 68 to 514, and 5% for 515 to 2048.
        let vpn = match rng.gen_range(0..100) {
            0..=79 => rng.gen_range(0..68),
            80..=94 => rng.gen_range(68..515),
            _ => rng.gen_range(515..2049),
        };

        let offset = rng.gen_range(0..4096);

        let va = (vpn << 12) | offset;

        let is_instruction = false;

        let o_result = o_mmu.translate_and_refill(0, va, ts, is_instruction);
        let fw_result = fw_mmu.translate_and_refill(0, va, ts, is_instruction);

        match (o_result, fw_result) {
            (
                super::MMUTranslationResult::Hit(o_ppn, _),
                super::MMUTranslationResult::Hit(fw_ppn, _),
            ) => {
                assert_eq!(o_ppn, fw_ppn);
            }
            (
                super::MMUTranslationResult::Miss(o_ppn, o_traces),
                super::MMUTranslationResult::Miss(fw_ppn, fw_traces),
            ) => {
                assert_eq!(o_ppn, fw_ppn);
                assert_eq!(o_traces, fw_traces);
            }
            (
                super::MMUTranslationResult::MissNotCacheable(o_ppn),
                super::MMUTranslationResult::MissNotCacheable(fw_ppn),
            ) => {
                assert_eq!(o_ppn, fw_ppn);
            }
            (
                super::MMUTranslationResult::Hit(_, hit_level),
                super::MMUTranslationResult::Miss(_, _),
            ) => {
                println!("Ordinary MMU: {:?}", o_result);
                println!("FW MMU: {:?}", fw_result);
                assert!(hit_level == 2);
            }
            (
                super::MMUTranslationResult::Miss(_, _),
                super::MMUTranslationResult::Hit(_, hit_level),
            ) => {
                println!("Ordinary MMU: {:?}", o_result);
                println!("FW MMU: {:?}", fw_result);
                assert!(hit_level == 2);
            }
            _ => {
                println!("Ordinary MMU: {:?}", o_result);
                println!("FW MMU: {:?}", fw_result);
                panic!("Translation results are not matched.");
            }
        }
    }
}
