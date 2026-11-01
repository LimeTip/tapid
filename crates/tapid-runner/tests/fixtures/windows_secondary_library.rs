// Isolated secondary input for the ambient loader authority controls.
#[unsafe(no_mangle)]
pub extern "C" fn tapid_secondary_value() -> u32 {
    if cfg!(alternate_secondary) { 274 } else { 273 }
}
