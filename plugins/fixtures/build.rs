//! Build the native plugins and Haxe programs used by src/bin/{gpu,window}.rs.
use std::{env, fs, path::Path, path::PathBuf, process::Command};

// Fixture directory and the plugins it loads. Keep these explicit: a shared
// library filename is not a reliable way to infer its fixture, and one
// fixture may deliberately compose several plugins.
struct Fixture {
    name: &'static str,
    plugins: &'static [(&'static str, &'static str)],
}

const FIXTURES: &[Fixture] = &[
    Fixture {
        name: "gpu",
        plugins: &[("caribou-gpu", "caribou_gpu")],
    },
    Fixture {
        name: "window",
        plugins: &[("caribou-window", "caribou_window")],
    },
    Fixture {
        name: "window_gpu",
        plugins: &[
            ("caribou-gpu", "caribou_gpu"),
            ("caribou-window", "caribou_window"),
        ],
    },
];

fn run(command: &mut Command, description: &str) {
    let status = command
        .status()
        .unwrap_or_else(|error| panic!("{description}: {error}"));
    assert!(status.success(), "{description}: {status}");
}

fn watch(path: impl AsRef<Path>) {
    println!("cargo:rerun-if-changed={}", path.as_ref().display());
}

fn main() {
    let fixtures = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let root = fixtures.join("../..").canonicalize().unwrap();
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let target = out.join("plugins");
    let haxe = root.join("haxe");

    watch(root.join("Cargo.toml"));
    watch(root.join("Cargo.lock"));
    watch(&haxe);
    // The descriptor command uses the core/adapters, and plugins use the ABI
    // and generator. Watch their sources, not directories we write below.
    for entry in fs::read_dir(root.join("crates")).unwrap() {
        let path = entry.unwrap().path();
        if path.join("Cargo.toml").is_file() {
            watch(path.join("Cargo.toml"));
            watch(path.join("src"));
            if path.join("build.rs").is_file() {
                watch(path.join("build.rs"));
            }
        }
    }
    for fixture in FIXTURES {
        for &(package, _) in fixture.plugins {
            let plugin = package.strip_prefix("caribou-").unwrap_or(package);
            watch(root.join("plugins").join(format!("cb_{plugin}")));
        }
        watch(fixtures.join(fixture.name).join("src"));
        watch(
            fixtures
                .join(fixture.name)
                .join(format!("{}.hxml", fixture.name)),
        );
    }

    // Cargo holds the outer target directory's lock. Build both plugins and
    // the descriptor command in our own target directory to avoid re-entry
    // deadlocks and dependence on a stale/preinstalled caribou executable.
    let mut build = Command::new(env::var_os("CARGO").unwrap());
    build.args(["build", "--locked", "-p", "caribou-driver"]);
    let mut packages = std::collections::BTreeSet::new();
    for fixture in FIXTURES {
        for &(package, _) in fixture.plugins {
            packages.insert(package);
        }
    }
    for package in packages {
        build.args(["-p", package]);
    }
    run(
        build
            .arg("--target-dir")
            .arg(&target)
            .current_dir(&root)
            .env_remove("CARGO_ENCODED_RUSTFLAGS")
            .env_remove("RUSTFLAGS"),
        "building the fixture plugins and descriptor command",
    );

    let libraries = target.join("debug");
    let mut paths = vec![libraries.clone()];
    paths.extend(env::split_paths(&env::var_os("PATH").unwrap_or_default()));
    let path = env::join_paths(paths).expect("the fixture command search path is valid");

    // Haxe's -lib caribou resolves through a build-local repository. Never
    // rewrite the user's global haxelib dev registration from a Cargo build.
    let haxelib = out.join("haxelib");
    fs::create_dir_all(&haxelib).expect("the fixture haxelib repository is created");
    run(
        Command::new("haxelib")
            .env("HAXELIB_PATH", &haxelib)
            .args(["dev", "caribou"])
            .arg(&haxe),
        "registering Caribou in the fixture-local haxelib repository",
    );

    for fixture in FIXTURES {
        let project = fixtures.join(fixture.name);
        let plugins = project.join("plugins");
        fs::create_dir_all(&plugins).expect("the fixture plugin directory is created");
        for &(_, library) in fixture.plugins {
            let filename = format!(
                "{}{library}.{}",
                env::consts::DLL_PREFIX,
                env::consts::DLL_EXTENSION
            );
            fs::copy(libraries.join(&filename), plugins.join(&filename))
                .unwrap_or_else(|error| panic!("copying {filename} to {}: {error}", fixture.name));
        }
        run(
            Command::new("haxe")
                .current_dir(&project)
                .env("HAXELIB_PATH", &haxelib)
                .env("PATH", &path)
                .arg(format!("{}.hxml", fixture.name)),
            &format!("compiling the {} Haxe fixture", fixture.name),
        );
    }
}
