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

mod bimodal;
pub mod bbl_btb;
pub mod btb;
mod gshare;
mod ras;
pub mod tage;

use crate::debug::statistics::{EventType, Statistics};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;

use crate::parameter::{self, BP_RAS_COUNT};

use super::{BranchResolutionResult, BranchType};

#[derive(PartialEq)]
pub enum BranchPredictorResult {
    Match,
    Mispredict,
    NotActive,
}

/// Export-only state used by QPoints/gem5 checkpoint conversion.
///
/// This structure is recorded from the dynamic branch stream, but it is NOT
/// consulted by WormCache/Flexus prediction. Live prediction remains driven by
/// `pc_btb + tage + ras`.
#[derive(Serialize, Deserialize, Default)]
pub struct RestoreExportState {
    #[serde(default)]
    pub bbl_btb: bbl_btb::BblBTB<{ parameter::BTB_SET }, { parameter::BTB_ASSO }>,
}

impl RestoreExportState {
    pub fn record_basic_block(
        &mut self,
        pc: u64,
        result: BranchResolutionResult,
        target: u64,
        bbl_bytes: u64,
    ) {
        self.bbl_btb
            .record_basic_block(pc, result, target, bbl_bytes);
    }
}

#[repr(align(64))]
#[derive(Serialize, Deserialize)]
pub struct PerCoreFetchUnit {
    #[serde(default, alias = "btb")]
    pc_btb: btb::BTB<{ parameter::BTB_SET }, { parameter::BTB_ASSO }>,
    ras: ras::ReturnAddressStacle<BP_RAS_COUNT>,
    tage: tage::TAGEPredictor,
    #[serde(default)]
    restore_export: RestoreExportState,
    #[serde(default, alias = "bbl_btb", skip_serializing)]
    legacy_bbl_btb: Option<bbl_btb::BblBTB<{ parameter::BTB_SET }, { parameter::BTB_ASSO }>>,
    #[serde(skip, default)]
    collect_gem5_bbl_btb: bool,
}

impl PerCoreFetchUnit {
    pub fn new(collect_gem5_bbl_btb: bool) -> PerCoreFetchUnit {
        PerCoreFetchUnit {
            pc_btb: btb::BTB::new(),
            ras: ras::ReturnAddressStacle::new(),
            tage: tage::TAGEPredictor::new(),
            restore_export: RestoreExportState::default(),
            legacy_bbl_btb: None,
            collect_gem5_bbl_btb,
        }
    }

    pub fn set_collect_gem5_bbl_btb(&mut self, enabled: bool) {
        self.collect_gem5_bbl_btb = enabled;
    }

    pub fn train(
        &mut self,
        pc: u64,
        result: BranchResolutionResult,
        target: u64,
        bbl_bytes: u64,
        core_id: usize,
    ) {
        let is_os = pc >> 63 == 1;
        let pc_btb_result = self.pc_btb.train(pc, result, target, bbl_bytes);
        if self.collect_gem5_bbl_btb {
            self.restore_export
                .record_basic_block(pc, result, target, bbl_bytes);
        }
        let btb_miss = pc_btb_result.0 == BranchPredictorResult::Mispredict;

        let tage_miss = if pc_btb_result.1 == BranchType::Conditional {
            self.tage.train(pc, result, target) == BranchPredictorResult::Mispredict
        } else if result.branch_type != BranchType::NonBranch {
            self.tage.update_history(pc, result.is_taken); // This has to be done for non-conditional branches.
            false // No way to train the TAGE predictor for non-conditional branches.
        } else {
            false
        };

        let ras_miss = self.ras.train(pc, result, target) == BranchPredictorResult::Mispredict;

        if btb_miss {
            Statistics::global_record(core_id as u32, EventType::BTBMiss, is_os);
        }

        if ras_miss {
            Statistics::global_record(core_id as u32, EventType::RASMiss, is_os);
        }

        if tage_miss {
            Statistics::global_record(core_id as u32, EventType::TageMiss, is_os);
        }

        Statistics::global_record(core_id as u32, EventType::BranchCount, is_os);

        // Determine the branch prediction result.
        match result.branch_type {
            BranchType::NonBranch => unreachable!(),
            BranchType::Conditional => {
                if tage_miss || btb_miss {
                    Statistics::global_record(core_id as u32, EventType::BPMiss, is_os);
                }
            }
            BranchType::Return => {
                if ras_miss && btb_miss {
                    Statistics::global_record(core_id as u32, EventType::BPMiss, is_os);
                }
            }
            _ => {
                if btb_miss {
                    Statistics::global_record(core_id as u32, EventType::BPMiss, is_os);
                }
            }
        }
    }

    pub fn set_tage_decision_trace_limit(&mut self, limit: Option<usize>) {
        self.tage.set_decision_trace_limit(limit);
    }
}

impl Default for PerCoreFetchUnit {
    fn default() -> Self {
        Self::new(false)
    }
}

#[serde_as]
#[derive(Serialize, Deserialize)]
pub struct FetchUnit<const CORE_COUNT: usize> {
    #[serde_as(as = "[_; CORE_COUNT]")]
    pub private_units: [PerCoreFetchUnit; CORE_COUNT],
}

impl<const CORE_COUNT: usize> FetchUnit<CORE_COUNT> {
    pub fn new(collect_gem5_bbl_btb: bool) -> Self {
        FetchUnit {
            private_units: std::array::from_fn(|_| PerCoreFetchUnit::new(collect_gem5_bbl_btb)),
        }
    }

    pub fn set_collect_gem5_bbl_btb(&mut self, enabled: bool) {
        for unit in self.private_units.iter_mut() {
            unit.set_collect_gem5_bbl_btb(enabled);
        }
    }

    pub fn train(
        &mut self,
        core_id: usize,
        pc: u64,
        result: BranchResolutionResult,
        target: u64,
        bbl_bytes: u64,
    ) {
        self.private_units[core_id].train(pc, result, target, bbl_bytes, core_id);
    }

    pub fn set_tage_decision_trace_limit(&mut self, limit: Option<usize>) {
        for unit in self.private_units.iter_mut() {
            unit.set_tage_decision_trace_limit(limit);
        }
    }

    pub fn dump_training_trace(&self, folder_name: &str) {
        for i in 0..CORE_COUNT {
            let file_name = format!("{}/{}-bpred-training-history.json", folder_name, i);
            let file = std::fs::File::create(file_name).unwrap();
            serde_json::to_writer(file, &self.private_units[i].tage.training_trace).unwrap();
        }
    }

    pub fn dump_tage_decision_trace(&self, folder_name: &str) {
        for i in 0..CORE_COUNT {
            let file_name = format!("{}/tage_decision_trace_core_{}.json.zst", folder_name, i);
            let file = std::fs::File::create(file_name).unwrap();
            let mut file = zstd::Encoder::new(file, 3).unwrap();
            serde_json::to_writer(&mut file, &self.private_units[i].tage.decision_trace).unwrap();
            file.finish().unwrap();
        }
    }
}

impl<const CORE_COUNT: usize> Default for FetchUnit<CORE_COUNT> {
    fn default() -> Self {
        Self::new(false)
    }
}
