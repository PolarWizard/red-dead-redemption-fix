//! The Flash movies, the HUD and the menus: which movie is running, where the
//! player puts its stage on screen, and which aspect ratio its scripts are
//! told.

use std::cell::Cell;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use grapnel::Context;

use super::{ASPECT_16_9, WIDER_THAN_16_9};
use crate::utils::{
    ModuleInfo, SignatureHook, detour_at, find, hook_at, log, log_error, log_warn, read, readable, rip_target, write,
};

/// Movies that keep the full screen. Every other movie with a 16:9 stage is
/// drawn into the centered region. A name matches without regard to case, on
/// its own or with the `_z` suffix of its Undead Nightmare variant.
///
/// `fader` and `effects` cover the whole screen. `mapRenderProxy` draws over
/// the pause map, which the engine draws across the full screen
/// (`FUN_140600f20`). `multiplayerIcons` places tags over players from their
/// screen positions (`FUN_1404c6560`).
const FULL_SCREEN_MOVIES: [&str; 4] = ["fader", "effects", "mapRenderProxy", "multiplayerIcons"];

/// `wrapper+0x8` points at the movie's definition. `FUN_1401190d0` registers
/// `*(wrapper+8) + 0x28` as the movie's lookup name, and `FUN_14011a8f0`
/// compares the same inline string with `"popup"`.
const WRAPPER_DEFINITION: usize = 0x8;
const DEFINITION_NAME: usize = 0x28;
/// The longest name read. The names the game compares are far shorter.
const NAME_CAPACITY: usize = 32;
/// The base constructor `FUN_140114cb0` zeroes `wrapper+0x8`, and the derived
/// constructors then store the definition; a value below this is not one.
const LOWEST_POINTER: usize = 0x10000;
/// `wrapper+0x48` is the player: `FUN_1401157d0` draws it with
/// `MOV RCX,[RDI+0x48]; MOV RAX,[RCX]; CALL [RAX+0x28]`.
const WRAPPER_PLAYER: usize = 0x48;
/// `player+0x48` is the loaded swf: `FUN_1401810d0` starts with
/// `MOV R8,[RCX+0x48]`.
const PLAYER_SWF: usize = 0x48;
/// The swf's stage width and height in pixels, as floats: `FUN_1401810d0`
/// divides by them with `DIVSS XMM1,[R8+0x40]` and `DIVSS XMM1,[R8+0x48]`.
const SWF_STAGE_WIDTH: usize = 0x40;
const SWF_STAGE_HEIGHT: usize = 0x48;
/// `FUN_1401810d0` builds the root context at `RSP+0x20`. Its matrix sits at
/// +0x28..+0x3c as a, b, c, d, tx, ty; the constructor `FUN_14017f340`
/// stores 1.0 to +0x28 and +0x34 and zero to the rest.
const ROOT_CONTEXT: usize = 0x20;
const CONTEXT_TX: usize = 0x38;
/// The executable's thread-local block is `[[GS:0x58]]`, slot 0. In it,
/// `+0x38` is non-zero once the block is initialised and `+0x28` points at
/// the thread's render context, as `FUN_14012f2f0` reads them. The context
/// keeps its scissor rectangle at `+0x148` as left, right, top, bottom
/// (`FUN_140ec7e90`: `MOVUPS [RCX+0x148],XMM0`).
const TLS_READY: usize = 0x38;
const TLS_RENDER_CONTEXT: usize = 0x28;
const CONTEXT_SCISSOR: usize = 0x148;

/// The movie wrapper whose load, update or draw runs on this thread.
#[derive(Clone, Copy)]
struct Current {
    /// `this` of `FUN_140114d80`, `FUN_140115660` or `FUN_1401157d0`; 0
    /// outside all three.
    wrapper: usize,
    /// The wrapper's definition, the key of [`MOVIES`].
    definition: usize,
    /// Whether the movie is not on [`FULL_SCREEN_MOVIES`].
    centered: bool,
    /// Where [`place_movie`] put the movie, while its display list is walked.
    layout: Option<Layout>,
}

impl Current {
    const NONE: Self = Self { wrapper: 0, definition: 0, centered: false, layout: None };
}

/// A centered movie's placement, kept while its display list is walked.
#[derive(Clone, Copy)]
struct Layout {
    /// The root translation, in stage units.
    shift: f32,
    /// The scissor rectangle before [`clip_to_region`].
    scissor: Option<[i32; 4]>,
}

thread_local! {
    static CURRENT: Cell<Current> = const { Cell::new(Current::NONE) };
}

/// A movie [`classify`] has seen, keyed by its definition.
struct Movie {
    definition: usize,
    name: String,
    centered: bool,
    /// Whether [`note_first_draw`] has logged it.
    drawn: bool,
}

static MOVIES: Mutex<Vec<Movie>> = Mutex::new(Vec::new());

/// Original `FUN_140114d80` (load), `FUN_140115660` (update) and
/// `FUN_1401157d0` (draw), reached through grapnel's trampolines.
static LOAD: AtomicUsize = AtomicUsize::new(0);
static UPDATE: AtomicUsize = AtomicUsize::new(0);
static DRAW: AtomicUsize = AtomicUsize::new(0);

/// `FUN_140ec7e90(context, rect)`, the render context's scissor setter; 0
/// when its signature was not found and movies are not clipped.
static SET_SCISSOR: AtomicUsize = AtomicUsize::new(0);

static ASPECT_OUTSIDE_LOGGED: AtomicBool = AtomicBool::new(false);

type LoadFn = unsafe extern "system" fn(usize, u32) -> u64;
type WrapperFn = unsafe extern "system" fn(usize);
type SetScissorFn = unsafe extern "system" fn(usize, *const [i32; 4]);

/// Draws the HUD and the menus into a centered 16:9 region, clipped to it,
/// and tells their scripts the screen is 16:9.
///
/// # How it was found
///
/// The scale globals `DAT_142ac2844` (x) and `DAT_142ac2850` (y) are written
/// only in `FUN_1401810d0`, the swf player's draw (vtable `0x141c73130` slot
/// +0x28), and zeroed again right after its display-list walk. All seven
/// reads of `scaleX`, in the vertex emitters `FUN_1401959d0`,
/// `FUN_140195c90` (two), `FUN_1401a7d20`, `FUN_1401a7e30`, `FUN_1401a7f40`
/// and `FUN_1401a8490`, add the matrix translation before they scale, for
/// example in `FUN_1401a7d20`:
///
/// ```text
/// ADDSS XMM0,[tx]                 ; a*x + c*y + tx
/// MULSS XMM0,[DAT_142ac2844]      ; * scaleX
/// ```
///
/// The walk `FUN_14018dc30` composes each node's world matrix with
/// `FUN_14018a730(node+0x58, node+0x40, parent+0x28)`, so the root context's
/// translation reaches every node.
///
/// ```text
/// MOVSS [DAT_142ac2844],XMM1      ; scaleX         <- PLACE_SIG starts here
/// ...
/// MOVSS [DAT_142ac2850],XMM1      ; scaleY
/// CALL  FUN_14017f340             ; root context at RSP+0x20 = identity
/// LEA   RDX,[RSP+0x20]            ; <- place hook
/// MOV   R8,R10                    ; the player
/// MOV   RCX,R9                    ; the root node
/// CALL  FUN_14018dc30             ; walk the display list
/// MOV   RCX,[RSP+0xc8]            ; <- scissor restore hook
/// ```
///
/// The game answers `FSCommand:GetAspectRatio` in `FUN_1401188e0`, one of the
/// handlers `FUN_140117750` registers. It loads the aspect on two paths (the
/// main thread reads `DAT_142ae94ec`, other threads `DAT_142b005dc`) that
/// join at the store into the movie's `fs_AspectRatio` variable:
///
/// ```text
/// MOVSS XMM0,[DAT_142ae94ec]      ; main thread     <- ASPECT_SIG starts here
/// JMP   store
/// ...
/// MOVSS XMM0,[DAT_142b005dc]      ; other threads
/// store:
/// MOVSS [RBX],XMM0                ; <- aspect hook
/// MOV   dword ptr [RBX+0x10],0x4  ; tag the variable as a float
/// ```
///
/// Every movie wrapper loads its swf in `FUN_140114d80`, updates it, which
/// runs its scripts, in `FUN_140115660`, and draws it in `FUN_1401157d0`
/// (vtable slots +0x8, +0x18 and +0x28 of all 36 wrapper classes). The
/// update and the draw take only `this` in RCX; the load also takes an
/// integer in EDX (`MOV ESI,EDX`).
///
/// The letterbox `FUN_1405bdd30` clips its clears with `FUN_14012f2f0(x, y,
/// w, h)`, which stores `{x, x+w, y, y+h}` through `FUN_140ec7e90` into the
/// render context of the calling thread. Nothing in the Flash player calls
/// either.
///
/// # Why it works
///
/// A viewport of `W`x`H` pixels and a 1280x720 stage give `scaleX = W/1280`
/// and `scaleY = H/720`. The place hook replaces them, for a centered movie on
/// a screen wider than 16:9, with `w = H*16/9`, `x0 = (W - w)/2`,
/// `scaleX = w/1280` and root `tx = x0/scaleX`. A stage point `x` then lands
/// on `(x + tx) * scaleX = x0 + x*w/1280`: the stage fills the centered
/// region exactly as it fills a 16:9 screen, square pixels included. The game
/// zeroes both scale globals after the walk and rebuilds the root context for
/// every movie, so the change never reaches another movie.
///
/// On a 16:9 screen nothing outside the stage is visible. Centered, anything
/// a movie places beyond its stage edge would show beside the region, so the
/// place hook also narrows the scissor to the region, and the restore hook
/// puts the previous rectangle back after the walk. The viewport and the
/// aspect globals stay as they are, so everything full-screen stays
/// full-screen.
///
/// The aspect hook lowers the reported aspect to 16:9 while a centered movie
/// loads, updates or draws, so its scripts lay it out for the region it is
/// drawn into. Markers that game code places from 3D positions
/// (`FUN_1406c59f0`: `x = (px - W/2)*720/H`, centered on the screen) land on
/// the same pixels in a 16:9 stage centered on the screen.
///
/// The detours on the three wrapper functions record which movie runs on
/// the current thread, so the hooks above know when to act. The hooks install
/// all-or-nothing, placement first and the movie detours last, so a partial
/// failure leaves every movie as the game draws it. Clipping is skipped, with
/// a warning, if its signatures are missing.
pub fn center_movies(module: &ModuleInfo) -> bool {
    const LOAD_SIG: &str = "48 89 5C 24 08    48 89 74 24 10    57    48 81 EC ?? ?? ?? ??    8B F2    48 8B F9    \
         48 8B 15 ?? ?? ?? ??    E8 ?? ?? ?? ??";
    const UPDATE_SIG: &str = "40 53    48 83 EC ??    48 8B D9    E8 ?? ?? ?? ??    FF 15 ?? ?? ?? ??    33 C9    \
         E8 ?? ?? ?? ??    80 7B 55 00    74 ??";
    const DRAW_SIG: &str = "40 57    48 81 EC ?? ?? ?? ??    48 8B F9    FF 15 ?? ?? ?? ??    80 7F 19 00    \
         0F 84 ?? ?? ?? ??    48 83 7F 48 00";
    const PLACE_SIG: &str = "F3 0F 11 0D ?? ?? ?? ??    66 0F 6E C8    0F 5B C9    F3 41 0F 5E 48 48    \
         F3 0F 11 0D ?? ?? ?? ??    E8 ?? ?? ?? ??    48 8D 54 24 20    4D 8B C2    49 8B C9    E8 ?? ?? ?? ??    \
         48 8B 8C 24 ?? ?? ?? ??";
    const SCALE_X_STORE: usize = 0;
    const SCALE_Y_STORE: usize = 21;
    const AFTER_ROOT_CONTEXT: usize = 34;
    const AFTER_WALK: usize = 50;
    const ASPECT_SIG: &str = "F3 0F 10 05 ?? ?? ?? ??    EB ??    48 8D 5A 08    EB ??    F3 0F 10 05 ?? ?? ?? ??    \
         F3 0F 11 03    C7 43 10 04 00 00 00";
    const ASPECT_STORE: usize = 24;
    // FUN_140ec7e90: MOVUPS XMM0,[RDX]; MOVUPS [RCX+0x148],XMM0;
    // MOV RCX,[RCX+0x128]; MOV RAX,[RCX]; JMP [RAX+0x80]
    const SET_SCISSOR_SIG: &str = "0F 10 02    0F 11 81 48 01 00 00    48 8B 89 28 01 00 00    48 8B 01    \
         48 FF A0 80 00 00 00";
    // FUN_14012f2f0: MOV ECX,0x38 ... CMP byte [RCX+RAX],0; JNZ; CALL init;
    // LEA EAX,[RDI+RBP]; MOV R8D,0x28 -- the TLS offsets of the ready byte
    // and the render context.
    const RENDER_CONTEXT_SIG: &str = "B9 38 00 00 00    41 8B F1    41 8B E8    8B DA    48 8B 00    80 3C 01 00    \
         75 ??    E8 ?? ?? ?? ??    8D 04 2F    41 B8 28 00 00 00";

    let load = SignatureHook { tag: "movie_load", signature: LOAD_SIG, offset: 0 };
    let update = SignatureHook { tag: "movie_update", signature: UPDATE_SIG, offset: 0 };
    let draw = SignatureHook { tag: "movie_draw", signature: DRAW_SIG, offset: 0 };
    let place = SignatureHook { tag: "movie_place", signature: PLACE_SIG, offset: 0 };
    let aspect = SignatureHook { tag: "movie_aspect", signature: ASPECT_SIG, offset: ASPECT_STORE };
    let (Some(load), Some(update), Some(draw), Some(place), Some(aspect)) = (
        find(module, &load),
        find(module, &update),
        find(module, &draw),
        find(module, &place),
        find(module, &aspect),
    ) else {
        return false;
    };
    let set_scissor = SignatureHook { tag: "movie_clip", signature: SET_SCISSOR_SIG, offset: 0 };
    let render_context = SignatureHook { tag: "movie_clip_context", signature: RENDER_CONTEXT_SIG, offset: 0 };
    let set_scissor = find(module, &set_scissor).filter(|_| find(module, &render_context).is_some());

    // The two MOVSS [rip+disp32],XMM1 stores name the scale globals.
    let scale_x = rip_target(place + SCALE_X_STORE, 4, 8);
    let scale_y = rip_target(place + SCALE_Y_STORE, 4, 8);
    log(&format!(
        "movie_place : scale globals @ {}+{:x} (x), {}+{:x} (y)",
        module.name,
        module.rva(scale_x),
        module.name,
        module.rva(scale_y)
    ));

    if !hook_at(module, "movie_place", place + AFTER_ROOT_CONTEXT, move |ctx| place_movie(ctx, scale_x, scale_y)) {
        return false;
    }
    if !hook_at(module, "movie_walked", place + AFTER_WALK, end_walk) {
        return false;
    }
    match set_scissor {
        Some(set_scissor) => SET_SCISSOR.store(set_scissor, Ordering::Release),
        None => log_warn("movie_clip : not available; centered movies are not clipped to the region"),
    }
    if !hook_at(module, "movie_aspect", aspect, clamp_aspect) {
        return false;
    }
    let detours: [(&str, usize, *const u8, &AtomicUsize); 3] = [
        ("movie_load", load, load_detour as *const u8, &LOAD),
        ("movie_update", update, update_detour as *const u8, &UPDATE),
        ("movie_draw", draw, draw_detour as *const u8, &DRAW),
    ];
    for (tag, target, detour, original) in detours {
        let Some(trampoline) = detour_at(module, tag, target, detour) else { return false };
        original.store(trampoline, Ordering::Release);
    }
    true
}

/// Keeps the subtitle and objective text-size options scaling the text about
/// the center of the region instead of the screen.
///
/// # How it was found
///
/// `FUN_14018dc30`, the display-list walk, rescales the `subtitle_txt` and
/// `objective_txt` fields of `popup` by the subtitle size `DAT_142273550` and
/// the objective size `DAT_1422734dc`. Both paths keep the text centered on
/// the stage with a literal 1280.0, the stage width, from `DAT_141d7123c`:
/// `tx' = tx - (1280 - 2*tx) * (s' - s)/s * 0.5`.
///
/// ```text
/// MOVUPS XMM3,[R15+0x58]          ; the node's world matrix
/// MOVSD  XMM6,[R15+0x68]          ; its tx, ty
/// MOVSS  XMM2,[DAT_141d7123c]     ; 1280.0
/// MOVAPS XMM4,XMM3                ; <- text scale hook
/// MULSS  XMM4,[DAT_142273550]     ; the text size option
/// ```
///
/// # Why it works
///
/// The world matrix includes the root translation [`center_movies`] adds, so
/// the stage center lies at `640 + shift` and the doubled center becomes
/// `1280 + 2*shift`. The hook adds `2*shift` to XMM2 right after the load, and
/// leaves it untouched for every movie the fix does not move. Nine other
/// functions read `DAT_141d7123c`, so the constant itself stays. At the
/// default text size `(s' - s)/s` is 0 and the term has no effect either way.
pub fn follow_text_scale(module: &ModuleInfo) -> bool {
    const SUBTITLE_SCALE_SIG: &str = "48 8B D8    E8 ?? ?? ?? ??    85 C0    0F 85 ?? ?? ?? ??    41 0F 10 5F 58    \
         F2 41 0F 10 77 68    F3 0F 10 15 ?? ?? ?? ??    0F 28 E3";
    const OBJECTIVE_SCALE_SIG: &str = "48 8B CB    E8 ?? ?? ?? ??    85 C0    0F 85 ?? ?? ?? ??    41 0F 10 5F 58    \
         F2 41 0F 10 77 68    F3 0F 10 15 ?? ?? ?? ??    0F 28 E3";
    const CENTER_LOAD: usize = 27;
    const AFTER_CENTER_LOAD: usize = 35;
    const STAGE_WIDTH: f32 = 1280.0;

    let sites = [
        SignatureHook { tag: "subtitle_scale", signature: SUBTITLE_SCALE_SIG, offset: 0 },
        SignatureHook { tag: "objective_scale", signature: OBJECTIVE_SCALE_SIG, offset: 0 },
    ];
    let mut found = Vec::with_capacity(sites.len());
    for site in &sites {
        let Some(hit) = find(module, site) else { return false };
        let constant = rip_target(hit + CENTER_LOAD, 4, 8);
        let value = unsafe { read::<f32>(constant) };
        if value != STAGE_WIDTH {
            log_error(&format!("{} : expected {STAGE_WIDTH} at {}+{:x}, found {value}", site.tag, module.name, module.rva(constant)));
            return false;
        }
        found.push((site.tag, hit + AFTER_CENTER_LOAD));
    }
    found.into_iter().all(|(tag, addr)| hook_at(module, tag, addr, follow_shift))
}

/// Detour of `FUN_140114d80`, a movie wrapper's load.
unsafe extern "system" fn load_detour(wrapper: usize, kind: u32) -> u64 {
    as_current(wrapper, || unsafe { original::<LoadFn>(&LOAD)(wrapper, kind) })
}

/// Detour of `FUN_140115660`, a movie wrapper's update, which runs the
/// movie's scripts.
unsafe extern "system" fn update_detour(wrapper: usize) {
    as_current(wrapper, || unsafe { original::<WrapperFn>(&UPDATE)(wrapper) })
}

/// Detour of `FUN_1401157d0`, a movie wrapper's draw.
unsafe extern "system" fn draw_detour(wrapper: usize) {
    as_current(wrapper, || unsafe { original::<WrapperFn>(&DRAW)(wrapper) })
}

/// The trampoline in `slot` as a function of type `F`.
///
/// # Safety
///
/// `slot` holds the trampoline of a function with the signature `F`.
unsafe fn original<F: Copy>(slot: &AtomicUsize) -> F {
    unsafe { std::mem::transmute_copy::<usize, F>(&slot.load(Ordering::Acquire)) }
}

/// Runs `run` with `wrapper` as this thread's [`CURRENT`] movie, then
/// restores whichever movie was current before.
fn as_current<R>(wrapper: usize, run: impl FnOnce() -> R) -> R {
    let previous = CURRENT.with(|c| c.replace(classify(wrapper)));
    let result = run();
    CURRENT.with(|c| c.set(previous));
    result
}

/// The place hook: narrows the stage of a centered movie to the region and
/// clips it there. R8 holds the swf, and the root context is at `RSP+0x20`.
fn place_movie(ctx: &mut Context, scale_x: usize, scale_y: usize) {
    let current = CURRENT.with(|c| c.get());
    if current.wrapper == 0 {
        return;
    }
    let swf = ctx.r8 as usize;
    let stage = unsafe { (read::<f32>(swf + SWF_STAGE_WIDTH), read::<f32>(swf + SWF_STAGE_HEIGHT)) };
    let viewport = unsafe { (read::<f32>(scale_x) * stage.0, read::<f32>(scale_y) * stage.1) };
    let placement = if current.centered { centered(stage, viewport) } else { None };
    if let Some(placement) = &placement {
        unsafe {
            write::<f32>(scale_x, placement.scale_x);
            write::<f32>(ctx.rsp as usize + ROOT_CONTEXT + CONTEXT_TX, placement.shift);
        }
        let layout = Layout { shift: placement.shift, scissor: clip_to_region(placement.left, placement.width) };
        CURRENT.with(|c| c.set(Current { layout: Some(layout), ..current }));
    }
    note_first_draw(current.definition, stage, viewport, placement.as_ref());
}

/// Narrows the render context's scissor to the pixels `left..left + width`,
/// returning the rectangle it replaced.
fn clip_to_region(left: f32, width: f32) -> Option<[i32; 4]> {
    let context = render_context()?;
    let saved = unsafe { read::<[i32; 4]>(context + CONTEXT_SCISSOR) };
    let clip = [saved[0].max(left.round() as i32), saved[1].min((left + width).round() as i32), saved[2], saved[3]];
    set_scissor(context, &clip).then_some(saved)
}

/// The walked hook: puts back the scissor [`place_movie`] narrowed, after
/// the movie's display-list walk, and ends the movie's layout.
fn end_walk(_ctx: &mut Context) {
    let current = CURRENT.with(|c| c.get());
    let Some(layout) = current.layout else { return };
    CURRENT.with(|c| c.set(Current { layout: None, ..current }));
    if let (Some(saved), Some(context)) = (layout.scissor, render_context()) {
        set_scissor(context, &saved);
    }
}

/// Sets the scissor of the render `context` through `FUN_140ec7e90`. False
/// when clipping is not available.
fn set_scissor(context: usize, rect: &[i32; 4]) -> bool {
    let set = SET_SCISSOR.load(Ordering::Acquire);
    if set != 0 {
        unsafe { std::mem::transmute::<usize, SetScissorFn>(set)(context, rect) };
    }
    set != 0
}

/// The calling thread's render context, read the way `FUN_14012f2f0` reads
/// it, or `None` before the game has initialised the thread's block.
fn render_context() -> Option<usize> {
    let slots: usize;
    // TEB.ThreadLocalStoragePointer of the calling thread.
    unsafe { std::arch::asm!("mov {}, gs:[0x58]", out(reg) slots, options(nostack, readonly, preserves_flags)) };
    if slots == 0 {
        return None;
    }
    let block = unsafe { read::<usize>(slots) };
    if block == 0 || unsafe { read::<u8>(block + TLS_READY) } == 0 {
        return None;
    }
    let context = unsafe { read::<usize>(block + TLS_RENDER_CONTEXT) };
    (context != 0).then_some(context)
}

/// The aspect hook: reports 16:9 to a centered movie on a wider screen. XMM0
/// holds the aspect about to be stored into `fs_AspectRatio`.
fn clamp_aspect(ctx: &mut Context) {
    let current = CURRENT.with(|c| c.get());
    if current.wrapper == 0 {
        if !ASPECT_OUTSIDE_LOGGED.swap(true, Ordering::Relaxed) {
            log_warn("movie_aspect : a movie asked for the aspect ratio outside a movie load, update or draw; it got the screen's");
        }
        return;
    }
    if !current.centered || !stage_is_16_9(current.wrapper) {
        return;
    }
    let aspect = &mut ctx.xmm0.f32()[0];
    if *aspect > ASPECT_16_9 {
        *aspect = ASPECT_16_9;
    }
}

/// The text scale hook: moves the stage center in XMM2 with the movie.
fn follow_shift(ctx: &mut Context) {
    if let Some(layout) = CURRENT.with(|c| c.get().layout) {
        ctx.xmm2.f32()[0] += 2.0 * layout.shift;
    }
}

/// Where a centered movie's stage lands.
struct Placement {
    /// The narrowed `scaleX`.
    scale_x: f32,
    /// The root translation, in stage units.
    shift: f32,
    /// The region's left edge and width, in viewport pixels.
    left: f32,
    width: f32,
}

/// The centered 16:9 placement of a `stage` (width, height) drawn into a
/// `viewport` of that many pixels, or `None` when the stage is not 16:9 or
/// the viewport is not wider than 16:9.
fn centered(stage: (f32, f32), viewport: (f32, f32)) -> Option<Placement> {
    let (stage_w, stage_h) = stage;
    let (view_w, view_h) = viewport;
    if !(stage_w > 0.0 && stage_h > 0.0 && view_h > 0.0) || !is_16_9(stage_w, stage_h) {
        return None;
    }
    if view_w <= view_h * WIDER_THAN_16_9 {
        return None;
    }
    let width = view_h * ASPECT_16_9;
    let left = (view_w - width) * 0.5;
    let scale_x = width / stage_w;
    Some(Placement { scale_x, shift: left / scale_x, left, width })
}

/// Whether a `width`x`height` stage is 16:9, within 1%.
fn is_16_9(width: f32, height: f32) -> bool {
    (width - height * ASPECT_16_9).abs() <= height * ASPECT_16_9 * 0.01
}

/// Whether the swf loaded into `wrapper`'s player has a 16:9 stage. False
/// while the player or its swf is not loaded.
fn stage_is_16_9(wrapper: usize) -> bool {
    if wrapper == 0 {
        return false;
    }
    let player = unsafe { read::<usize>(wrapper + WRAPPER_PLAYER) };
    if player == 0 {
        return false;
    }
    let swf = unsafe { read::<usize>(player + PLAYER_SWF) };
    if swf == 0 {
        return false;
    }
    let (width, height) = unsafe { (read::<f32>(swf + SWF_STAGE_WIDTH), read::<f32>(swf + SWF_STAGE_HEIGHT)) };
    width > 0.0 && height > 0.0 && is_16_9(width, height)
}

/// Identifies the movie `wrapper` belongs to, reading and logging each movie
/// once.
fn classify(wrapper: usize) -> Current {
    let unknown = Current { wrapper, ..Current::NONE };
    let definition = unsafe { read::<usize>(wrapper + WRAPPER_DEFINITION) };
    if definition < LOWEST_POINTER {
        return unknown;
    }
    let Ok(mut movies) = MOVIES.lock() else { return unknown };
    let centered = match movies.iter().find(|movie| movie.definition == definition) {
        Some(movie) => movie.centered,
        None => {
            let name = read_name(definition + DEFINITION_NAME);
            let centered = name.as_deref().is_some_and(|name| !keeps_full_screen(name));
            match &name {
                Some(name) if centered => log(&format!("movies: '{name}' is centered")),
                Some(name) => log(&format!("movies: '{name}' keeps the full screen")),
                None => log_warn(&format!("movies: definition {definition:#x} has no readable name; it keeps the full screen")),
            }
            movies.push(Movie { definition, name: name.unwrap_or_default(), centered, drawn: false });
            centered
        }
    };
    Current { wrapper, definition, centered, ..Current::NONE }
}

/// The NUL-terminated ASCII name at `address`, if those bytes are readable
/// and form one.
fn read_name(address: usize) -> Option<String> {
    if !readable(address, NAME_CAPACITY) {
        return None;
    }
    let bytes = unsafe { std::slice::from_raw_parts(address as *const u8, NAME_CAPACITY) };
    let name = &bytes[..bytes.iter().position(|&b| b == 0)?];
    (!name.is_empty() && name.iter().all(u8::is_ascii_graphic)).then(|| String::from_utf8_lossy(name).into_owned())
}

/// Whether `name` is on [`FULL_SCREEN_MOVIES`], with or without the `_z`
/// suffix.
fn keeps_full_screen(name: &str) -> bool {
    let base = name.strip_suffix("_z").or_else(|| name.strip_suffix("_Z")).unwrap_or(name);
    FULL_SCREEN_MOVIES.iter().any(|movie| base.eq_ignore_ascii_case(movie))
}

/// Logs a movie's stage, viewport and placement the first time it is drawn.
fn note_first_draw(definition: usize, stage: (f32, f32), viewport: (f32, f32), placement: Option<&Placement>) {
    let Ok(mut movies) = MOVIES.lock() else { return };
    let Some(movie) = movies.iter_mut().find(|movie| movie.definition == definition) else { return };
    if movie.drawn {
        return;
    }
    movie.drawn = true;
    let line = format!(
        "movies: '{}' first drawn: stage {}x{}, viewport {}x{}",
        movie.name, stage.0, stage.1, viewport.0, viewport.1
    );
    match placement {
        Some(p) => log(&format!("{line}, centered at x {:.0}..{:.0}", p.left, p.left + p.width)),
        None if movie.centered && !is_16_9(stage.0, stage.1) => {
            log_warn(&format!("{line}; its stage is not 16:9, so it keeps the full screen"))
        }
        None if movie.centered => log(&format!("{line}; the viewport is not wider than 16:9, nothing to center")),
        None => log(&format!("{line}, full screen")),
    }
}
