mod config;
mod fixes;
mod utils;

use std::ffi::c_void;
use std::panic::catch_unwind;

use utils::{ModuleInfo, init_log_file, log, log_error};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::GetModuleHandleA;
use windows::Win32::System::SystemServices::DLL_PROCESS_ATTACH;

/// Opens the log next to this DLL and records what is being patched.
fn log_open(this_module: HMODULE, module: &ModuleInfo) {
    let name = "red_dead_redemption_fix.log";
    match config::module_dir(this_module) {
        Some(dir) => init_log_file(dir.join(name)),
        None => init_log_file(name),
    }

    let profile = if cfg!(debug_assertions) { "debug" } else { "release" };
    log("-------------------------------------");
    log(&format!("Version: {}-{} ({profile})", env!("CARGO_PKG_VERSION"), env!("GIT_HASH")));
    log(&format!("Rustc: {}", env!("RUSTC_VERSION")));
    log(&format!("Module Name: {}", module.name));
    log(&format!("Module Path: {}", module.path));
    log(&format!("Module Addr: {:#x}", module.address.0 as usize));
    log(&format!("Module Size: {:#x} ({} bytes)", module.size, module.size));
    log("-------------------------------------");
}

/// Logs whether `fix` applied.
fn report(fix: &str, applied: bool) {
    if applied {
        log(&format!("main: {fix} applied"));
    } else {
        log_error(&format!("main: {fix} not applied -- see the lines above"));
    }
}

/// Applies every fix, synchronously, from `DllMain`.
///
/// Every fix hooks code the game runs while it draws or updates its UI, and
/// Ultimate ASI Loader maps the plugin while the game is still starting up,
/// before the first frame. Nothing here is unsafe under the loader lock: no
/// `LoadLibrary`, no waiting on other threads; only file I/O, pattern scans
/// and code patches. No game function is called here: parts of the
/// executable are decrypted at runtime, and every site this mod scans for
/// lies outside them.
///
/// Panics are isolated and captured so if one is thrown it does not crash the
/// process and it is logged for debugging.
fn main(this_module: HMODULE) {
    // Route a panic's message and location to the log file rather than letting it vanish.
    std::panic::set_hook(Box::new(|info| log_error(&format!("panic: {info}"))));

    let result = catch_unwind(|| {
        let handle = unsafe { GetModuleHandleA(None) }.expect("failed to get module handle");
        let module = ModuleInfo::new(handle);
        log_open(this_module, &module);

        let config = config::load(this_module);
        if !config.super_enable {
            log("main: super_enable is false in the config, mod not applied");
            return;
        }
        if !config.hud.enable {
            log("main: hud.enable is false in the config, the HUD stays at the screen edges");
            return;
        }
        report("hud (movies)", fixes::hud::center_movies(&module));
        report("hud (text scale)", fixes::hud::follow_text_scale(&module));
    });

    if result.is_err() {
        log_error("main: panicked -- see the panic line above in this log for details");
    }
}

/// Entry point of the DLL.
#[unsafe(no_mangle)]
extern "system" fn DllMain(hinst: HMODULE, reason: u32, _reserved: *mut c_void) -> i32 {
    if reason == DLL_PROCESS_ATTACH {
        main(hinst);
    }
    1
}
