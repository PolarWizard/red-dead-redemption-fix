//! The startup sequence.

use crate::utils::{ModuleInfo, SignatureHook, find, log, log_error, read, write_code};

/// Skips the logo video the title screen plays after the legal text.
///
/// # How it was found
///
/// `FUN_1405f4690` builds the title screen, which `FUN_1405f4390` draws
/// during startup. It preloads the video `$/tune/logo_hires` (or
/// `logo_lowres` below 1920 wide) into `[screen+0x18]`, unless a startup
/// parameter slot, `DAT_142b67d80`, is set. Nothing in the retail build sets
/// that slot:
///
/// ```text
/// CMP qword ptr [DAT_142b67d80],RDI  ; RDI = 0     <- LOGO_SIG starts here
/// JNZ no_video                       ; <- patched to JMP
/// CALL GetCurrentThreadId            ; pick logo_hires or logo_lowres ...
/// MOV  ECX,[DAT_1422722d4]           ; screen width
/// ```
///
/// # Why it works
///
/// The update `FUN_1405f5070` starts the video once the legal text reports
/// `LegalsCompleted`. When `[screen+0x18]` is null it skips the video and
/// marks the title screen done (`screen+0x11 = 1`), the same path a missing
/// video file takes. `JNZ rel32` (`0F 85`) becomes `NOP; JMP rel32` (`90 E9`):
/// both instructions end at the same address, so the displacement still
/// reaches `no_video`.
pub fn skip_logo_video(module: &ModuleInfo) -> bool {
    const LOGO_SIG: &str = "48 39 3D ?? ?? ?? ??    0F 85 ?? ?? ?? ??    E8 ?? ?? ?? ??    8B 0D ?? ?? ?? ??";
    const BRANCH: usize = 7;

    let site = SignatureHook { tag: "intro_logo", signature: LOGO_SIG, offset: BRANCH };
    let Some(branch) = find(module, &site) else { return false };
    let opcode = unsafe { read::<[u8; 2]>(branch) };
    if opcode != [0x0F, 0x85] {
        log_error(&format!("intro_logo : expected 0F 85 @ {}+{:x}, found {opcode:02X?}", module.name, module.rva(branch)));
        return false;
    }
    write_code(branch, &[0x90, 0xE9]);
    log(&format!("intro_logo : JNZ -> JMP @ {}+{:x}", module.name, module.rva(branch)));
    true
}
