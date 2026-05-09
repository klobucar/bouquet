// On macOS, building a PyO3 cdylib with `extension-module` produces a
// dylib whose Python symbols are resolved at runtime by the loading
// interpreter. The default macOS linker rejects unresolved symbols at link
// time; this `build.rs` flips that to `dynamic_lookup` so plain
// `cargo build --workspace` works in addition to `maturin develop`.
//
// On Linux the cdylib resolves Python symbols by being loaded into a process
// that already has libpython linked, so no linker flag is needed.

fn main() {
    pyo3_build_config::add_extension_module_link_args();
}
