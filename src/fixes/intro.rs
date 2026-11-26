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

/// Skips the legal screens the title screen shows before the logo video.
///
/// # How it was found
///
/// The title screen `FUN_1405f4690` plays the `legals` Flash movie
/// (`FUN_1401fced0`). While the screen is up (`[screen+0x8]`, 10000 from the
/// constructor), its update `FUN_1405f5070` reads the movie's
/// `LegalsCompleted` variable every frame and moves on only once the movie's
/// script has set it:
///
/// ```text
/// MOV    RCX,[RDI]                ; the movie      <- LEGALS_SIG starts here
/// MOVZX  R9D,SIL
/// MOVSS  XMM1,[1/30]
/// XOR    R8D,R8D
/// MOV    RAX,[RCX]
/// CALL   [RAX+0x20]               ; advance the movie
/// TEST   BPL,BPL                  ; LegalsCompleted
/// JZ     not_yet                  ; <- patched to NOPs
/// CMP    [RDI+0x18],R15           ; the logo video, if one was loaded
/// ```
///
/// The game leaves the title screen only once it is marked done
/// (`screen+0x11`) and its first load has finished (`[DAT_142aac8b8+0x78]`,
/// checked in `FUN_1401f5820`). Until then `FUN_1405f4cc0` clears the screen
/// to black and draws the movie every frame:
///
/// ```text
/// MOV    RCX,[DAT_142ac09e0]      ;                <- MOVIE_DRAW_SIG starts here
/// CALL   FUN_1401512a0
/// MOV    RCX,[RDI]                ; the movie
/// MOV    RAX,[RCX]
/// CALL   [RAX+0x28]               ; draw it        <- patched to NOPs
/// CMP    [RDI+0x30],RBP           ; the logo video, if one is playing
/// ```
///
/// # Why it works
///
/// Without the branch the update takes the completed path on the first frame
/// the screen is up: it starts the logo video or, when [`skip_logo_video`]
/// left none, marks the title screen done (`screen+0x11 = 1`). That is the
/// path the game takes when the movie reports the legal screens finished.
/// The first load still takes as long as it takes; without the draw the
/// screen stays black over it instead of showing the legal pages, and the
/// start screen follows as usual.
pub fn skip_legal_screens(module: &ModuleInfo) -> bool {
    const LEGALS_SIG: &str = "48 8B 0F    44 0F B6 CE    F3 0F 10 0D ?? ?? ?? ??    45 33 C0    48 8B 01    FF 50 20    \
         40 84 ED    0F 84 ?? ?? ?? ??    4C 39 7F 18";
    const BRANCH: usize = 27;
    const MOVIE_DRAW_SIG: &str = "48 8B 0D ?? ?? ?? ??    E8 ?? ?? ?? ??    48 8B 0F    48 8B 01    FF 50 28    \
         48 39 6F 30    0F 84";
    const DRAW_CALL: usize = 18;

    let legals = SignatureHook { tag: "intro_legals", signature: LEGALS_SIG, offset: BRANCH };
    let draw = SignatureHook { tag: "intro_legals_draw", signature: MOVIE_DRAW_SIG, offset: DRAW_CALL };
    let (Some(branch), Some(call)) = (find(module, &legals), find(module, &draw)) else { return false };
    let (opcode, call_bytes) = unsafe { (read::<[u8; 2]>(branch), read::<[u8; 3]>(call)) };
    if opcode != [0x0F, 0x84] || call_bytes != [0xFF, 0x50, 0x28] {
        log_error(&format!(
            "intro_legals : expected 0F 84 @ {}+{:x} and FF 50 28 @ {}+{:x}, found {opcode:02X?} and {call_bytes:02X?}",
            module.name,
            module.rva(branch),
            module.name,
            module.rva(call)
        ));
        return false;
    }
    write_code(branch, &[0x90; 6]);
    write_code(call, &[0x90; 3]);
    log(&format!(
        "intro_legals : JZ -> NOP @ {}+{:x}, movie draw -> NOP @ {}+{:x}",
        module.name,
        module.rva(branch),
        module.name,
        module.rva(call)
    ));
    true
}
