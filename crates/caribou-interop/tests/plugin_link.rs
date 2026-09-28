//! The math plugin's members, exported under their link symbols: what
//! an AOT call reaches them by. In a process of its own, since describing
//! a plugin opens it and installs a host of its own.

use std::path::PathBuf;

/// Where the build script put the test plugins.
fn plugin_dir() -> PathBuf {
    PathBuf::from(env!("CARIBOU_TEST_PLUGINS")).join("plugins/debug")
}

/// Every member the plugin describes is exported under the symbol the
/// core's link rule gives it, which is what an AOT call is linked to.
#[test]
fn every_member_is_exported_under_its_link_symbol() {
    let dir = plugin_dir();
    let library = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| {
            p.extension()
                .is_some_and(|e| e == std::env::consts::DLL_EXTENSION)
        })
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

/// The links the driver builds a program's call sites from name the same
/// symbols, each one the library exports.
#[test]
fn the_plugins_links_name_what_it_exports() {
    let dir = plugin_dir();
    let library = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| {
            p.extension()
                .is_some_and(|e| e == std::env::consts::DLL_EXTENSION)
        })
        .expect("the math plugin");
    let links = caribou_plugin::links(&library).expect("the plugin's links");
    let opened = unsafe { libloading::Library::new(&library) }.unwrap();
    for link in &links {
        let found = unsafe { opened.get::<unsafe extern "C" fn()>(link.symbol.as_bytes()) };
        assert!(
            found.is_ok(),
            "{} for {}.{}",
            link.symbol,
            link.class,
            link.name
        );
    }
    let hypot = links.iter().find(|l| l.name == "hypot").expect("hypot");
    assert_eq!(hypot.symbol, "caribou_4math_4Math_4Math_t5hypot_2");
    assert_eq!(
        hypot.params,
        [caribou_abi::TypeTag::F64, caribou_abi::TypeTag::F64]
    );
    assert_eq!(hypot.ret, caribou_abi::TypeTag::F64);
}
