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

pub mod arch;
pub mod parameter;

pub mod checkpoint;
pub mod chronic;
pub mod components;
pub mod debug;
mod qemu_api;
mod util;

pub mod timestamp;

// Plugin
use crate::chronic::chronic_behavior_init;
use crate::chronic::on_finish_loading_snapshot;
use crate::debug::statistics;
use crate::debug::statistics::Statistics;
#[allow(unused_imports)]
use components::bp::BranchPredictorPlugin;
#[allow(unused_imports)]
use components::cache_hierarchy::ParallelCacheHierarchyPlugin;
#[allow(unused_imports)]
use components::cache_hierarchy::SingleCacheHierarchyPlugin;
#[allow(unused_imports)]
use components::instruction_frequency::InstructionFrequencyPlugin;
#[allow(unused_imports)]
use components::pw_log::PageWalkLoggerPlugin;
#[allow(unused_imports)]
use components::touch_once::TouchOnePlugin;
#[allow(unused_imports)]
use components::trace::TracePlugin;
#[allow(unused_imports)]
use components::wfi::WaitForInterruptCounterPlugin;

use components::Plugin;
use parameter::PluginList;
use rustc_hash::FxHashMap;
use util::get_monotonic_ts;

use std::ffi;
use std::io::Write;

#[unsafe(link_section = ".rodata")]
#[unsafe(no_mangle)]
static PARAMETER_RS: &str = include_str!("./parameter.rs");

#[allow(non_upper_case_globals)]
#[unsafe(no_mangle)]
pub static qemu_plugin_version: u32 = qemu_api::QEMU_PLUGIN_VERSION;

#[unsafe(no_mangle)]
unsafe extern "C" fn vcpu_tb_trans(
    _: qemu_api::qemu_plugin_id_t,
    tb: *mut qemu_api::qemu_plugin_tb,
) {
    unsafe {
        PluginList::on_translation(tb);

        if parameter::ENABLE_STATISTICS {
            statistics::on_translation_instructions(tb);
        }
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn savevm_cb(name: *const ffi::c_char) {
    unsafe {
        let converted_name = ffi::CStr::from_ptr(name).to_str();

        if converted_name.is_err() {
            // print the raw char and return.
            println!("Failed to convert the name to string.");
            // print the raw char until we saw a null character.
            let mut i = 0;
            loop {
                let c = *name.offset(i);
                if c == 0 {
                    break;
                }
                print!("{}", c as u8 as char);
                i += 1;
            }

            panic!();
        }

        let name = converted_name.unwrap();

        let name = format!("{}.uarch", name);
        // create a folder for the name.
        std::fs::create_dir_all(&name).unwrap();
        let current_time = std::time::SystemTime::now();
        PluginList::serialize(&name);
        Statistics::save_to_csv(&format!("{}/statistics.csv", name), get_monotonic_ts());
        timestamp::serialize(&name);

        let elapsed_time = std::time::SystemTime::now()
            .duration_since(current_time)
            .unwrap()
            .as_millis();

        println!("Serialized the plugin data in {} ms.", elapsed_time);
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn loadvm_cb(name: *const ffi::c_char) {
    unsafe {
        let name = ffi::CStr::from_ptr(name).to_str().unwrap();
        let folder_name = format!("{}.uarch", name);
        PluginList::deserialize(&folder_name);

        // Handling the timestamp.
        timestamp::initialize();
        crate::chronic::on_loading_snapshot(&name);
        timestamp::deserialize(&folder_name);
    }

    // This function is called after the snapshot is loaded.
    on_finish_loading_snapshot();
}

#[unsafe(no_mangle)]
unsafe extern "C" fn qemu_plugin_exit(_: qemu_api::qemu_plugin_id_t, _: *mut ffi::c_void) {
    BranchPredictorPlugin::shutdown_logs();
    BranchPredictorPlugin::dump_tage_decision_trace(".");
}

#[unsafe(no_mangle)]
unsafe extern "C" fn qemu_plugin_install(
    id: qemu_api::qemu_plugin_id_t,
    qemu_info: *const qemu_api::qemu_info_t,
    argc: i32,
    argv: *const *const u8,
) -> i32 {
    unsafe {
        // make sure that the number of vCPUs is equal to the core count.
        assert_eq!(
            qemu_api::qemu_plugin_n_vcpus(),
            parameter::CORE_COUNT as i32,
            "Unmatched core count, thus exit."
        );

        // check system emulation cost.
        assert!(
            qemu_info.as_ref().unwrap().system_emulation,
            "Only support system emulation mode, thus exit."
        );

        // check the architectural name
        assert_eq!(
            ffi::CStr::from_ptr(qemu_info.as_ref().unwrap().target_name)
                .to_str()
                .unwrap(),
            "aarch64",
            "Only support aarch64 architecture, thus exit."
        );

        let mut options = FxHashMap::default();

        // Now, we collect the options.
        for i in 0..argc as usize {
            let arg = ffi::CStr::from_ptr(*argv.offset(i as isize) as *const i8)
                .to_str()
                .unwrap();
            let mut iter = arg.split("=");
            let key = iter.next().unwrap();
            let value = iter.next().unwrap();
            options.insert(key.to_string(), value.to_string());
        }

        qemu_api::qemu_plugin_register_vcpu_tb_trans_cb(id, Some(vcpu_tb_trans));
        qemu_api::qemu_plugin_register_atexit_cb(id, Some(qemu_plugin_exit), std::ptr::null_mut());
        qemu_api::qemu_plugin_register_savevm_cb(Some(savevm_cb));
        qemu_api::qemu_plugin_register_loadvm_cb(Some(loadvm_cb));
        PluginList::init(id, &options);

        chronic_behavior_init(&options);

        if parameter::ENABLE_STATISTICS {
            debug::statistics::create_thread_for_periodic_log();
        }

        // Dump the PARAMETER_RS to a log file.
        let mut log_file = std::fs::File::create("parameter.rs").unwrap();
        log_file.write_all(PARAMETER_RS.as_bytes()).unwrap();
        drop(log_file);

        0
    }
}
