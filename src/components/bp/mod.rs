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

pub mod fetch;

use core::ffi;
use std::fs::File;
use std::io::{LineWriter, Write};
use std::sync::{Mutex, OnceLock};

use super::Plugin;
use crate::{parameter, qemu_api};

use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};

use zstd::{Decoder, Encoder};

// Use Arena to allocate the BranchMetaData.
// https://crates.io/crates/bumpalo

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub enum BranchType {
    NonBranch = 0,
    Conditional = 1,
    Unconditional = 2,
    DirectCall = 3,
    IndirectBranch = 4,
    IndirectCall = 5,
    Return = 6,
}

impl BranchType {
    pub fn is_call(&self) -> bool {
        matches!(self, BranchType::DirectCall | BranchType::IndirectCall)
    }

    pub fn is_return(&self) -> bool {
        matches!(self, BranchType::Return)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct BranchResolutionResult {
    pub branch_type: BranchType,
    pub is_taken: bool,
}

impl BranchResolutionResult {
    fn from_u32(value: u32) -> BranchResolutionResult {
        let is_taken = value & 1 == 1;
        let result_value = value >> 1;

        BranchResolutionResult {
            is_taken,
            branch_type: match result_value {
                0 => unreachable!(),
                1 => BranchType::Conditional,
                2 => {
                    assert!(is_taken);
                    BranchType::Unconditional
                }
                3 => {
                    assert!(is_taken);
                    BranchType::DirectCall
                }
                4 => {
                    assert!(is_taken);
                    BranchType::IndirectBranch
                }
                5 => {
                    assert!(is_taken);
                    BranchType::IndirectCall
                }
                6 => {
                    assert!(is_taken);
                    BranchType::Return
                }
                _ => unreachable!(),
            },
        }
    }
}

const ALLOCATED_CORE: usize = if parameter::MEASURE_HALF_OF_CORES {
    parameter::CORE_COUNT / 2
} else {
    parameter::CORE_COUNT
};

static mut FETCH_UNIT: *mut fetch::FetchUnit<{ ALLOCATED_CORE }> = std::ptr::null_mut();
static BRANCH_LOGS: OnceLock<Vec<Mutex<LineWriter<File>>>> = OnceLock::new();
static COLLECT_GEM5_BBL_BTB: OnceLock<bool> = OnceLock::new();

#[derive(Default, Debug, Clone, Copy)]
struct CoreBbState {
    current_bb_start: Option<u64>,
    prev_pc: Option<u64>,
    next_pc_starts_new_bb: bool,
}

static BB_STATES: OnceLock<Vec<Mutex<CoreBbState>>> = OnceLock::new();

unsafe extern "C" fn vcpu_insn_exec(vcpu_idx: u32, inst_virtual_addr: *mut ffi::c_void) {
    if parameter::MEASURE_HALF_OF_CORES && vcpu_idx >= parameter::CORE_COUNT as u32 / 2 {
        return;
    }

    let pc_vpn = unsafe { qemu_api::qemu_plugin_read_pc_vpn() };
    let pc = (pc_vpn << 12) | (inst_virtual_addr as u64 & 0xfff);

    if let Some(states) = BB_STATES.get() {
        if let Some(state) = states.get(vcpu_idx as usize) {
            if let Ok(mut state) = state.lock() {
                let starts_new_bb = state.current_bb_start.is_none()
                    || state.next_pc_starts_new_bb
                    || state.prev_pc.map_or(false, |prev_pc| prev_pc.saturating_add(4) != pc);

                if starts_new_bb {
                    state.current_bb_start = Some(pc);
                    state.next_pc_starts_new_bb = false;
                }

                state.prev_pc = Some(pc);
            }
        }
    }
}

unsafe extern "C" fn branch_resolved_cb(vcpu_index: u32, pc: u64, target: u64, flags: u32) {
    unsafe {
        if parameter::MEASURE_HALF_OF_CORES && vcpu_index >= parameter::CORE_COUNT as u32 / 2 {
            return;
        }

        if let Some(logs) = BRANCH_LOGS.get() {
            if let Some(log) = logs.get(vcpu_index as usize) {
                if let Ok(mut log) = log.lock() {
                    let _ = writeln!(log, "{}", pc);
                }
            }
        }

        let result = BranchResolutionResult::from_u32(flags);
        let bbl_bytes = BB_STATES
            .get()
            .and_then(|states| states.get(vcpu_index as usize))
            .and_then(|state| state.lock().ok().map(|state| state.current_bb_start))
            .flatten()
            .map(|bb_start| pc.saturating_sub(bb_start))
            .unwrap_or(0);

        if let Some(states) = BB_STATES.get() {
            if let Some(state) = states.get(vcpu_index as usize) {
                if let Ok(mut state) = state.lock() {
                    state.next_pc_starts_new_bb = true;
                }
            }
        }

        (*FETCH_UNIT).train(vcpu_index as usize, pc, result, target, bbl_bytes)
    }
}

pub struct BranchPredictorPlugin {}

fn option_enabled(options: &FxHashMap<String, String>, key: &str) -> bool {
    match options.get(key).map(|v| v.as_str()) {
        Some("1") | Some("true") | Some("yes") | Some("on") => true,
        _ => false,
    }
}

impl Plugin for BranchPredictorPlugin {
    fn init(_plugin_id: u64, options: &FxHashMap<String, String>) {
        println!("BranchPredictorPlugin initialized.");

        // get the mode name.
        let mode = String::new();
        let mode = options.get("mode").unwrap_or(&mode);

        assert_ne!(
            mode, "vtime",
            "Pure vtime is enabled. BP should be disabled."
        );

        assert!(unsafe {
            qemu_api::qemu_plugin_register_vcpu_branch_resolved_cb(Some(branch_resolved_cb))
        });

        let collect_gem5_bbl_btb = option_enabled(options, "collect_gem5_bbl_btb");
        COLLECT_GEM5_BBL_BTB
            .set(collect_gem5_bbl_btb)
            .expect("Failed to initialize gem5 BBL-BTB collection option.");
        unsafe {
            FETCH_UNIT = Box::into_raw(Box::new(fetch::FetchUnit::new(collect_gem5_bbl_btb)));
        }
        BB_STATES
            .set(
                (0..ALLOCATED_CORE)
                    .map(|_| Mutex::new(CoreBbState::default()))
                    .collect(),
            )
            .expect("Failed to initialize BTB basic-block tracking state.");

        if option_enabled(options, "branch_trace") {
            let mut logs = Vec::with_capacity(ALLOCATED_CORE);
            for core_id in 0..ALLOCATED_CORE {
                let file = File::create(format!("branch_trace_core_{}.log", core_id))
                    .expect("Failed to create branch trace log file.");
                logs.push(Mutex::new(LineWriter::new(file)));
            }
            BRANCH_LOGS
                .set(logs)
                .expect("Failed to initialize branch trace logs.");
        }
    }

    unsafe fn on_translation(tb: *mut crate::qemu_api::qemu_plugin_tb) {
        let instruction_count = unsafe { qemu_api::qemu_plugin_tb_n_insns(tb) };
        if instruction_count == 0 {
            return;
        }

        for i in 0..instruction_count {
            let insn = unsafe { qemu_api::qemu_plugin_tb_get_insn(tb, i) };
            let insn_addr = unsafe { qemu_api::qemu_plugin_insn_vaddr(insn) };
            unsafe {
                qemu_api::qemu_plugin_register_vcpu_insn_exec_cb(
                    insn,
                    Some(vcpu_insn_exec),
                    qemu_api::qemu_plugin_cb_flags_QEMU_PLUGIN_CB_NO_REGS,
                    insn_addr as *mut ffi::c_void,
                );
            }
        }
    }

    fn serialize(name: &str) {
        // open a file
        let mut file = std::fs::File::create(format!("{}/fetch.json.zstd", name)).unwrap();

        let mut file = Encoder::new(&mut file, 0).unwrap();

        // write the content
        let json = serde_json::to_string(unsafe { &(*FETCH_UNIT) }).unwrap();
        file.write_all(json.as_bytes()).unwrap();

        file.finish().unwrap();
    }

    fn deserialize(name: &str) {
        // open a file
        let file = std::fs::File::open(format!("{}/fetch.json.zstd", name));

        if file.is_err() {
            println!("Cannot load the fetch unit state. Error: {:?}", file.err());
            return;
        }

        let file = file.unwrap();

        let file = Decoder::new(file).unwrap();

        // read the content
        let reader = std::io::BufReader::new(file);

        // Deserialize the content
        let mut reader = serde_json::Deserializer::from_reader(reader);

        Deserialize::deserialize_in_place(&mut reader, unsafe { &mut (*FETCH_UNIT) }).unwrap();
        let collect_gem5_bbl_btb = *COLLECT_GEM5_BBL_BTB.get().unwrap_or(&false);
        unsafe {
            (*FETCH_UNIT).set_collect_gem5_bbl_btb(collect_gem5_bbl_btb);
        }
    }
}
