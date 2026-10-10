use std::ffi::c_void;
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use grapnel::{Context, InlineHook, MidHook};
use memchr::memmem;
use windows::Win32::Foundation::{HMODULE, SYSTEMTIME};
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::Memory::{
    MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_EXECUTE_READWRITE, PAGE_GUARD, PAGE_NOACCESS, PAGE_PROTECTION_FLAGS,
    VirtualProtect, VirtualQuery,
};
use windows::Win32::System::SystemInformation::GetLocalTime;

static LOG_FILE: OnceLock<Mutex<File>> = OnceLock::new();

/// Opens (truncating) the log file at `path` for [`log`] to write into.
pub fn init_log_file(path: impl AsRef<Path>) {
    if let Ok(file) = File::create(path) {
        let _ = LOG_FILE.set(Mutex::new(file));
    }
}

/// A severity tag written into each log line.
#[derive(Clone, Copy)]
pub enum LogLevel {
    Info,
    Warning,
    Error,
    Debug,
}

impl LogLevel {
    fn tag(self) -> &'static str {
        match self {
            LogLevel::Info => "INFO",
            LogLevel::Warning => "WARNING",
            LogLevel::Error => "ERROR",
            LogLevel::Debug => "DEBUG",
        }
    }
}

/// Formats a broken-down time: `YYYY-MM-DDTHH:MM:SS.mmm`
fn format_systemtime(t: &SYSTEMTIME) -> String {
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
    )
}

/// Appends a timestamped, level-tagged line to the file [`init_log_file`]
/// opened, if one has been opened yet.
fn log_at(level: LogLevel, message: &str) {
    let Some(file) = LOG_FILE.get() else { return };
    let Ok(mut file) = file.lock() else { return };
    let t = unsafe { GetLocalTime() };
    let _ = writeln!(file, "[{}] [{}] {message}", format_systemtime(&t), level.tag());
}

/// Logs at [`LogLevel::Info`] -- the default for a normal-operation line
/// (a fix applied, a signature matched, a config value in effect).
pub fn log(message: &str) {
    log_at(LogLevel::Info, message);
}

/// Logs at [`LogLevel::Warning`] -- a fallback path was taken but the mod
/// still functions (e.g. a malformed config value, falling back to a default).
pub fn log_warn(message: &str) {
    log_at(LogLevel::Warning, message);
}

/// Logs at [`LogLevel::Error`] -- a fix did not apply (signature not found,
/// hook install failed) or the process panicked.
pub fn log_error(message: &str) {
    log_at(LogLevel::Error, message);
}

/// Logs at [`LogLevel::Debug`] -- available for detail that would otherwise
/// clutter normal operation.
#[allow(dead_code)]
pub fn log_debug(message: &str) {
    log_at(LogLevel::Debug, message);
}

/// A scanned/patched module: its base address plus everything worth logging
/// about the file backing it.
pub struct ModuleInfo {
    /// Base virtual address.
    pub address: HMODULE,
    /// Name of the module.
    pub name: String,
    /// Path of the filesystem to the exe.
    pub path: String,
    /// On-disk file size in bytes.
    pub size: u64,
}

impl ModuleInfo {
    /// Builds a `ModuleInfo` from a module handle, resolving its file name,
    /// size, and modified time for logging.
    pub fn new(address: HMODULE) -> Self {
        let mut buf = [0u16; 260];
        let len = unsafe { GetModuleFileNameW(Some(address), &mut buf) } as usize;
        let path = String::from_utf16_lossy(&buf[..len]);
        let name = Path::new(&path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.clone());
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        Self { address, name, path, size }
    }

    /// `addr` as an offset from the module base, for log lines.
    pub fn rva(&self, addr: usize) -> usize {
        addr.wrapping_sub(self.address.0 as usize)
    }
}

/// A hook site keyed off a pattern scan.
pub struct SignatureHook {
    /// Name identifier for the hook.
    pub tag: &'static str,
    /// IDA-style byte string describing where the hook should be placed.
    pub signature: &'static str,
    /// Byte offset from the pattern match to the hook point.
    pub offset: usize,
}

/// Converts memory bytes into an IDA-style byte string, e.g. `[0x40, 0x63]` -> `"40 63"`.
#[allow(dead_code)]
pub fn bytes_to_string(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parses an IDA-style byte pattern (e.g. `"48 8B 05 ?? ?? ?? ?? 48 85 C0"`) into a
/// sequence of optional bytes, where `None` is a wildcard.
fn parse_pattern(pattern: &str) -> Vec<Option<u8>> {
    pattern
        .split_whitespace()
        .map(|byte| {
            if byte == "??" {
                None
            } else {
                Some(u8::from_str_radix(byte, 16).expect("invalid byte in AOB pattern"))
            }
        })
        .collect()
}

/// Overwrites code or read-only data at `address` with `bytes`, temporarily
/// marking the region writable, then restoring its original protection.
pub fn write_code(address: usize, bytes: &[u8]) {
    unsafe {
        let mut old_protect = PAGE_PROTECTION_FLAGS(0);
        let _ = VirtualProtect(
            address as *const c_void,
            bytes.len(),
            PAGE_EXECUTE_READWRITE,
            &mut old_protect,
        );
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), address as *mut u8, bytes.len());
        let mut discard = PAGE_PROTECTION_FLAGS(0);
        let _ = VirtualProtect(address as *const c_void, bytes.len(), old_protect, &mut discard);
    }
}

/// Overwrites memory at `address` with `pattern` (an IDA-style byte string,
/// e.g. `"DE AD BE EF"`), temporarily marking the region writable, then
/// restoring its original protection.
#[allow(dead_code)]
pub fn patch(address: usize, pattern: &str) {
    let bytes: Vec<u8> = pattern
        .split_whitespace()
        .map(|b| u8::from_str_radix(b, 16).expect("invalid byte in patch pattern"))
        .collect();
    write_code(address, &bytes);
}

/// Reads a `T` at `address`, which need not be aligned.
///
/// # Safety
///
/// `address` must point at `size_of::<T>()` readable bytes.
pub unsafe fn read<T: Copy>(address: usize) -> T {
    unsafe { std::ptr::read_unaligned(address as *const T) }
}

/// Writes `value` at `address`, which need not be aligned.
///
/// # Safety
///
/// `address` must point at `size_of::<T>()` writable bytes.
pub unsafe fn write<T: Copy>(address: usize, value: T) {
    unsafe { std::ptr::write_unaligned(address as *mut T, value) }
}

/// Whether `len` bytes from `address` lie in one committed, readable
/// region. Asks the memory manager rather than touching the bytes, so a bad
/// pointer costs a failed check instead of an access violation.
pub fn readable(address: usize, len: usize) -> bool {
    let mut info = MEMORY_BASIC_INFORMATION::default();
    let size = unsafe {
        VirtualQuery(Some(address as *const c_void), &mut info, std::mem::size_of::<MEMORY_BASIC_INFORMATION>())
    };
    if size == 0 || info.State != MEM_COMMIT || info.Protect.0 & (PAGE_NOACCESS.0 | PAGE_GUARD.0) != 0 {
        return false;
    }
    let end = (info.BaseAddress as usize).saturating_add(info.RegionSize);
    address.checked_add(len).is_some_and(|last| last <= end)
}

/// Resolves a RIP-relative operand: the signed 32-bit displacement stored at
/// `instr + disp_offset`, measured from the end of the `instr_len`-byte
/// instruction that starts at `instr`.
///
/// `CALL rel32` (`E8`, displacement at +1, length 5) yields the called
/// function; `MOVSS [rip+disp32],XMM1` (`F3 0F 11 0D`, displacement at +4,
/// length 8) yields the global it writes.
pub fn rip_target(instr: usize, disp_offset: usize, instr_len: usize) -> usize {
    let disp = unsafe { read::<i32>(instr + disp_offset) };
    (instr + instr_len).wrapping_add_signed(disp as isize)
}

/// Returns the address and size of `module`'s `.text` section, read from its
/// PE headers. Every signature in this crate targets code, and scanning only
/// the code section keeps scans fast and away from pages a protector may
/// leave unreadable.
fn code_section(module: HMODULE) -> Option<(*const u8, usize)> {
    const E_LFANEW: usize = 0x3c;
    const PE_SIGNATURE: u32 = 0x0000_4550; // "PE\0\0"
    const NUMBER_OF_SECTIONS: usize = 4 + 2;
    const SIZE_OF_OPTIONAL_HEADER: usize = 4 + 16;
    const OPTIONAL_HEADER: usize = 4 + 20;
    const SECTION_HEADER_SIZE: usize = 40;
    const VIRTUAL_SIZE: usize = 8;
    const VIRTUAL_ADDRESS: usize = 12;

    let base = module.0 as usize;
    if base == 0 {
        return None;
    }
    unsafe {
        let nt = base + read::<u32>(base + E_LFANEW) as usize;
        if read::<u32>(nt) != PE_SIGNATURE {
            return None;
        }
        let sections = read::<u16>(nt + NUMBER_OF_SECTIONS) as usize;
        let optional_size = read::<u16>(nt + SIZE_OF_OPTIONAL_HEADER) as usize;
        let first = nt + OPTIONAL_HEADER + optional_size;
        (0..sections).map(|i| first + i * SECTION_HEADER_SIZE).find_map(|header| {
            let name = std::slice::from_raw_parts(header as *const u8, 8);
            (name == b".text\0\0\0").then(|| {
                let address = base + read::<u32>(header + VIRTUAL_ADDRESS) as usize;
                (address as *const u8, read::<u32>(header + VIRTUAL_SIZE) as usize)
            })
        })
    }
}

/// Whether `data` starts with `pattern` (`None` entries are wildcards).
fn matches_at(data: &[u8], pattern: &[Option<u8>]) -> bool {
    data.len() >= pattern.len()
        && data.iter().zip(pattern).all(|(byte, pat)| pat.is_none_or(|p| *byte == p))
}

/// The longest run of literal bytes in `pattern`, as (offset, bytes). A
/// substring search for it narrows the candidates before the full,
/// wildcard-aware comparison runs.
fn longest_literal_run(pattern: &[Option<u8>]) -> (usize, Vec<u8>) {
    let mut best = (0, Vec::new());
    let mut start = 0;
    while start < pattern.len() {
        let run: Vec<u8> = pattern[start..].iter().map_while(|b| *b).collect();
        if run.len() > best.1.len() {
            best = (start, run.clone());
        }
        start += run.len() + 1;
    }
    best
}

/// Every offset in `data` where `pattern` matches, in ascending order.
///
/// The literal run is searched for with overlapping matches: a run such as
/// `6A 00 6A 00` occurs inside `6A 00 6A 00 6A 00` at two offsets, and the
/// pattern can start at either.
fn aob_scan_all(data: &[u8], pattern: &[Option<u8>]) -> Vec<usize> {
    let (run_offset, run) = longest_literal_run(pattern);
    if run.is_empty() {
        return Vec::new();
    }
    let finder = memmem::Finder::new(&run);
    let mut hits = Vec::new();
    let mut from = 0;
    while let Some(found) = finder.find(&data[from..]) {
        let hit = from + found;
        if let Some(start) = hit.checked_sub(run_offset)
            && matches_at(&data[start..], pattern)
        {
            hits.push(start);
        }
        from = hit + 1;
    }
    hits
}

/// Scans `module`'s code for `signature`, returning the absolute address of the
/// first match, if any.
pub fn pattern_scan(module: HMODULE, signature: &str) -> Option<usize> {
    pattern_scan_all(module, signature).first().copied()
}

/// Scans `module`'s code for every occurrence of `signature`, returning all
/// matching absolute addresses.
pub fn pattern_scan_all(module: HMODULE, signature: &str) -> Vec<usize> {
    let Some((base, size)) = code_section(module) else {
        return Vec::new();
    };
    let data = unsafe { std::slice::from_raw_parts(base, size) };
    let pattern = parse_pattern(signature);
    aob_scan_all(data, &pattern).into_iter().map(|offset| base as usize + offset).collect()
}

/// Scans `module` for `sh.signature`, returning the match plus `sh.offset`,
/// or `None`, logged, if there is no match. Lets a fix resolve every site it
/// needs before it patches any of them.
pub fn find(module: &ModuleInfo, sh: &SignatureHook) -> Option<usize> {
    match pattern_scan(module.address, sh.signature) {
        Some(hit) => {
            log(&format!("{} : Found '{}' @ {}+{:x}", sh.tag, sh.signature, module.name, module.rva(hit)));
            Some(hit + sh.offset)
        }
        None => {
            log_error(&format!("{} : Did not find '{}'", sh.tag, sh.signature));
            None
        }
    }
}

/// Installs a mid-function hook permanently at a fixed `offset` from
/// `module`'s base, bypassing signature scanning. A no-op if `enable` is
/// false.
///
/// Useful for investigations that need to probe an address before it is worth
/// a signature at all. No shipped fix should call this.
#[allow(dead_code)]
pub fn inject_hook_at<F>(
    enable: bool,
    module: &ModuleInfo,
    tag: &str,
    offset: usize,
    callback: F,
) -> bool
where
    F: FnMut(&mut Context) + Send + 'static,
{
    if !enable {
        return false;
    }
    let hook_addr = (module.address.0 as usize + offset) as *mut u8;
    match MidHook::install(hook_addr, callback) {
        Ok(_hook) => {
            log(&format!("{tag} : Hooked @ {}+{offset:x}", module.name));
            true
        }
        Err(e) => {
            log_error(&format!("{tag} : Failed to install hook @ {}+{offset:x}: {e}", module.name));
            false
        }
    }
}

/// Scans `module` for `sh.signature` and, if found, installs a mid-function
/// hook permanently for the duration of the main process at the address plus
/// `sh.offset` that jumps to the `callback`. A no-op if `enable` is false.
#[allow(dead_code)]
pub fn inject_hook<F>(enable: bool, module: &ModuleInfo, sh: &SignatureHook, callback: F) -> bool
where
    F: FnMut(&mut Context) + Send + 'static,
{
    if !enable {
        return false;
    }
    match find(module, sh) {
        Some(addr) => hook_at(module, sh.tag, addr, callback),
        None => false,
    }
}

/// Installs a mid-function hook permanently at `addr`, an address inside
/// `module` found by an earlier scan. Several hooks on the bytes of one
/// signature need this: installing the first rewrites those bytes, so a
/// second scan for the same signature finds nothing.
pub fn hook_at<F>(module: &ModuleInfo, tag: &str, addr: usize, callback: F) -> bool
where
    F: FnMut(&mut Context) + Send + 'static,
{
    let rel_addr = module.rva(addr);
    match MidHook::install(addr as *mut u8, callback) {
        Ok(_hook) => {
            log(&format!("{tag} : Hooked @ {}+{rel_addr:x}", module.name));
            true
        }
        Err(e) => {
            log_error(&format!("{tag} : Failed to install hook @ {}+{rel_addr:x}: {e}", module.name));
            false
        }
    }
}

/// Detours the function at `addr`, an address inside `module` found by an
/// earlier scan, to `detour` for the rest of the process. Returns the address
/// that runs the original function (grapnel's trampoline: the relocated
/// prologue, then a jump back into the body), or `None`, logged, if the
/// detour could not be installed.
pub fn detour_at(module: &ModuleInfo, tag: &str, addr: usize, detour: *const u8) -> Option<usize> {
    let rel_addr = module.rva(addr);
    match InlineHook::install(addr as *mut u8, detour) {
        Ok(hook) => {
            log(&format!("{tag} : Detoured @ {}+{rel_addr:x}", module.name));
            Some(hook.original::<usize>())
        }
        Err(e) => {
            log_error(&format!("{tag} : Failed to install detour @ {}+{rel_addr:x}: {e}", module.name));
            None
        }
    }
}
