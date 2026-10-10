//! Every correction this project applies for ultrawide play, grouped by the
//! part of the game each one touches.
//!
//! # How the game draws its HUD
//!
//! Reference build: Steam `RDR.exe`, x86-64, image base `0x140000000`, MD5
//! `7c550c7a9fcc74449e703a68ebf94fdd`. `FUN_`/`DAT_` names are Ghidra's
//! default labels for that build. The patches locate their code by signature.
//!
//! * **Flash movies.** The HUD is a set of Flash movies played by the game's
//!   own player (the `swfFILE`, `swfSPRITE` and `swfEDITTEXT` type names), not
//!   Scaleform. The movie manager `DAT_142c45610` holds up to 0x28 movie
//!   wrappers. Each frame `FUN_14011a8f0` draws the visible ones through
//!   wrapper vtable slot +0x28, `FUN_1401157d0`, which calls the player's draw
//!   `FUN_1401810d0`.
//! * **The stage is stretched to the viewport.** `FUN_1401810d0` sets two
//!   scale globals, `scaleX = viewport width / stage width` and
//!   `scaleY = viewport height / stage height`, builds a root context whose
//!   matrix is the identity, and walks the display list. Every vertex the
//!   player emits is `((a*x + c*y + tx) * scaleX, (b*x + d*y + ty) * scaleY)`.
//!   A 1280x720 stage on a 32:9 screen is drawn at twice its width.
//! * **The scripts compensate.** The game answers each movie's
//!   `FSCommand:GetAspectRatio` with the screen's aspect (`FUN_1401188e0`),
//!   which the movie's ActionScript can use to undo the stretch and push its
//!   elements out to the screen edges. The scripts live in `flash.rpf`, not in
//!   the executable, so this half is inferred from the stretch and the
//!   variable rather than read.
//! * **The radar ring is not Flash.** `FUN_140692340` draws it with its own
//!   orthographic projection, 5% of the full screen width from the left edge.
//!
//! # Keeping the HUD in a centered 16:9 region
//!
//! For the HUD movies (`mp_HUD`, `popup` and `numberSystem`), [`hud`] hands
//! the player a 16:9 screen in the middle of the real one: it narrows `scaleX`
//! to the region, moves the root translation to the region's left edge, and
//! reports 16:9 to the movie's scripts so they lay out as on a 16:9 monitor.
//! [`radar`] moves the ring into the same region. The 3D view, fades,
//! post-process tints and the pause map keep the full screen.

pub mod hud;
pub mod radar;

/// Aspect of the region the HUD is kept in.
const ASPECT_16_9: f32 = 16.0 / 9.0;

/// Screens up to 0.2% wider than 16:9 count as 16:9, so 1920x1080 and
/// 2560x1440 draw exactly as without the mod.
const WIDER_THAN_16_9: f32 = ASPECT_16_9 * 1.002;
