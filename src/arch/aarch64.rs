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

// This file defines the page walk logic of aarch64.

// We only handle the most basic case: 48bit VA, 4KB page size, 4-level page table, with huge page support.

use std::ffi::c_void;

use serde::{Deserialize, Serialize};
use crate::{arch::PageSize, qemu_api};

use super::{ISA, TranslationResult};

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct MiscRegs {
    pub cpsr: u64,
    pub sctlr_el1: u64,
    pub tcr_el1: u64,
    pub ttbr0_el1: u64,
    pub ttbr1_el1: u64,
    pub mair_el1: u64,
}

#[derive(Debug)]
pub struct AArch64;

fn paddr_reader(addr: u64) -> u64 {
    // make addr aligned with 8.
    let addr = addr & !0b111;
    let mut buf: u64 = 0;
    unsafe {
        qemu_api::qemu_plugin_read_physical_memory(addr, 8, &mut buf as *mut u64 as *mut c_void);
    }
    buf
}

impl ISA for AArch64 {
    fn get_asid() -> u16 {
        let tcr = unsafe { qemu_api::qemu_plugin_read_tcr_el1() };
        let t1_size = 64 - ((tcr >> 16) & 0b111111);
        let t0_size = 64 - (tcr & 0b111111);

        assert!(t0_size == 48); // the lower 48-bit VA are used for translation.
        assert!(t1_size == 48); // the OS should take over all spaces.
        // How do we decide whether this is a kernel space or a user space?
        // I have to read the two granules.
        let which_ttbr_for_asid = if tcr >> 22 & 0b1 == 1 { 1 } else { 0 };
        unsafe { (qemu_api::qemu_plugin_read_ttbr_el1(which_ttbr_for_asid) >> 48) as u16 }
    }

    fn get_misc_regs() -> MiscRegs {
        MiscRegs {
            cpsr: unsafe { qemu_api::qemu_plugin_read_cpsr() },
            sctlr_el1: unsafe { qemu_api::qemu_plugin_read_sctlr_el1() },
            tcr_el1: unsafe { qemu_api::qemu_plugin_read_tcr_el1() },
            ttbr0_el1: unsafe { qemu_api::qemu_plugin_read_ttbr_el1(0) },
            ttbr1_el1: unsafe { qemu_api::qemu_plugin_read_ttbr_el1(1) },
            mair_el1: unsafe { qemu_api::qemu_plugin_read_mair_el1() },
        }
    }

    fn ptw(va: u64) -> TranslationResult {
        let is_kernel = va & 0xFFFF000000000000 != 0;
        let tcr = unsafe { qemu_api::qemu_plugin_read_tcr_el1() };
        let which_ttbr_for_base = if va < (1 << 48 ) { 0 } else { 1 };
        let ttbr = unsafe { qemu_api::qemu_plugin_read_ttbr_el1(which_ttbr_for_base) };

        let t1_size = 64 - ((tcr >> 16) & 0b111111);
        let t0_size = 64 - (tcr & 0b111111);

        assert!(t0_size == 48); // the lower 48-bit VA are used for translation.
        assert!(t1_size == 48); // the OS should take over all spaces.

        // 0. check whether the page walk access is cachable. If not, we just return all -1.
        let cacheable = if is_kernel {
            let shrability = (tcr >> 28) & 0b11;
            match shrability {
                0b00 => todo!(), // TODO: Add a flag in the cache hierarchy to avoid putting values into the shared cache.
                0b01 => false,
                0b10 => (tcr >> 26) & 0b11 != 0b00,
                0b11 => (tcr >> 24) & 0b11 != 0b00,
                _ => unreachable!(),
            }
        } else {
            let shrability = (tcr >> 12) & 0b11;
            match shrability {
                0b00 => todo!(), // TODO: Add a flag in the cache hierarchy to avoid putting values into the shared cache.
                0b01 => false,
                0b10 => (tcr >> 10) & 0b11 != 0b00,
                0b11 => (tcr >> 8) & 0b11 != 0b00,
                _ => unreachable!(),
            }
        };
        // 1. check whether a TLB miss should trigger an exception. If yes, we also return 4 -1.
        if is_kernel {
            let tlb_miss_exception = (tcr >> 23) & 0b1 != 0;
            assert!(
                !tlb_miss_exception,
                "We do not support TLB miss exception in kernel mode!"
            )
        } else {
            let tlb_miss_exception = (tcr >> 7) & 0b1 != 0;
            assert!(
                !tlb_miss_exception,
                "We do not support TLB miss exception in kernel mode!"
            )
        }

        // 2. check the physical address space. The physical address space should be 48bit. Otherwise, we panic
        let pa_space = (tcr >> 32) & 0b111;
        assert_eq!(
            pa_space, 0b101,
            "We only support 48bit physical address space!"
        );

        // 3. check the granularity of translation. It should be 4K. Otherwise, we panic.
        if is_kernel {
            let granule = (tcr >> 30) & 0b11;
            assert_eq!(granule, 0b10, "We only support 4KB page size!")
        } else {
            let granule = (tcr >> 14) & 0b11;
            assert_eq!(granule, 0b00, "We only support 4KB page size!");
        };

        // 3. check the page table level. It should be 4. Otherwise, we panic.
        if is_kernel {
            let t0sz = 64 - ((tcr >> 16) & 0b111111);
            assert_eq!(t0sz, 48, "We only support 4-level page table!")
        } else {
            let t0sz = 64 - (tcr & 0b111111);
            assert_eq!(t0sz, 48, "We only support 4-level page table!");
        }

        let mut result = TranslationResult {
            paddr: 0,
            is_global: false,
            page_size: PageSize::_4KB,
            traces: [u64::MAX; 4],
            cacheable,
        };

        // Now, we start the real page walk. First, we get the page table base address.
        let ttbr = ttbr & 0x0000FFFFFFFFF000;

        // The first level page table entry is at ttbr + (va >> 39) * 8
        let l0pte_addr = ttbr + ((va >> 39) & 0b111111111) * 8;
        result.traces[0] = l0pte_addr;

        let l0pte = paddr_reader(l0pte_addr);
        // println!("L0 PTE: {:x} -> {:x}", l0pte_addr, l0pte);

        // if l0pte & 0b11 != 0b11 {
        //     // Well, we are done. This is a 1GB page.
        //     return result;
        // }

        assert!(
            l0pte & 0b11 == 0b11,
            "It is impossible to see a 512GB page in AArch64 now!"
        );

        // Now, the second level.
        let l1pte_addr = (l0pte & 0x0000FFFFFFFFF000) + ((va >> 30) & 0b111111111) * 8;
        result.traces[1] = l1pte_addr;
        let l1pte = paddr_reader(l1pte_addr);
        // println!("L1 PTE: {:x} -> {:x}", l1pte_addr, l1pte);

        if l1pte & 0b11 != 0b11 {
            // Well, we are done. This is a 1GB page.
            result.paddr = (l1pte & 0x0000_FFFF_C000_0000) + (va & 0x0000_0000_3FFF_FFFF);
            result.is_global = (l1pte >> 11) & 0b1 == 0;
            result.page_size = PageSize::_1GB;
            return result;
        }

        // Now, the third level.
        let l2pte_addr = (l1pte & 0x0000FFFFFFFFF000) + ((va >> 21) & 0b111111111) * 8;
        result.traces[2] = l2pte_addr;
        let l2pte = paddr_reader(l2pte_addr);
        // println!("L2 PTE: {:x} -> {:x}", l2pte_addr, l2pte);

        if l2pte & 0b11 != 0b11 {
            // Well, we are done. This is a 2MB page.
            result.paddr = (l2pte & 0x0000_FFFF_FFE0_0000) + (va & 0x0000_0000_001F_FFFF);
            result.is_global = (l2pte >> 11) & 0b1 == 0;
            result.page_size = PageSize::_2MB;
            return result;
        }

        // Now, the fourth level.
        let l3pte_addr = (l2pte & 0x0000FFFFFFFFF000) + ((va >> 12) & 0b111111111) * 8;
        result.traces[3] = l3pte_addr;
        let l3pte = paddr_reader(l3pte_addr);
        // println!("L3 PTE: {:x} -> {:x}", l3pte_addr, l3pte);

        // Well, we are done. This is a 4KB page.
        result.paddr = (l3pte & 0x0000FFFFFFFFF000) + (va & 0x0000_0000_0000_0FFF);
        result.page_size = PageSize::_4KB;
        result.is_global = (l3pte >> 11) & 0b1 == 0;

        result
    }
}
