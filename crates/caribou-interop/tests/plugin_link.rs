//! The math plugin's members, exported under their link symbols: what
//! an AOT call reaches them by. In a process of its own, since describing
//! a plugin opens it and installs a host of its own.

use std::path::PathBuf;

/// Where the build script put the test plugins.
fn plugin_dir() -> PathBuf {
    PathBuf::from(env!("OUT_DIR")).join("plugins/debug")
}

/// Every member the plugin describes is exported under the symbol the
/// core's link rule gives it, which is what an AOT call is linked to.
#[test]
fn every_member_is_exported_under_its_link_symbol() {
    let dir = plugin_dir();
    let library = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.extension().is_some_and(|e| e == std::env::consts::DLL_EXTENSION))
        .expect("the math plugin");
    let modules = caribou_plugin::describe(&library).expect("the plugin describes itself");
    let opened = unsafe { libloading::Library::new(&library) }.unwrap();
    let mut checked = 0;
    for module in &modules {
        for class in &module.classes {
            for member in &class.members {
                let symbol = caribou::link::symbol(
                    &module.lang,
                    &module.module,
                    &class.name,
                    member.kind.into(),
                    &member.name,
                    member.params.len(),
                );
                let found = unsafe { opened.get::<unsafe extern "C" fn()>(symbol.as_bytes()) };
                assert!(found.is_ok(), "{symbol} for {}.{}", class.name, member.name);
                checked += 1;
            }
        }
    }
    assert!(checked > 30, "{checked} members checked");
}
