//! Writes the C# bindings for `src/ffi.rs`, with the return codes from `src/codes.rs`. The
//! generated file is committed: change it together with those two files.

include!("src/codes.rs");

const OUTPUT: &str = "../../windows/CellGlide.Core/Native/NativeMethods.g.cs";
/// The return codes go right after this line of csbindgen's output.
const ANCHOR: &str = "        const string __DllName = \"cellglide\";\n";

fn main() {
    println!("cargo:rerun-if-changed=src/ffi.rs");
    println!("cargo:rerun-if-changed=src/codes.rs");
    csbindgen::Builder::default()
        .input_extern_file("src/ffi.rs")
        .csharp_dll_name("cellglide")
        .csharp_namespace("CellGlide.Core.Native")
        .csharp_class_name("NativeMethods")
        .csharp_use_function_pointer(true)
        .generate_csharp_file(OUTPUT)
        .expect("csbindgen writes NativeMethods.g.cs");
    let generated = std::fs::read_to_string(OUTPUT).expect("NativeMethods.g.cs is readable");
    let mut codes = String::from("\n        // Return codes (crates/cg-ffi/src/codes.rs).\n");
    for (name, value) in CODES {
        codes.push_str(&format!("        internal const int {name} = {value};\n"));
    }
    let with_codes = generated.replacen(ANCHOR, &format!("{ANCHOR}{codes}"), 1);
    assert_ne!(
        with_codes, generated,
        "csbindgen's output changed: no __DllName line"
    );
    std::fs::write(OUTPUT, with_codes).expect("NativeMethods.g.cs is writable");
}
