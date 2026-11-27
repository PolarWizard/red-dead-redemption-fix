//! Cutscenes.

use super::{ASPECT_16_9, WIDER_BY};
use crate::utils::{ModuleInfo, SignatureHook, find, log, log_error, read, write_code};

/// Stops the `popup` movie drawing pillarbox bars in cutscenes on a region
/// wider than 16:9.
///
/// # How it was found
///
/// While a cutscene plays and no overlay is up, `FUN_1401f5820` sets the byte
/// `DAT_1422d9be7`. The movie loop `FUN_14011a8f0` reads it to show and draw
/// `popup` (subtitles, prompts) even when the movie is not active, and
/// `FSCommand:GetShowBlackBars` (`FUN_1401189a0`) copies it into the movie's
/// `show_black_bars` variable:
///
/// ```text
/// MOVZX EAX,byte ptr [DAT_1422d9be7]   ; <- patched      BARS_SIG starts here
/// MOV   byte ptr [RCX],AL              ; variable value
/// MOV   dword ptr [RCX+0x10],2         ; type tag
/// ```
///
/// Nothing else reads the byte.
///
/// # Why it works
///
/// The `popup` script lays out bars from `show_black_bars` and the aspect
/// reported by `GetAspectRatio`, which the HUD fix sets to the region's.
/// `MOVZX EAX,[rip+disp]` (7 bytes) becomes `XOR EAX,EAX` plus a 5-byte NOP,
/// so the script reads the value it sees outside cutscenes. The forced show
/// and draw in the movie loop still read the byte, so subtitles and prompts
/// appear as before.
pub fn hide_black_bars(module: &ModuleInfo, aspect: f32) -> bool {
    // The IsWide and IsHighDef handlers share the load; the padding and the
    // next handler's prologue make the signature unique.
    const BARS_SIG: &str = "0F B6 05 ?? ?? ?? ??    88 01    C7 41 10 02 00 00 00    48 83 C4 28    C3    \
         48 8D 48 08    EB E0    CC CC CC CC CC CC CC CC CC CC CC CC CC CC    \
         48 83 EC 28    4C 8B DA    48 8D 0D ?? ?? ?? ??    33 D2    E8 ?? ?? ?? ??    \
         4D 8B 8B 10 01 00 00    8D 0C C0    4D 85 C9    74 ??    8B C1    33 D2";

    if aspect <= ASPECT_16_9 * WIDER_BY {
        log("cutscene_bars : not needed at the 16:9 preset, where the bars have no width");
        return true;
    }
    let site = SignatureHook { tag: "cutscene_bars", signature: BARS_SIG, offset: 0 };
    let Some(load) = find(module, &site) else { return false };
    let opcode = unsafe { read::<[u8; 3]>(load) };
    if opcode != [0x0F, 0xB6, 0x05] {
        log_error(&format!("cutscene_bars : expected 0F B6 05 @ {}+{:x}, found {opcode:02X?}", module.name, module.rva(load)));
        return false;
    }
    write_code(load, &[0x31, 0xC0, 0x0F, 0x1F, 0x44, 0x00, 0x00]);
    log(&format!("cutscene_bars : show_black_bars load -> 0 @ {}+{:x}", module.name, module.rva(load)));
    true
}
