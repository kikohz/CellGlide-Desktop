// The C interface's return codes. `ffi.rs` and `build.rs` both include this file, and
// `build.rs` writes `CODES` into NativeMethods.g.cs (csbindgen can't write a negative
// constant), so the C# values are these values.

/// Done.
pub const CG_OK: i32 = 0;
/// cg_init while already running (or, from the callback, while stopping).
pub const CG_ALREADY: i32 = 1;
pub const CG_BAD_ARGUMENT: i32 = 2;
/// Something panicked inside the call; the DLL is still usable.
pub const CG_PANIC: i32 = -1;

/// Every code by name, in the order the C# lists them.
#[allow(dead_code)]
const CODES: [(&str, i32); 4] = [
    ("CG_OK", CG_OK),
    ("CG_ALREADY", CG_ALREADY),
    ("CG_BAD_ARGUMENT", CG_BAD_ARGUMENT),
    ("CG_PANIC", CG_PANIC),
];
