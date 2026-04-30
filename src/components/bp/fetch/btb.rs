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

use crate::components::bp::{BranchResolutionResult, BranchType};

use serde::{Deserialize, Serialize};
use serde_with::serde_as;

use super::BranchPredictorResult;

#[derive(Deserialize, Serialize, Clone)]
pub struct BTBEntry {
    pub tag: u64,
    pub target: u64,
    pub ts: u64, // zero means invalid.
    pub branch_type: BranchType,
    #[serde(default)]
    pub bbl_bytes: u64,
}

#[serde_as]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct BTB<const SET: usize, const ASSO: usize> {
    #[serde_as(as = "Vec<[_; ASSO]>")]
    array: Vec<[BTBEntry; ASSO]>,
    local_ts: u64,
}

impl<const SET: usize, const ASSO: usize> BTB<SET, ASSO> {
    pub fn new() -> Self {
        BTB {
            array: Vec::from_iter((0..SET).map(|_| {
                std::array::from_fn(|_| BTBEntry {
                    tag: 0,
                    target: 0,
                    ts: 0,
                    branch_type: BranchType::NonBranch,
                    bbl_bytes: 0,
                })
            })),
            local_ts: 0,
        }
    }

    // return true if the target is predicted by the BTB.
    pub fn train(
        &mut self,
        pc: u64,
        result: BranchResolutionResult,
        target: u64,
        bbl_bytes: u64,
    ) -> (BranchPredictorResult, BranchType) {
        self.local_ts += 1;

        // This is the word-aligned PC, so the index is shifted by 2 to avoid wasting space.
        let index = ((pc >> 2) % SET as u64) as usize;

        // We need to check if the entry is already in the BTB. If yes, we update the timestamp and return.
        // This should be the common case.
        for entry in self.array[index].iter_mut() {
            if entry.tag == pc {
                entry.ts = self.local_ts;
                entry.bbl_bytes = bbl_bytes;
                // BTB is not trained or accessed when the branch is predicted to be not taken.
                if !result.is_taken {
                    // This is useful to guide the TAGE training.
                    return (BranchPredictorResult::NotActive, entry.branch_type);
                }

                let miss = entry.target != target;

                let prediction = if miss {
                    (BranchPredictorResult::Mispredict, entry.branch_type)
                } else {
                    (BranchPredictorResult::Match, entry.branch_type)
                };

                entry.target = target; // also update the target and the branch type.
                entry.branch_type = result.branch_type;

                return prediction;
            }
        }

        // Only insert the entry if the branch is taken.
        if result.is_taken {
            // Find the entry with the minimum timestamp. Ts is zero means it is not valid.
            let mut min_index = 0;
            let mut min_ts = u64::MAX;

            for (i, entry) in self.array[index].iter().enumerate() {
                if entry.ts < min_ts {
                    min_ts = entry.ts;
                    min_index = i;
                }
            }

            // always replace the entry with the minimum timestamp
            self.array[index][min_index].tag = pc;
            self.array[index][min_index].target = target;
            self.array[index][min_index].ts = self.local_ts;
            self.array[index][min_index].branch_type = result.branch_type;
            self.array[index][min_index].bbl_bytes = bbl_bytes;

            return (BranchPredictorResult::Mispredict, result.branch_type);
        }

        // For a non-taken branch, it is not a misprediction. It is just that no prediction is made.
        (BranchPredictorResult::NotActive, BranchType::NonBranch)
    }
}

impl<const SET: usize, const ASSO: usize> Default for BTB<SET, ASSO> {
    fn default() -> Self {
        Self::new()
    }
}
