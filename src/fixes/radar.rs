//! The minimap ring, which the engine draws outside the Flash player.

use super::WIDER_BY;
use crate::utils::{ModuleInfo, SignatureHook, find, hook_at, log_error, read, rip_target};

/// Moves the minimap ring from the left edge of the screen into the centered
/// region of `aspect`.
///
/// # How it was found
///
/// `FUN_140692340` draws the ring (the `RadarRingBack`/`RadarRingFront`
/// textures) with its own orthographic projection. Its only caller is
/// `FUN_1405c8720`, at `0x1405c9477`, guarded by the pause-map flag
/// `DAT_142ac52a8 == 0`. It never reaches the Flash player, so
/// [`crate::fixes::hud::center_movies`] does not move it. The virtual screen
/// is `base` units high (720, or 480 when `DAT_142abc08a` is 0) and
/// `base * aspect` wide, and both margins are 0.05 of an extent:
///
/// ```text
/// MULSS  XMM7,[DAT_141d707c0]      ; y margin = base * 0.05
/// MOVAPS XMM15,XMM8                ; base * aspect, the virtual width
/// ...
/// MULSS  XMM15,[DAT_141d707c0]     ; x margin = width * 0.05  <- RADAR_SIG
/// MOV    dword ptr [RBP+0x30],0x0  ; <- radar hook
/// ...
/// MOVSS  [RSP+0x50],XMM7           ; the y margin, stored
/// ```
///
/// XMM15's one later use is the x of the translation row the ring and its
/// clip rectangle are built from. Nothing writes XMM7 between the two
/// multiplies.
///
/// # Why it works
///
/// With `f` = 0.05 and the region's aspect `a`, XMM15 holds `width * f` and
/// XMM7 holds `base * f`, so `edge = XMM7 * a` is the margin on a screen of
/// aspect `a` and the same height. The ring sits at `edge` from the left of
/// such a screen, and the region starts `(width - base*a)/2 =
/// (XMM15 - edge)/(2f)` units from the left of this one, so the hook sets
/// `XMM15 = edge + (XMM15 - edge)/(2f)`. It leaves XMM15 alone unless the
/// screen is wider than the region (by 0.2%). The projection's scale and the
/// vertical layout do not change, so the ring keeps its size and height.
pub fn center_radar(module: &ModuleInfo, aspect: f32) -> bool {
    const RADAR_SIG: &str = "F3 44 0F 59 3D ?? ?? ?? ??    C7 45 30 00 00 00 00    F3 44 0F 10 25 ?? ?? ?? ??    \
         F3 0F 11 7C 24 50";
    const MARGIN_MULTIPLY: usize = 0;
    const AFTER_MARGIN_MULTIPLY: usize = 9;

    let site = SignatureHook { tag: "hud_radar", signature: RADAR_SIG, offset: 0 };
    let Some(hit) = find(module, &site) else { return false };

    // MULSS XMM15,[rip+disp32]: F3 44 0F 59 3D, displacement at +5, 9 bytes.
    let factor = unsafe { read::<f32>(rip_target(hit + MARGIN_MULTIPLY, 5, 9)) };
    if !(factor > 0.0 && factor < 0.5) {
        log_error(&format!("hud_radar : margin factor {factor} is not a fraction of the screen; not hooked"));
        return false;
    }

    hook_at(module, site.tag, hit + AFTER_MARGIN_MULTIPLY, move |ctx| {
        let margin = ctx.xmm15.f32()[0];
        let edge = ctx.xmm7.f32()[0] * aspect;
        if margin > edge * WIDER_BY {
            ctx.xmm15.f32()[0] = edge + (margin - edge) / (2.0 * factor);
        }
    })
}
