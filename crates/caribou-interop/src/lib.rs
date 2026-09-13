//! Nothing to link but a helper: the crate exists for its tests, which
//! host Ash and WrenLift in one process and pass values between them
//! through the bridge.

use std::fs::File;
use std::io::{Read, Seek, Write};
use std::os::fd::AsRawFd;

/// What `f` writes to the process's stdout: a Haxe program prints through
/// the runtime's own `Sys.println`.
pub fn captured<F: FnOnce()>(f: F) -> String {
    let path = std::env::temp_dir().join(format!(
        "caribou-captured-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let mut file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .expect("a capture file");
    std::io::stdout().flush().unwrap();
    let saved = unsafe { libc::dup(1) };
    assert!(unsafe { libc::dup2(file.as_raw_fd(), 1) } >= 0);
    f();
    std::io::stdout().flush().unwrap();
    unsafe { libc::fflush(std::ptr::null_mut()) };
    assert!(unsafe { libc::dup2(saved, 1) } >= 0);
    unsafe { libc::close(saved) };
    let mut text = String::new();
    file.rewind().unwrap();
    file.read_to_string(&mut text).unwrap();
    drop(file);
    let _ = std::fs::remove_file(path);
    text
}
