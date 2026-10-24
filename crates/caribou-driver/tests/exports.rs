//! A binary with ash_std in it exports none of ash_std's own functions, so
//! an HDLL binds them to the runtime image the program installs into.

#![cfg(unix)]

use std::ffi::CString;

fn exported(name: &str) -> bool {
    let name = CString::new(name).unwrap();
    !unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) }.is_null()
}

#[test]
fn the_runtime_is_not_exported() {
    // Keeps the linked copy in the binary, as `caribou` has it.
    let _ = caribou_ash::install;
    for name in [
        "hlp_future_resolve",
        "hlp_future_await",
        "hl_alloc_obj",
        "ash_host_wait",
    ] {
        assert!(!exported(name), "{name} is exported");
    }
}
