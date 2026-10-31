//! Writes the C# bindings for `src/ffi.rs`. The generated file is committed: change it together
//! with `src/ffi.rs`.

fn main() {
    println!("cargo:rerun-if-changed=src/ffi.rs");
    csbindgen::Builder::default()
        .input_extern_file("src/ffi.rs")
        .csharp_dll_name("cellglide")
        .csharp_namespace("CellGlide.Core.Native")
        .csharp_class_name("NativeMethods")
        .csharp_use_function_pointer(true)
        .generate_csharp_file("../../windows/CellGlide.Core/Native/NativeMethods.g.cs")
        .expect("csbindgen writes NativeMethods.g.cs");
}
