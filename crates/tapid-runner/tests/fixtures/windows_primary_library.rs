// A real PE import forces the normal loader to resolve a secondary application DLL.
#[link(name = "tapid-secondary", kind = "raw-dylib")]
unsafe extern "C" {
    fn tapid_secondary_value() -> u32;
}

#[unsafe(no_mangle)]
pub extern "C" fn napi_register_module_v1() -> u32 {
    // SAFETY: this import is provided by the paired, locally compiled Rust fixture.
    unsafe { tapid_secondary_value() }
}
