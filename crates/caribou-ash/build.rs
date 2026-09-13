fn main() {
    // The setjmp frame under every call into HashLink code; see trap.c.
    cc::Build::new()
        .file("src/trap.c")
        .warnings(true)
        .compile("caribou_ash_trap");
    println!("cargo:rerun-if-changed=src/trap.c");
}
