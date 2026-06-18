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

use std::{fs::File, io::Write};

use crate::{
    debug::statistics::{EventType, Statistics},
    parameter::{self, PluginList},
    qemu_api,
    util::get_monotonic_ts,
};
use spin::Mutex as SpinMutex;

use std::sync::OnceLock;

static SNAPSHOT_INFO: SpinMutex<Option<(String, u64)>> = SpinMutex::new(None);

use std::sync::atomic::{AtomicU64, Ordering};

static mut PERIODIC_SNAPSHOT_INIT_INDEX: u64 = 0;
static PERIODIC_SNAPSHOT_COUNT: AtomicU64 = AtomicU64::new(0);
static mut PERIODIC_SNAPSHOT_REQUIRED_COUNT: u64 = 0xffff_ffff_ffff_ffff;

static mut PERIODIC_SNAPSHOT_THRESHOLD: u64 = 0xffff_ffff_ffff_ffff;
static mut PERIODIC_SNAPSHOT_INTERVAL: u64 = 0xffff_ffff_ffff_ffff;
static mut PERIODIC_SNAPSHOT_CURRENT_CYCLES: u64 = 0;
static mut PERIODIC_SNAPSHOT_NO_QEMU_SNAPSHOT: bool = false;

static SNAPSHOT_PREFIX: OnceLock<String> = OnceLock::new();
static QEMU_SNAPSHOT_FORMAT: OnceLock<SpinMutex<String>> = OnceLock::new();

static SNAPSHOT_LATENCY: OnceLock<SpinMutex<Vec<u64>>> = OnceLock::new();

unsafe extern "C" fn event_loop_callback() {
    unsafe {
        let snapshot_info_guard = SNAPSHOT_INFO.try_lock();
        if snapshot_info_guard.is_none() {
            return;
        }

        let mut snapshot_info_guard = snapshot_info_guard.unwrap();

        if snapshot_info_guard.is_none() {
            return;
        }

        let snapshot_info = snapshot_info_guard.take().unwrap();

        println!(
            "Snapshot request: {}, At Cycle: {}",
            &snapshot_info.0, snapshot_info.1
        );

        let c_snapshot_name = std::ffi::CString::new(snapshot_info.0.clone()).unwrap();

        let snapshot_format = QEMU_SNAPSHOT_FORMAT.get().unwrap().lock().clone();

        let snapshot_format = if snapshot_format == "zstd" {
            qemu_api::qemu_plugin_snapshot_format_t_QEMU_PLUGIN_SNAPSHOT_FORMAT_EXTERNAL_ZSTD
        } else if snapshot_format == "incremental" {
            qemu_api::qemu_plugin_snapshot_format_t_QEMU_PLUGIN_SNAPSHOT_FORMAT_EXTERNAL_INCREMENTAL_DELTA
        } else if snapshot_format == "incremental_first_base" {
            qemu_api::qemu_plugin_snapshot_format_t_QEMU_PLUGIN_SNAPSHOT_FORMAT_EXTERNAL_INCREMENTAL_BASE
        } else {
            panic!("Unsupported snapshot format: {}", snapshot_format);
        };

        // get the current timestamp in miliseconds
        let current_time = std::time::SystemTime::now();
        qemu_api::qemu_plugin_savevm(c_snapshot_name.as_ptr(), snapshot_format);
        let elapsed_time = std::time::SystemTime::now()
            .duration_since(current_time)
            .unwrap()
            .as_millis();

        // record the snapshot latency
        let mut snapshot_latency = SNAPSHOT_LATENCY.get().unwrap().lock();
        snapshot_latency.push(elapsed_time as u64);
        drop(snapshot_latency);

        let snapshot_count = PERIODIC_SNAPSHOT_COUNT.fetch_add(1, Ordering::Relaxed) + 1;

        if snapshot_count >= PERIODIC_SNAPSHOT_REQUIRED_COUNT {
            println!("Generate {} snapshots. Quit.", snapshot_count);
            let mut miss_file = std::fs::File::create("statistics.final.csv").unwrap();
            miss_file
                .write_fmt(format_args!("{}\n", Statistics::get_header()))
                .unwrap();

            // update the local target time before writing the statistics
            for core_id in 0..parameter::CORE_COUNT {
                Statistics::global_set(
                    core_id as u32,
                    EventType::TargetLocalCycle,
                    false,
                    qemu_api::qemu_plugin_get_vcpu_vtime(core_id as u32),
                );
            }

            for stat in Statistics::global_get_line_for_all_cores(get_monotonic_ts()) {
                miss_file.write_all(stat.as_bytes()).unwrap();
                miss_file.write_all(b"\n").unwrap();
            }

            // dump the snapshot latency
            let mut miss_file = std::fs::File::create("snapshot_latency.csv").unwrap();
            miss_file
                .write_fmt(format_args!("{}\n", "Snapshot Latency (ms)"))
                .unwrap();
            let snapshot_latency = SNAPSHOT_LATENCY.get().unwrap().lock();
            for latency in snapshot_latency.iter() {
                miss_file.write_fmt(format_args!("{}\n", latency)).unwrap();
            }
            miss_file.flush().unwrap();
            drop(miss_file);

            // Single-node: exit(0)s inside (every snapshot already on disk). Multi-node: the final
            // snapshot above only ARMED the checkpoint; PDES writes it at the next quantum boundary
            // and then drives the coordinated exit, so this returns and we must NOT exit here (that
            // premature exit dropped the last snapshot — the 999-vs-1000 bug).
            qemu_api::qemu_plugin_pdes_fw_complete();
        }
    }
}

// add a global file to record the statistics for each quantum.
static STATISTICS_QUANTUM_FILE: OnceLock<SpinMutex<File>> = OnceLock::new();

// Remember, this function will be used as a quantum callback.
unsafe extern "C" fn quantum_checking_callback(diff: u64) -> bool {
    unsafe {
        PERIODIC_SNAPSHOT_CURRENT_CYCLES += diff;

        if PERIODIC_SNAPSHOT_CURRENT_CYCLES >= PERIODIC_SNAPSHOT_THRESHOLD {
            let snapshot_name = format!(
                "{}_{}",
                SNAPSHOT_PREFIX.get().unwrap(),
                PERIODIC_SNAPSHOT_COUNT.load(Ordering::Relaxed) + PERIODIC_SNAPSHOT_INIT_INDEX
            );
            let snapshot_info = (snapshot_name, PERIODIC_SNAPSHOT_CURRENT_CYCLES);

            if !PERIODIC_SNAPSHOT_NO_QEMU_SNAPSHOT {
                let snapshot_info_guard = SNAPSHOT_INFO.try_lock();
                if snapshot_info_guard.is_none() {
                    return false;
                }

                let mut snapshot_info_guard = snapshot_info_guard.unwrap();

                if snapshot_info_guard.is_none() {
                    *snapshot_info_guard = Some(snapshot_info);
                }
            } else {
                println!(
                    "WormCache-only snapshot request: {}, At Cycle: {}",
                    &snapshot_info.0, snapshot_info.1
                );

                // manually call serialize function of all plugins.
                std::fs::create_dir_all(&snapshot_info.0).unwrap();
                PluginList::serialize(&snapshot_info.0);
                let snapshot_count = PERIODIC_SNAPSHOT_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
                1;

                if snapshot_count >= PERIODIC_SNAPSHOT_REQUIRED_COUNT {
                    println!("Generate {} snapshots. Quit.", snapshot_count);
                    let mut miss_file = std::fs::File::create("statistics.final.csv").unwrap();
                    miss_file
                        .write_fmt(format_args!("{}\n", Statistics::get_header()))
                        .unwrap();

                    // update the local target time before writing the statistics
                    for core_id in 0..parameter::CORE_COUNT {
                        Statistics::global_set(
                            core_id as u32,
                            EventType::TargetLocalCycle,
                            false,
                            qemu_api::qemu_plugin_get_vcpu_vtime(core_id as u32),
                        );
                    }

                    for stat in Statistics::global_get_line_for_all_cores(get_monotonic_ts()) {
                        miss_file.write_all(stat.as_bytes()).unwrap();
                        miss_file.write_all(b"\n").unwrap();
                    }
                    std::process::exit(0);
                }
            }

            PERIODIC_SNAPSHOT_THRESHOLD += PERIODIC_SNAPSHOT_INTERVAL;

            return !PERIODIC_SNAPSHOT_NO_QEMU_SNAPSHOT;
        }

        false
    }
}

pub unsafe fn init(
    init_threshold: u64,
    interval: u64,
    required_count: u64,
    prefix: String,
    init_index: u64,
    no_qemu_snapshot: bool,
) {
    unsafe {
        PERIODIC_SNAPSHOT_THRESHOLD = init_threshold;
        PERIODIC_SNAPSHOT_REQUIRED_COUNT = required_count;
        PERIODIC_SNAPSHOT_INTERVAL = interval;
        SNAPSHOT_PREFIX.set(prefix).unwrap();
        PERIODIC_SNAPSHOT_INIT_INDEX = init_index;
        PERIODIC_SNAPSHOT_NO_QEMU_SNAPSHOT = no_qemu_snapshot;

        // make the default snapshot format to be incremental_first_base.
        QEMU_SNAPSHOT_FORMAT
            .set(SpinMutex::new("incremental_first_base".to_string()))
            .expect("Failed to set the snapshot format.");

        assert!(qemu_api::qemu_plugin_register_periodic_check_cb(Some(
            quantum_checking_callback
        )));

        if !PERIODIC_SNAPSHOT_NO_QEMU_SNAPSHOT {
            assert!(qemu_api::qemu_plugin_register_event_loop_poll_cb(Some(
                event_loop_callback
            )));
        }

        STATISTICS_QUANTUM_FILE
            .set(SpinMutex::new(
                File::create("statistics.quantum.csv").expect("Failed to create statistics file."),
            ))
            .expect("Failed to set the statistics file.");

        STATISTICS_QUANTUM_FILE
            .get()
            .unwrap()
            .lock()
            .write_fmt(format_args!("{}\n", Statistics::get_header()))
            .unwrap();

        SNAPSHOT_LATENCY
            .set(SpinMutex::new(Vec::new()))
            .expect("Failed to set the snapshot latency file.");
    }
}

fn update_snapshot_type(snapshot_type: &str) {
    let snapshot_format = QEMU_SNAPSHOT_FORMAT.get().unwrap();
    let mut snapshot_format_guard = snapshot_format.lock();
    *snapshot_format_guard = snapshot_type.to_string();
}

pub fn on_load_snapshot(snapshot_name: &str) {
    if QEMU_SNAPSHOT_FORMAT.get().is_none() {
        return;
    }

    // If the snapshot is an incremental base, which means {name}.basemem.zstd and {name}.state.zstd exist, we change the snapshot type to incremental.
    use std::fs;

    // check if the snapshot file exists
    let base_file = format!("{}.mem/base", snapshot_name);
    let state_file = format!("{}.state.zstd", snapshot_name);
    if fs::metadata(&base_file).is_ok() && fs::metadata(&state_file).is_ok() {
        update_snapshot_type("incremental");
        println!(
            "Detected incremental base snapshot: {}. Following snaphots are generaed with delta",
            snapshot_name
        );
    }
}
