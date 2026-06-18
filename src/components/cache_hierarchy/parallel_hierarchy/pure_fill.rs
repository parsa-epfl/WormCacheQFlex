use std::sync::{
    OnceLock,
    atomic::{AtomicBool, Ordering},
};

use crate::{parameter, qemu_api};
use spin::mutex::SpinMutex;

static PURE_FILL_CHECKPOINT_CREATED: AtomicBool = AtomicBool::new(false);
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

        let _ = snapshot_info_guard.take().unwrap();

        // println!("Snapshot request: {}", &snapshot_info.0);

        // let c_snapshot_name = std::ffi::CString::new(snapshot_info.0.clone()).unwrap();

        // qemu_api::qemu_plugin_savevm(
        //     c_snapshot_name.as_ptr(),
        //     qemu_api::qemu_plugin_snapshot_format_t_QEMU_PLUGIN_SNAPSHOT_FORMAT_EXTERNAL_INCREMENTAL_BASE,
        // );
        //
        qemu_api::qemu_plugin_notify_fully_warmed();

        PURE_FILL_CHECKPOINT_CREATED.store(true, Ordering::SeqCst);
    }
}

static SNAPSHOT_NAME: OnceLock<String> = OnceLock::new();
static WARM_RATIO: OnceLock<f64> = OnceLock::new();

unsafe extern "C" fn quantum_checking_callback(_: u64) -> bool {
    if PURE_FILL_CHECKPOINT_CREATED.load(std::sync::atomic::Ordering::SeqCst) {
        return false;
    }

    let warm_ratio = *WARM_RATIO.get().unwrap();
    // An all-phantom node (REAL_CORE_COUNT == 0) warms nothing, so the shared cache never fills and
    // the warmed-set count stays 0 — treat it as fully warmed immediately so it signals "ready to
    // checkpoint" right away instead of hanging the master forever waiting for CTRL_CKP_INIT.
    let warmed = if parameter::REAL_CORE_COUNT == 0 {
        true
    } else {
        let warmed_set = unsafe { (*super::PLUGIN).get_scache_warmed_set_count() };
        warmed_set >= (parameter::SHARED_CACHE_SET as f64 * warm_ratio) as usize
    };

    if warmed {
        let snapshot_info = (SNAPSHOT_NAME.get().unwrap().clone(), 0);

        let snapshot_info_guard = SNAPSHOT_INFO.try_lock();
        if snapshot_info_guard.is_none() {
            return false;
        }

        let mut snapshot_info_guard = snapshot_info_guard.unwrap();

        if snapshot_info_guard.is_none() {
            *snapshot_info_guard = Some(snapshot_info);
            println!("All the sets are warmed up. Create a snapshot.");
            return true; // suggest a interrupt.
        }
    }
    return false;
}

pub unsafe fn init(name: &str, warm_ratio: f64) {
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
}
