// Input trace for the PC BTB and the BBL BTB, for testing only.
//
// With the plugin option `btb_trace=1`, each BTB writes every branch it is
// trained with to its own per-core file, in the same line format:
//
//   btb_trace_core_<N>.pc_btb.log   written by BTB::train
//   btb_trace_core_<N>.bbl_btb.log  written by BblBTB::record_basic_block
//
// For one core the two files should be identical. `btb_trace_limit=N` stops
// each file after N lines.
//
// The files are kept here, not in the BTB structs, so the structs, their
// serialized state and snapshot loading are unchanged. A BTB does not know its
// core, so branch_resolved_cb records the core in a thread-local before
// training; training runs on the same thread.

use std::cell::Cell;
use std::fs::File;
use std::io::{LineWriter, Write};
use std::sync::{Mutex, OnceLock};

use rustc_hash::FxHashMap;

use crate::components::bp::BranchResolutionResult;

pub enum TracedBtb {
    PcBtb,
    BblBtb,
}

struct TraceFile {
    writer: LineWriter<File>,
    lines_written: u64,
}

struct CoreTraceFiles {
    pc_btb: Mutex<TraceFile>,
    bbl_btb: Mutex<TraceFile>,
}

// Set only when `btb_trace` is on.
static TRACE_FILES: OnceLock<Vec<CoreTraceFiles>> = OnceLock::new();
static LINE_LIMIT: OnceLock<Option<u64>> = OnceLock::new();

thread_local! {
    // The core whose branch is being trained on this thread.
    static CURRENT_CORE: Cell<Option<usize>> = const { Cell::new(None) };
}

fn open_trace_file(path: String) -> Mutex<TraceFile> {
    let file = File::create(&path)
        .unwrap_or_else(|err| panic!("Failed to create BTB trace file {}: {}", path, err));
    Mutex::new(TraceFile {
        writer: LineWriter::new(file),
        lines_written: 0,
    })
}

pub fn init(options: &FxHashMap<String, String>, core_count: usize) {
    let enabled = matches!(
        options.get("btb_trace").map(|v| v.as_str()),
        Some("1") | Some("true") | Some("yes") | Some("on")
    );
    if !enabled {
        return;
    }

    let line_limit = options.get("btb_trace_limit").map(|value| {
        value
            .parse::<u64>()
            .unwrap_or_else(|_| panic!("Invalid btb_trace_limit value: {}", value))
    });
    LINE_LIMIT
        .set(line_limit)
        .expect("BTB trace limit initialized twice.");

    let files = (0..core_count)
        .map(|core_id| CoreTraceFiles {
            pc_btb: open_trace_file(format!("btb_trace_core_{}.pc_btb.log", core_id)),
            bbl_btb: open_trace_file(format!("btb_trace_core_{}.bbl_btb.log", core_id)),
        })
        .collect();
    TRACE_FILES
        .set(files)
        .unwrap_or_else(|_| panic!("BTB trace files initialized twice."));
}

pub fn set_current_core(core_id: usize) {
    if TRACE_FILES.get().is_some() {
        CURRENT_CORE.with(|core| core.set(Some(core_id)));
    }
}

pub fn log_input(btb: TracedBtb, pc: u64, result: BranchResolutionResult, target: u64) {
    let Some(files) = TRACE_FILES.get() else {
        return;
    };
    let Some(core_files) = CURRENT_CORE.with(|core| core.get()).and_then(|id| files.get(id))
    else {
        return;
    };

    let file = match btb {
        TracedBtb::PcBtb => &core_files.pc_btb,
        TracedBtb::BblBtb => &core_files.bbl_btb,
    };
    let Ok(mut file) = file.lock() else {
        return;
    };

    if let Some(limit) = *LINE_LIMIT.get().unwrap_or(&None) {
        if file.lines_written >= limit {
            return;
        }
    }

    let _ = writeln!(
        file.writer,
        "pc={:#x} type={:?} taken={} target={:#x}",
        pc, result.branch_type, result.is_taken as u8, target
    );
    file.lines_written += 1;
}
