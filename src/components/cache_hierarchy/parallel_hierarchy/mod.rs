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

use core::ffi;
use std::io::Write;

use rustc_hash::FxHashMap;

use crate::{
    parameter::{self, ENABLE_STATISTICS},
    qemu_api,
    timestamp::get_ts,
    util::get_monotonic_ts,
};

use super::{MemoryAccessRequest, MemoryHierarchy, common::CacheAccessType};
use super::{
    common::L0InstructionCache,
    mmu::{MMUFlushMode, tlb::AddressSpaceID},
};

pub mod hierarchy;
pub mod parser;
mod pure_fill;

type HierarchyForPlugin = parser::HierarchyForPlugin;

static mut PLUGIN: *mut HierarchyForPlugin = std::ptr::null_mut();
// static mut DUMMY_PLUGIN: *mut HierarchyForPlugin = std::ptr::null_mut();

// TODO: The QEMU side has to make load-link to get exclusive permission so that the plugin can handle it properly.
unsafe extern "C" fn vcpu_mem_access(
    vcpu_idx: u32,
    info: qemu_api::qemu_plugin_meminfo_t,
    vaddr: u64,
    inst_virtual_addr: *mut ffi::c_void,
) {
    unsafe {
        let hw_handler = qemu_api::qemu_plugin_get_hwaddr(info, vaddr);
        let is_device = qemu_api::qemu_plugin_hwaddr_is_io(hw_handler);

        if !is_device {
            let is_store = qemu_api::qemu_plugin_mem_is_store(info);

            let pa = qemu_api::qemu_plugin_hwaddr_phys_addr(hw_handler);

            let inst_virtual_addr = inst_virtual_addr as u64;
            let is_os = (inst_virtual_addr >> 48) & 1 == 1;

            let ts = get_ts();

            if parameter::MEASURE_HALF_OF_CORES && vcpu_idx >= parameter::CORE_COUNT as u32 / 2 {
                return;
            } else {
                (*PLUGIN).access_memory_with_va_and_pa(
                    &MemoryAccessRequest {
                        core_id: vcpu_idx,
                        va: vaddr,
                        access_type: if is_store {
                            CacheAccessType::DataWrite
                        } else {
                            CacheAccessType::DataRead
                        },
                        is_os,
                    },
                    Some(pa),
                    ts,
                );
            };
        } else {
            // TODO: check the I/O event
        }
    }
}

static mut L0_CACHE: *mut L0InstructionCache<{ parameter::CORE_COUNT }> = std::ptr::null_mut();

unsafe extern "C" fn vcpu_insn_exec(
    vcpu_idx: u32,
    inst_virtual_addr: *mut ffi::c_void, // it is basically its physical address.
) {
    unsafe {
        let vpn = qemu_api::qemu_plugin_read_pc_vpn();
        let vaddr = vpn << 12 | (inst_virtual_addr as u64 & 0xfff);

        if (*L0_CACHE).check_and_update(vcpu_idx, vaddr) {
            return;
        }

        let ts = get_ts();

        if parameter::MEASURE_HALF_OF_CORES && vcpu_idx >= parameter::CORE_COUNT as u32 / 2 {
            return;
        } else {
            (*PLUGIN).access_memory_with_va(
                &MemoryAccessRequest {
                    core_id: vcpu_idx,
                    va: vaddr,
                    access_type: CacheAccessType::InstructionFetch,
                    is_os: vaddr >> 63 == 1,
                },
                ts,
            );
        }
    }
}

// TODO: One additional PluginAPI is needed for this instruction. It will be a similar function to the memory access.
unsafe extern "C" fn _vcpu_invalidate_cache(
    _vcpu_idx: u32,
    _paddr: *mut ffi::c_void, // it is basically its physical address.
) {
}

unsafe extern "C" fn vcpu_invalid_tlb(
    vcpu_idx: u32,
    mode: u32,
    asid: u64,
    vpn: u64,
    page_count: u64,
) {
    unsafe {
        let info = if mode == 0 {
            MMUFlushMode::All
        } else if mode == 1 {
            MMUFlushMode::ByASID(AddressSpaceID::NonGlobal(asid as u16))
        } else if mode == 2 {
            MMUFlushMode::ByVPN(vpn, page_count)
        } else if mode == 3 {
            MMUFlushMode::ByVPNAndASID(vpn, page_count, AddressSpaceID::NonGlobal(asid as u16))
        } else {
            unreachable!()
        };

        if parameter::MEASURE_HALF_OF_CORES && vcpu_idx >= parameter::CORE_COUNT as u32 / 2 {
            // (*DUMMY_PLUGIN).flush_mmu(vcpu_idx - parameter::CORE_COUNT as u32 / 2, info);
        } else {
            (*PLUGIN).flush_mmu(vcpu_idx, info);
        }
    }
}

pub struct ParallelCacheHierarchyPlugin {}

impl super::super::Plugin for ParallelCacheHierarchyPlugin {
    #[inline]
    fn init(_plugin_id: u64, options: &FxHashMap<String, String>) {
        let mode = String::new();
        let mode = options.get("mode").unwrap_or(&mode);
        assert_ne!(
            mode, "vtime",
            "Pure vtime is enabled. Memory Hierarchy should be disabled."
        );

        if mode == "pure_fill" {
            unsafe {
                let prefix = options.get("prefix").unwrap_or(&"init".to_string()).clone();
                let warm_ratio = options
                    .get("warm_ratio")
                    .unwrap_or(&"1.0".to_string())
                    .clone();
                let fallback_cycles = options.get("fallback_cycles").cloned();
                let warm_ratio: f64 = warm_ratio.parse().unwrap();
                let fallback_cycles = fallback_cycles.map(|cycles| {
                    cycles
                        .parse()
                        .expect("fallback_cycles must be a non-negative integer")
                });
                println!(
                    "Pure fill mode is enabled. The cache will be warmed up with the prefix: {}, warm ratio: {}, fallback cycles: {:?}",
                    prefix, warm_ratio, fallback_cycles
                );
                pure_fill::init(&prefix, warm_ratio, fallback_cycles);
            }
        }

        unsafe {
            PLUGIN = Box::into_raw(Box::new(HierarchyForPlugin::new()));
            L0_CACHE = Box::into_raw(Box::new(L0InstructionCache::new()));
            qemu_api::qemu_plugin_register_flushing_local_tlb_cb(Some(vcpu_invalid_tlb));
        }

        if parameter::USE_UNIFIED_CACHE {
            assert!(HierarchyForPlugin::information().contains("UnifiedPrivateCache"))
        } else {
            assert!(HierarchyForPlugin::information().contains("HarvardPrivateCache"))
        }

        if parameter::USE_INFINITE_DIRECTORY {
            assert!(HierarchyForPlugin::information().contains("Infinite Directory"))
        } else {
            assert!(HierarchyForPlugin::information().contains("Finite Directory"))
        }

        println!("Memory plugin initialized.");
        println!("{}", HierarchyForPlugin::information());

        // this thread peridocally dumps the statistics.
        std::thread::spawn(move || {
            if !ENABLE_STATISTICS {
                return;
            }

            // open a csv file.
            let mut warmed_rate = std::fs::File::create("shared_cache_warm_count.csv").unwrap();

            warmed_rate
                .write_all(b"ts,warm_set_count,warm_slot_count\n")
                .unwrap();

            loop {
                let warmed_set = unsafe { (*PLUGIN).get_scache_warmed_set_count() };

                warmed_rate
                    .write_all(
                        format!("{},{},{}\n", get_monotonic_ts(), warmed_set, unsafe {
                            (*PLUGIN).get_scache_warmed_slots_count()
                        })
                        .as_bytes(),
                    )
                    .unwrap();

                std::thread::sleep(std::time::Duration::from_secs(10));
            }
        });
    }

    #[inline]
    unsafe fn on_translation(tb: *mut crate::qemu_api::qemu_plugin_tb) {
        unsafe {
            let n_instruction = qemu_api::qemu_plugin_tb_n_insns(tb);

            if n_instruction == 0 {
                return;
            }

            assert!(n_instruction < 32768);

            let mut block_id = vec![];
            for i in 0..n_instruction {
                let inst = qemu_api::qemu_plugin_tb_get_insn(tb, i);
                block_id.push(
                    qemu_api::qemu_plugin_insn_haddr(inst) as usize
                        >> crate::parameter::CACHE_LINE_SIZE.trailing_zeros(),
                );
            }

            let fb_info = crate::util::find_fetch_block_from_block_id_sequence(block_id);

            // bind the instruction call back.
            for (idx, _) in fb_info.into_iter() {
                let i = qemu_api::qemu_plugin_tb_get_insn(tb, idx);

                let insn_addr = (qemu_api::qemu_plugin_insn_vaddr(i) as u64) & 0x1_ffff_ffff_ffff;
                let offset = idx as u64;
                let combined = insn_addr | (offset << 49);

                qemu_api::qemu_plugin_register_vcpu_insn_exec_cb(
                    i,
                    Some(vcpu_insn_exec),
                    qemu_api::qemu_plugin_cb_flags_QEMU_PLUGIN_CB_NO_REGS,
                    combined as *mut ffi::c_void,
                );
            }

            // bind the memory callback.
            for i in 0..n_instruction {
                let inst = qemu_api::qemu_plugin_tb_get_insn(tb, i);

                let insn_addr =
                    (qemu_api::qemu_plugin_insn_vaddr(inst) as u64) & 0x1_ffff_ffff_ffff;
                let offset = i as u64;
                let combined = insn_addr | (offset << 49);

                qemu_api::qemu_plugin_register_vcpu_mem_cb(
                    inst,
                    Some(vcpu_mem_access),
                    qemu_api::qemu_plugin_cb_flags_QEMU_PLUGIN_CB_NO_REGS,
                    qemu_api::qemu_plugin_mem_rw_QEMU_PLUGIN_MEM_RW,
                    combined as *mut ffi::c_void,
                );
            }
        }
    }

    fn serialize(name: &str) {
        unsafe {
            (*PLUGIN).serialize(name, 0);
            // if parameter::MEASURE_HALF_OF_CORES {
            // (*DUMMY_PLUGIN).serialize(name, 1);
            // }
        }
    }

    fn deserialize(name: &str) {
        unsafe {
            (*PLUGIN).deserialize(name, 0);
            // if parameter::MEASURE_HALF_OF_CORES {
            // (*DUMMY_PLUGIN).deserialize(name, 1);
            // }
        }
    }
}
