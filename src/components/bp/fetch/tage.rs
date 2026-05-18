#![allow(dead_code)]
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

// This file contains the basic TAGE branch predictor.
// It is basically an one-to-one translation of the C++ implementation in QFlex.

use super::BranchPredictorResult;
use crate::components::bp::{BranchResolutionResult, BranchType};

use serde::{Deserialize, Serialize};
use serde_with::serde_as;

// bits per counter in the global history tables
pub const CBITS: usize = 3;

// the default predictor
// by default a 63.5  Kbits predictor, featuring 7 tagged components and a base bimodal component:
// NHIST = 7, LOGB =13, LOGG=9, CBITS=3
// 10 Kbits for the bimodal table.
// 8.5 Kbits for T0
// 8 Kbits  for T1 and T2
// 7.5 Kbits for T3 and T4
// 7 Kbits for T5 and T6

pub const LOGB: usize = 13;
pub const NHIST: usize = 7;
// base 2 logarithm of number of entries  on each tagged component
pub const LOGG: usize = LOGB - 4;

// Total width of an entry in the tagged table with the longest history length
pub const TBITS: usize = 12;

// AS: we use Geometric history length
// AS: maximum global history length used and minimum history length
// The table HISTORIES can be calculated with the following python code:
// ```
// import math
// MAXHIST = 131 - 1
// MINHIST = 5
// NHIST = 7
// HISTORIES = [int(math.round(MINHIST * math.pow(MAXHIST / MINHIST, i / (NHIST - 1)))) for i in range(NHIST)]
// HISTORIES = reversed(HISTORIES)
// print(HISTORIES)
// ```
// This logic should be able to purely implemented in Rust after constant floating point arithmetic is stabilized.
pub const MAXHIST: usize = 131;
pub const MINHIST: usize = 5;
pub const HISTORIES: [usize; NHIST] = [130, 76, 44, 25, 15, 9, 5];

type Address = u64;

type History = [bool; MAXHIST];

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FoldedHistory {
    comp: u32,
    c_length: u32,
    o_length: u32,
    out_point: u32,
}

impl FoldedHistory {
    fn new() -> FoldedHistory {
        FoldedHistory {
            comp: 0,
            c_length: 0,
            o_length: 0,
            out_point: 0,
        }
    }

    fn init(&mut self, original_length: u32, compressed_length: u32) {
        self.comp = 0;
        self.o_length = original_length;
        self.c_length = compressed_length;
        self.out_point = self.o_length % self.c_length;
        assert!(self.o_length < MAXHIST as u32); // MAXHIST needs to be defined somewhere
    }

    // I am very curious why only h[0] and h[o_length] are used.
    fn update(&mut self, h: &History) {
        assert!((self.comp >> self.c_length) == 0);
        self.comp = (self.comp << 1) | (h[0] as u32);
        self.comp ^= (h[self.o_length as usize] as u32) << self.out_point;
        self.comp ^= self.comp >> self.c_length;
        self.comp &= (1 << self.c_length) - 1;
    }
}

// bimodal table entry
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TAGEBiModalEntry {
    hyst: i8,
    pred: i8,
}

impl TAGEBiModalEntry {
    fn new() -> TAGEBiModalEntry {
        TAGEBiModalEntry { hyst: 1, pred: 0 }
    }
}

// global table entry
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TAGEGlobalTableEntry {
    ctr: i8,
    tag: u16,
    ubit: i8,
}

impl TAGEGlobalTableEntry {
    fn new() -> TAGEGlobalTableEntry {
        TAGEGlobalTableEntry {
            ctr: 0,
            tag: 0,
            ubit: 0,
        }
    }
}

enum PredictionResult {
    StronglyTaken,
    Taken,
    NotTaken,
    StronglyNotTaken,
}

struct TAGEPredictionResultWithBank {
    pub result: bool,
    pub bank: usize,
    pub alternate_prediction: bool,
    pub alternate_bank: usize,
    pub gi: [usize; NHIST],
    pub bi: usize,
}

#[serde_as]
#[derive(Debug, Serialize, Deserialize)]
pub struct TAGETrainingTrace {
    pc: Address,
    target: Address,
    direction: bool,

    prediction_result: bool,
    bank: usize,
    alternate_prediction: bool,
    alternate_bank: usize,

    gi: [usize; NHIST],
    bi: usize,

    phist: i32,
    #[serde_as(as = "[_; MAXHIST]")]
    ghist: History,
    #[serde_as(as = "[_; NHIST]")]
    ch_i: [u32; NHIST],
    #[serde_as(as = "[[_; NHIST]; 2]")]
    ch_t: [[u32; NHIST]; 2],
}

/// Compact per-conditional decision log for cross-checking WormCache and gem5
/// TAGE behavior.
///
/// WormCache's current TAGE implementation does not expose gem5's
/// `useAltPredForNewlyAllocated` policy, so it only emits:
/// 0 = BIMODAL_ONLY
/// 1 = TAGE_LONGEST_MATCH
///
/// Codes 2/3 remain reserved for future parity if WormCache grows an explicit
/// alternate-provider decision path.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TAGEDecisionTrace {
    pc: Address,
    direction: bool,
    prediction_result: bool,
    provider: u8,
    bank: i32,
    bank_index: i32,
    bank_ctr: i8,
    bank_ubit: i8,
    alternate_prediction: bool,
    alternate_bank: i32,
    alternate_bank_index: i32,
    alternate_bank_ctr: i8,
    bi: usize,
    bimodal_pred: bool,
    bimodal_hyst: i8,
    phist: i32,
}

#[serde_as]
#[derive(Debug, Serialize, Deserialize)]
pub struct TAGEPredictor {
    // pwin: i32,

    // 4 bits to determine whether newly allocated entries should be considered as
    // valid or not for delivering  the prediction
    pub tick: i32,
    pub phist: i32,

    // use a path history as for the OGEHL predictor
    #[serde_as(as = "[_; MAXHIST]")]
    pub ghist: History,
    #[serde_as(as = "[_; NHIST]")]
    pub ch_i: [FoldedHistory; NHIST],
    #[serde_as(as = "[[_; NHIST]; 2]")]
    pub ch_t: [[FoldedHistory; NHIST]; 2],
    #[serde_as(as = "Box<[_; 1 << LOGB]>")]
    pub btable: Box<[TAGEBiModalEntry; 1 << LOGB]>,
    #[serde_as(as = "[Box<[_; 1 << LOGG]>; NHIST]")]
    pub gtable: [Box<[TAGEGlobalTableEntry; 1 << LOGG]>; NHIST],

    // the seed for pseudo-random number generator
    pub seed: i32,

    #[serde(skip)]
    pub training_trace: Vec<TAGETrainingTrace>,
    #[serde(skip)]
    pub decision_trace: Vec<TAGEDecisionTrace>,
    #[serde(skip)]
    pub decision_trace_enabled: bool,
    #[serde(skip)]
    pub decision_trace_limit: Option<usize>,
    // Debugging training trace.
}

impl TAGEPredictor {
    pub fn new() -> TAGEPredictor {
        // interpolate values between [MINHIST, MAXHIST-1] using geometric series
        let mut m = [0; NHIST];
        m[0] = MAXHIST - 1;
        m[NHIST - 1] = MINHIST;
        for i in 1..(NHIST - 1) {
            let base = (MAXHIST - 1) as f64 / MINHIST as f64;
            let exp = i as f64 / (NHIST - 1) as f64;
            m[NHIST - 1 - i] = (MINHIST as f64 * f64::powf(base, exp)).round() as usize;
        }

        // m and HISTORIES should be the same.
        assert_eq!(m, HISTORIES);

        TAGEPredictor {
            seed: 0,
            tick: 0,

            phist: 0, // the path history. Only 16 bits are used.
            // phist_runahead: 0,
            // phist_retired: 0,
            ghist: [false; MAXHIST],
            // ghist_runahead: [false; MAXHIST],
            // ghist_retired: [false; MAXHIST],
            ch_i: std::array::from_fn(|idx| {
                let mut fh = FoldedHistory::new();
                fh.init(m[idx] as u32, LOGG as u32);
                fh
            }),

            // ch_i_runahead: std::array::from_fn(|idx| {
            //     let mut fh = FoldedHistory::new();
            //     fh.init(m[idx] as u32, LOGG as u32);
            //     fh
            // }),
            ch_t: [
                std::array::from_fn(|idx| {
                    let mut fh = FoldedHistory::new();
                    fh.init(m[idx] as u32, (TBITS - ((idx + (NHIST & 1)) / 2)) as u32);
                    fh
                }),
                std::array::from_fn(|idx| {
                    let mut fh = FoldedHistory::new();
                    fh.init(
                        m[idx] as u32,
                        (TBITS - ((idx + (NHIST & 1)) / 2) - 1) as u32,
                    );
                    fh
                }),
            ],

            // ch_t_runahead: [
            //     std::array::from_fn(|idx| {
            //         let mut fh = FoldedHistory::new();
            //         fh.init(m[idx] as u32, (TBITS - ((idx + (NHIST & 1)) / 2)) as u32);
            //         fh
            //     }),
            //     std::array::from_fn(|idx| {
            //         let mut fh = FoldedHistory::new();
            //         fh.init(
            //             m[idx] as u32,
            //             (TBITS - ((idx + (NHIST & 1)) / 2) - 1) as u32,
            //         );
            //         fh
            //     }),
            // ],
            btable: Box::new(std::array::from_fn(|_| TAGEBiModalEntry::new())),
            gtable: std::array::from_fn(|_| {
                Box::new(std::array::from_fn(|_| TAGEGlobalTableEntry::new()))
            }),

            training_trace: vec![],
            decision_trace: vec![],
            decision_trace_enabled: false,
            decision_trace_limit: None,
        }
    }

    pub fn set_decision_trace_limit(&mut self, limit: Option<usize>) {
        self.decision_trace_enabled = true;
        self.decision_trace_limit = limit;
    }

    fn record_decision_trace(
        &mut self,
        pc: Address,
        taken: bool,
        prediction_result: &TAGEPredictionResultWithBank,
    ) {
        if !self.decision_trace_enabled {
            return;
        }

        if matches!(
            self.decision_trace_limit,
            Some(limit) if self.decision_trace.len() >= limit
        ) {
            return;
        }

        let bank = if prediction_result.bank < NHIST {
            prediction_result.bank as i32
        } else {
            -1
        };
        let bank_index = if prediction_result.bank < NHIST {
            prediction_result.gi[prediction_result.bank] as i32
        } else {
            -1
        };
        let bank_ctr = if prediction_result.bank < NHIST {
            self.gtable[prediction_result.bank][prediction_result.gi[prediction_result.bank]].ctr
        } else {
            0
        };
        let bank_ubit = if prediction_result.bank < NHIST {
            self.gtable[prediction_result.bank][prediction_result.gi[prediction_result.bank]].ubit
        } else {
            0
        };

        let alternate_bank = if prediction_result.alternate_bank < NHIST {
            prediction_result.alternate_bank as i32
        } else {
            -1
        };
        let alternate_bank_index = if prediction_result.alternate_bank < NHIST {
            prediction_result.gi[prediction_result.alternate_bank] as i32
        } else {
            -1
        };
        let alternate_bank_ctr = if prediction_result.alternate_bank < NHIST {
            self.gtable[prediction_result.alternate_bank]
                [prediction_result.gi[prediction_result.alternate_bank]]
                .ctr
        } else {
            0
        };

        let provider = if prediction_result.bank < NHIST { 1 } else { 0 };

        self.decision_trace.push(TAGEDecisionTrace {
            pc,
            direction: taken,
            prediction_result: prediction_result.result,
            provider,
            bank,
            bank_index,
            bank_ctr,
            bank_ubit,
            alternate_prediction: prediction_result.alternate_prediction,
            alternate_bank,
            alternate_bank_index,
            alternate_bank_ctr,
            bi: prediction_result.bi,
            bimodal_pred: self.btable[prediction_result.bi].pred > 0,
            bimodal_hyst: self.btable[prediction_result.bi].hyst,
            phist: self.phist,
        });
    }

    fn bindex(&self, shifted_pc: Address) -> usize {
        let b_mask = (1 << LOGB) - 1;
        (shifted_pc & b_mask) as usize
    }

    // I am really confused by this function.
    #[inline(always)]
    fn gindex(&self, shifted_pc: Address, bank: usize) -> usize {
        let path_history_mixer_hash_function = |path_history: u32, size: usize, bank: usize| {
            let a = (path_history as usize) & ((1 << size) - 1);
            let a1 = a & ((1 << LOGG) - 1);
            let a2 = a >> LOGG;
            let a2 = ((a2 << bank) & ((1 << LOGG) - 1)) + (a2 >> (LOGG - bank));
            let a = a1 ^ a2;

            ((a << bank) & ((1 << LOGG) - 1)) + (a >> (LOGG - bank))
        };

        let index_without_path =
            shifted_pc ^ (shifted_pc >> (LOGG - NHIST + bank + 1)) ^ self.ch_i[bank].comp as u64;

        let index = if HISTORIES[bank] >= 16 {
            index_without_path
                ^ path_history_mixer_hash_function(self.phist as u32, 16, bank) as u64
        } else {
            index_without_path
                ^ path_history_mixer_hash_function(self.phist as u32, HISTORIES[bank], bank) as u64
        };

        let g_mask = (1 << LOGG) - 1;

        (index & g_mask) as usize
    }

    fn gtag(&self, shifted_pc: Address, bank: usize) -> u16 {
        let tag =
            shifted_pc ^ self.ch_t[0][bank].comp as u64 ^ (self.ch_t[1][bank].comp << 1) as u64;
        let mask = (1 << (TBITS - (bank + (NHIST & 1)) / 2)) - 1;
        (tag & mask) as u16
    }

    fn ctrupdate(cnt: i8, taken: bool, nbits: usize) -> i8 {
        let max: i8 = (1 << (nbits - 1)) - 1;
        let min: i8 = -max - 1;
        if taken {
            if cnt < max { cnt + 1 } else { cnt }
        } else if cnt > min {
            cnt - 1
        } else {
            cnt
        }
    }

    #[inline(always)]
    fn is_cond_taken(&self, pc: Address) -> TAGEPredictionResultWithBank {
        let pc = pc >> 2; // pc is always aligned to 4 bytes
        let bi: usize = self.bindex(pc);
        // let gi: Vec<_> = (0..NHIST).map(|idx| self.gindex(pc, idx)).collect();
        let gi: [usize; NHIST] = std::array::from_fn(|idx| self.gindex(pc, idx));

        let mut which_bank = NHIST;
        let mut alter_which_bank: usize = NHIST;

        for idx in 0..NHIST {
            if self.gtable[idx][gi[idx]].tag == self.gtag(pc, idx) {
                // it is a hit!
                which_bank = idx;
                break;
            }
        }

        for idx in (which_bank + 1)..NHIST {
            if self.gtable[idx][gi[idx]].tag == self.gtag(pc, idx) {
                // it is a hit!
                alter_which_bank = idx;
                break;
            }
        }

        if which_bank < NHIST {
            // get the alter_prediction result
            let alternate_prediction = if alter_which_bank < NHIST {
                self.gtable[alter_which_bank][gi[alter_which_bank]].ctr >= 0
            } else {
                self.btable[bi].pred > 0
            };
            let cnt = self.gtable[which_bank][gi[which_bank]].ctr;
            TAGEPredictionResultWithBank {
                result: cnt >= 0,
                bank: which_bank,
                alternate_prediction,
                alternate_bank: alter_which_bank,
                gi,
                bi,
            }
        } else {
            let alternate_prediction = self.btable[bi].pred > 0;
            TAGEPredictionResultWithBank {
                result: alternate_prediction,
                bank: which_bank,
                alternate_prediction,
                alternate_bank: alter_which_bank,
                gi,
                bi,
            }
        }
    }

    fn shift_global_history(&mut self, taken: bool) {
        self.ghist.rotate_right(1);
        self.ghist[0] = taken;
    }

    pub fn update_history(&mut self, pc: Address, taken: bool) {
        // update ghist.
        self.shift_global_history(taken);
        // update phist.
        self.phist = (self.phist << 1) | ((pc >> 2) & 1) as i32;
        self.phist &= (1 << 16) - 1;

        // update ch_i
        for idx in 0..NHIST {
            self.ch_i[idx].update(&self.ghist);
            self.ch_t[0][idx].update(&self.ghist);
            self.ch_t[1][idx].update(&self.ghist);
        }
    }

    fn get_random(&mut self) -> i32 {
        self.seed = ((1 << (2 * NHIST)) + 1) * self.seed + 0xf3f531;
        self.seed &= (1 << (2 * (NHIST))) - 1;
        self.seed
    }

    pub fn train(
        &mut self,
        pc: u64,
        result: BranchResolutionResult,
        _target: u64,
    ) -> BranchPredictorResult {
        // we only update the predictor when the branch is predicted as conditional.
        let taken = result.is_taken;
        let prediction_result = self.is_cond_taken(pc);
        let mut allocation = prediction_result.result != taken && prediction_result.bank > 0;

        if prediction_result.bank < NHIST {
            let ctr = self.gtable[prediction_result.bank]
                [prediction_result.gi[prediction_result.bank]]
                .ctr;
            let ubit = self.gtable[prediction_result.bank]
                [prediction_result.gi[prediction_result.bank]]
                .ubit;
            let local_taken = ctr >= 0;
            let pesudo_new_alloc = ((ctr * 2 + 1).abs() == 1) && (ubit == 0);

            if pesudo_new_alloc {
                if local_taken == taken {
                    allocation = false;
                }
            }
        }

        self.record_decision_trace(pc, taken, &prediction_result);

        if allocation {
            assert!(prediction_result.result != taken);
            let mut min: i8 = 3; // the the minimum useful counter value
            for idx in 0..(prediction_result.bank) {
                if self.gtable[idx][prediction_result.gi[idx]].ubit < min {
                    min = self.gtable[idx][prediction_result.gi[idx]].ubit;
                }
            }

            if min > 0 {
                // NO UNUSEFUL ENTRY TO ALLOCATE: age all possible targets, but do not allocate
                for idx in 0..(prediction_result.bank) {
                    self.gtable[idx][prediction_result.gi[idx]].ubit -= 1;
                }
            } else {
                // YES: allocate one entry, but apply some randomness
                // bank I is twice more probable than bank I-1
                let n_rand = self.get_random();
                let mut y = n_rand & ((1 << (prediction_result.bank - 1)) - 1);
                let mut x = prediction_result.bank - 1;
                while (y & 1) != 0 {
                    x -= 1;
                    y >>= 1;
                }

                for idx in 0..(x + 1) {
                    let t = x - idx;
                    if self.gtable[t][prediction_result.gi[t]].ubit == min {
                        self.gtable[t][prediction_result.gi[t]].tag = self.gtag(pc >> 2, t);
                        self.gtable[t][prediction_result.gi[t]].ctr = if taken { 0 } else { -1 };
                        self.gtable[t][prediction_result.gi[t]].ubit = 0;
                        break;
                    }
                }
            }
        }

        // periodic reset of ubit: reset is not complete but bit by bit
        self.tick += 1;

        if (self.tick & ((1 << 18) - 1)) == 0 {
            let mut mask = (self.tick >> 18) & 1;
            if mask == 0 {
                mask = 2;
            }
            for idx in 0..NHIST {
                for idx2 in 0..(1 << LOGG) {
                    self.gtable[idx][idx2].ubit &= mask as i8;
                }
            }
        }

        // update the counter that provided the prediction, and only this counter

        if prediction_result.bank < NHIST {
            self.gtable[prediction_result.bank][prediction_result.gi[prediction_result.bank]].ctr =
                TAGEPredictor::ctrupdate(
                    self.gtable[prediction_result.bank]
                        [prediction_result.gi[prediction_result.bank]]
                        .ctr,
                    taken,
                    CBITS,
                );
        } else {
            // the prediction is from the btable.
            assert!(prediction_result.alternate_prediction == prediction_result.result);
            assert!((self.btable[prediction_result.bi].pred > 0) == prediction_result.result);

            if prediction_result.result == taken {
                if taken {
                    if self.btable[prediction_result.bi].pred != 0 {
                        self.btable[prediction_result.bi].hyst = 1;
                    }
                } else if self.btable[prediction_result.bi].pred == 0 {
                    self.btable[prediction_result.bi].hyst = 0;
                }
            } else {
                let mut inter = self.btable[prediction_result.bi].pred * 2
                    + self.btable[prediction_result.bi].hyst;
                if taken {
                    if inter < 3 {
                        inter += 1;
                    }
                } else if inter > 0 {
                    inter -= 1;
                }
                self.btable[prediction_result.bi].pred = inter >> 1;
                self.btable[prediction_result.bi].hyst = inter & 1;
            }
        }

        // update the ubit counter
        if prediction_result.result != prediction_result.alternate_prediction {
            assert!(prediction_result.bank < NHIST);
            if prediction_result.result == taken {
                if self.gtable[prediction_result.bank][prediction_result.gi[prediction_result.bank]]
                    .ubit
                    < 3
                {
                    self.gtable[prediction_result.bank]
                        [prediction_result.gi[prediction_result.bank]]
                        .ubit += 1;
                }
            } else {
                if self.gtable[prediction_result.bank][prediction_result.gi[prediction_result.bank]]
                    .ubit
                    > 0
                {
                    self.gtable[prediction_result.bank]
                        [prediction_result.gi[prediction_result.bank]]
                        .ubit -= 1;
                }
            }
        }

        // // record the training trace.
        // self.training_trace.push(TAGETrainingTrace {
        //     pc,
        //     target: _target,
        //     direction: taken,
        //     prediction_result: prediction_result.result,
        //     bank: prediction_result.bank,
        //     alternate_prediction: prediction_result.alternate_prediction,
        //     alternate_bank: prediction_result.alternate_bank,
        //     gi: prediction_result.gi,
        //     bi: prediction_result.bi,
        //     phist: self.phist,
        //     ghist: self.ghist,
        //     ch_i: std::array::from_fn(|idx| self.ch_i[idx].comp),
        //     ch_t: [
        //         std::array::from_fn(|idx| self.ch_t[0][idx].comp),
        //         std::array::from_fn(|idx| self.ch_t[1][idx].comp),
        //     ],
        // });

        // before returning, update the history.

        if result.branch_type == BranchType::Conditional {
            // Only update the history for conditional branches.
            self.update_history(pc, taken);
        }

        if prediction_result.result != taken {
            return BranchPredictorResult::Mispredict;
        } else {
            return BranchPredictorResult::Match;
        }
    }
}

impl Default for TAGEPredictor {
    fn default() -> Self {
        Self::new()
    }
}
