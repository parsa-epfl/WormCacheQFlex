use std::sync::OnceLock;

use crate::{parameter, qemu_api};
use spin::mutex::SpinMutex;

static SNAPSHOT_INFO: SpinMutex<Option<(String, u64)>> = SpinMutex::new(None);

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

        println!("Snapshot request: {}", &snapshot_info.0);

        let c_snapshot_name = std::ffi::CString::new(snapshot_info.0.clone()).unwrap();

        qemu_api::qemu_plugin_savevm(
            c_snapshot_name.as_ptr(),
            qemu_api::qemu_plugin_snapshot_format_t_QEMU_PLUGIN_SNAPSHOT_FORMAT_EXTERNAL_INCREMENTAL_BASE,
        );

        std::process::exit(0);
    }
}

static SNAPSHOT_NAME: OnceLock<String> = OnceLock::new();
static WARM_RATIO: OnceLock<f64> = OnceLock::new();
static FALLBACK_CYCLES: OnceLock<Option<u64>> = OnceLock::new();
static mut CURRENT_CYCLES: u64 = 0;

fn try_request_snapshot(snapshot_info: (String, u64), reason: &str) -> bool {
    let snapshot_info_guard = SNAPSHOT_INFO.try_lock();
    if snapshot_info_guard.is_none() {
        return false;
    }

    let mut snapshot_info_guard = snapshot_info_guard.unwrap();

    if snapshot_info_guard.is_none() {
        *snapshot_info_guard = Some(snapshot_info);
        println!("{}", reason);
        return true;
    }

    false
}

unsafe extern "C" fn quantum_checking_callback(diff: u64) -> bool {
    unsafe {
        CURRENT_CYCLES += diff;
    }

    let warmed_set = unsafe { (*super::PLUGIN).get_scache_warmed_set_count() };
    let warm_ratio = *WARM_RATIO.get().unwrap();
    let current_cycles = unsafe { CURRENT_CYCLES };

    if warmed_set >= (parameter::SHARED_CACHE_SET as f64 * warm_ratio) as usize {
        if try_request_snapshot(
            (SNAPSHOT_NAME.get().unwrap().clone(), current_cycles),
            "All the sets are warmed up. Create a snapshot.",
        ) {
            return true;
        }
    }

    if let Some(fallback_cycles) = *FALLBACK_CYCLES.get().unwrap() {
        if current_cycles >= fallback_cycles
            && try_request_snapshot(
                (SNAPSHOT_NAME.get().unwrap().clone(), current_cycles),
                "Fallback warmup cycle threshold reached. Create a snapshot.",
            )
        {
            return true;
        }
    }

    return false;
}

pub unsafe fn init(name: &str, warm_ratio: f64, fallback_cycles: Option<u64>) {
    unsafe {
        assert!(qemu_api::qemu_plugin_register_event_loop_poll_cb(Some(
            event_loop_callback
        )));

        assert!(qemu_api::qemu_plugin_register_periodic_check_cb(Some(
            quantum_checking_callback
        )));
    }

    SNAPSHOT_NAME.set(format!("{}_{}", name, "warmed")).unwrap();

    assert!(warm_ratio >= 0.0 && warm_ratio <= 1.0);
    WARM_RATIO.set(warm_ratio).unwrap();
    FALLBACK_CYCLES.set(fallback_cycles).unwrap();
    unsafe {
        CURRENT_CYCLES = 0;
    }
}
