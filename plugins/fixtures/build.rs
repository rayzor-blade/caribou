#![feature(path_absolute_method)]

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let plugins = manifest_dir.join("../");
    println!("cargo:rerun-if-changed={}", plugins.display());
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("../../crates/caribou_abi/src").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("../../crates/caribou_abi_derive/src").display()
    );
    let target_dir = out_dir.join("plugins");
    // gen plugin package list
    let packages = std::fs::read_dir(&plugins)
        .expect("the plugins dir exists")
        .filter_map(|entry| {
            let entry = entry.expect("the plugin dir entry reads");
            let path = entry.path();
            if path.is_dir() {
                // use cargo package name as plugin name, which is not the same as the directory name
                let manifest_path = path.join("Cargo.toml");
                let manifest = std::fs::read_to_string(&manifest_path).unwrap_or_else(|_| {
                    panic!("the plugin manifest {} reads", manifest_path.display())
                });
                let name = manifest
                    .lines()
                    .find_map(|line| {
                        if line.starts_with("name") {
                            Some(line.split('=').nth(1)?.trim().trim_matches('"').to_string())
                        } else {
                            None
                        }
                    })
                    .unwrap_or_else(|| {
                        panic!("the plugin manifest {} has a name", manifest_path.display())
                    });
                if name == "caribou-plugin-fixtures" {
                    return None;
                }
                Some(name.to_string())
            } else {
                None
            }
        })
        .collect::<Vec<_>>();

    // build all the plugins in the workspace, so that the test program can load them
    let mut args = packages
        .iter()
        .flat_map(|name| ["-p", name])
        .collect::<Vec<_>>();

    args.push("--target-dir");
    args.push(target_dir.to_str().unwrap());
    let status = Command::new(std::env::var("CARGO").unwrap())
        .args(["build"])
        .args(args)
        .current_dir(&manifest_dir)
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTFLAGS")
        .status()
        .expect("cargo runs");
    assert!(status.success(), "the test plugins build");

    // copy the built plugin dylib to the pligin directory of the fixtures, so that the test program can load them
    let target_dir = target_dir.join("debug");
    // get the dylibs of the built plugins
    let dylibs = std::fs::read_dir(&target_dir)
        .expect("the target dir exists")
        .filter_map(|entry| {
            let entry = entry.expect("the target dir entry reads");
            let path = entry.path();
            if path.is_file()
                && path
                    .extension()
                    .is_some_and(|ext| ext == std::env::consts::DLL_EXTENSION)
            {
                Some(path)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();

    // use haxelib dev CLI to install the local caribou haxelib path, so that the test program can use it
    let status = Command::new("haxelib")
        .args([
            "dev",
            "caribou",
            manifest_dir.join("../../haxe").to_str().unwrap(),
        ])
        .status()
        .expect("haxelib runs");
    assert!(status.success(), "the caribou haxelib installs");

    // build the hashlink binary of each fixture, so that the test program can run them
    let fixtures = manifest_dir;
    // walk into fixtures dir and find all haxe hxml projects, and build them with hashlink target
    let entries = std::fs::read_dir(&fixtures).expect("the fixtures dir exists");
    for entry in entries {
        let entry = entry.expect("the fixture dir entry reads");
        let path = entry.path();
        if path.is_dir() {
            println!("cargo:rerun-if-changed={}", path.display());
            // check if entry name matches plugin package suffix, and copy its dylib to the fixture's plugin directory
            let found_plugin = dylibs.iter().find(|name| {
                println!(
                    "checking if plugin dylib {} matches fixture {}",
                    name.display(),
                    path.display()
                );
                path.file_name().unwrap_or_default().to_str().unwrap()
                    == name
                        .file_name()
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .replace(&format!(".{}", std::env::consts::DLL_EXTENSION), "")
                        .split("_")
                        .collect::<Vec<&str>>()[1]
            });
            println!(
                "found plugin dylib for fixture {}: {:?}",
                path.display(),
                found_plugin
            );
            if let Some(plugin_dylib) = found_plugin {
                let fixture_plugin_dir = path.join("plugins");
                std::fs::create_dir_all(&fixture_plugin_dir)
                    .expect("the fixture plugin dir exists");
                let fixture_plugin_dylib =
                    fixture_plugin_dir.join(plugin_dylib.file_name().unwrap());
                println!(
                    "copying plugin dylib {} to fixture {}",
                    plugin_dylib.display(),
                    fixture_plugin_dylib.display()
                );
                std::fs::copy(&plugin_dylib, &fixture_plugin_dylib).unwrap_or_else(|_| {
                    panic!(
                        "the plugin dylib {} copies to {}",
                        plugin_dylib.display(),
                        fixture_plugin_dylib.display()
                    )
                });
            }

            // get any hxml file in the directory
            let hxml_path = std::fs::read_dir(&path)
                .expect("the fixture dir entry reads")
                .filter_map(|entry| {
                    let entry = entry.expect("the fixture dir entry reads");
                    let path = entry.path();
                    if path.is_file() && path.extension().map(|ext| ext == "hxml").unwrap_or(false)
                    {
                        Some(path)
                    } else {
                        None
                    }
                })
                .next();
            if let Some(hxml_path) = hxml_path {
                if hxml_path.exists() {
                    println!(
                        "{:?}",
                        [
                            "-C",
                            &path.to_str().unwrap(),
                            &hxml_path.file_name().unwrap().to_str().unwrap()
                        ]
                    );
                    let status = Command::new("haxe")
                        .args([
                            "-C",
                            &path.to_str().unwrap(),
                            &hxml_path.file_name().unwrap().to_str().unwrap(),
                        ])
                        .status()
                        .expect("haxe runs");
                    assert!(status.success(), "the fixture {} builds", path.display());
                }
            }
        }
    }
}
