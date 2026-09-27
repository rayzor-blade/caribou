//! A project built ahead of time for wasm, its plugin crate and its Wren
//! modules linked into the program: the program calls their members
//! directly, and `caribou run` runs the module. Needs the `llvm` feature,
//! which is what builds ahead of time.
#![cfg(feature = "llvm")]

use std::path::{Path, PathBuf};
use std::process::Command;

const MAIN: &str = r#"
import math.Math;
import math.Vec2;

class Main {
  static function main() {
    Sys.println(Math.hypot(3, 4));
    Sys.println(Math.twice(21));
    Sys.println(Math.is_even(6));
    Sys.println(Math.shout("héllo"));
    Sys.println(Math.width("héllo"));
    Sys.println(Math.quotient(7, 2));
    try {
      Math.quotient(1, 0);
      Sys.println("no error");
    } catch (e:Dynamic) {
      Sys.println("caught: " + e);
    }
    var v = new Vec2(3, 4);
    Sys.println(v.len());
    v.scale(2);
    Sys.println(v.len());
    Sys.println(v.unit().len());
    Sys.println(v.dot(new Vec2(1, 0)));
    // Dropped faces release their payloads through the plugin.
    for (i in 0...200000) new Vec2(i, i);
    Sys.println(Vec2.live() < 200000);
  }
}
"#;

fn caribou(dir: &Path, haxelib: &Path, args: &[&str]) -> String {
    caribou_with_env(dir, haxelib, args, &[])
}

fn caribou_with_env(dir: &Path, haxelib: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_caribou"));
    command
        .args(args)
        .current_dir(dir)
        .env("HAXELIB_PATH", haxelib)
        .env_remove("ASH_GC_STRESS");
    for &(key, value) in env {
        command.env(key, value);
    }
    let out = command.output().expect("caribou runs");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "caribou {args:?}: {}{text}",
        String::from_utf8_lossy(&out.stderr)
    );
    text
}

#[test]
fn a_plugin_crate_links_into_a_wasm_program() {
    plugin_program("caribou-linked", "");
}

#[test]
fn a_plugin_crate_loads_beside_a_wasm_program_as_a_side_module() {
    let dir = plugin_program("caribou-side-module", ", link = \"side\"");
    assert!(dir.join("target/math.wasm").is_file(), "the side module beside the program");
}

/// Build and run the math plugin's program with the plugin taken by `link`,
/// a project file's plugin options, in `name` under the test target.
fn plugin_program(name: &str, link: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    // Kept between runs: the program's runtime builds from caribou's source.
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/Main.hx"), MAIN).unwrap();
    let math = root.join("crates/caribou-interop/plugins/math");
    std::fs::write(
        dir.join("plug.cbproj"),
        format!(
            "[project]\nname = \"plug\"\nentry = \"haxe:Main\"\nlanguages = [\"haxe\"]\n\n\
             [plugins]\nmath = {{ path = {:?}{link} }}\n",
            math.display().to_string()
        ),
    )
    .unwrap();
    let haxelib = dir.join("haxelib");
    std::fs::create_dir_all(&haxelib).unwrap();
    let status = Command::new("haxelib")
        .env("HAXELIB_PATH", &haxelib)
        .args(["dev", "caribou"])
        .arg(root.join("haxe"))
        .status()
        .expect("haxelib runs");
    assert!(status.success());

    let built = caribou(&dir, &haxelib, &["build", "--target", "wasm32-wasip1"]);
    let module = PathBuf::from(built.lines().last().expect("the module's path").trim());
    let ran = caribou(&dir, &haxelib, &["run", module.to_str().unwrap()]);
    // Without what the host says about loading libraries.
    let lines: Vec<&str> = ran.lines().filter(|l| !l.starts_with("[ash]")).collect();
    assert_eq!(
        lines,
        [
            "5",
            "42",
            "true",
            "HÉLLO!",
            "5",
            "3.5",
            "caught: quotient by zero",
            "5",
            "10",
            "1",
            "6",
            "true",
        ],
        "{ran}"
    );
    dir
}

const TALLY: &str = r#"
System.print("wren module ran")

class Tally {
  #export = "new(t: Num)"
  construct new(t) { _t = t }

  #export = "bump(x: Num) -> Num"
  bump(x) {
    _t = _t + x
    return _t
  }

  #export = "total -> Num"
  total { _t }
  #export = "total=(v: Num)"
  total=(v) { _t = v }

  #export = "make() -> Tally"
  static make() { Tally.new(7) }

  #export = "same(t: Tally) -> Tally"
  static same(t) { t }

  #export = "add(x: Num) -> Num"
  static add(x) { x + 1 }

  #export = "greet(s: String) -> String"
  static greet(s) { "hello " + s }

  #export = "even(n: Num) -> Bool"
  static even(n) { n % 2 == 0 }

  #export = "fail() -> Num"
  static fail() { Fiber.abort("wren says no") }
}
"#;

const CALLS_WREN: &str = r#"
import calc.tally.Tally;

class Main {
  static function main() {
    Sys.println(Tally.add(41));
    Sys.println(Tally.greet("haxe"));
    Sys.println(Tally.even(6));
    try {
      Tally.fail();
      Sys.println("no error");
    } catch (e:Dynamic) {
      Sys.println("caught: " + e);
    }
    var t = new Tally(10);
    Sys.println(t.bump(5));
    t.total = 2;
    Sys.println(t.total);
    Sys.println(Tally.make().total);
    Sys.println(Tally.same(t) == t);

    var checksum = 0.0;
    var identities = true;
    var failures = 0;
    for (i in 0...64) {
      t.total = i;
      checksum += t.bump(1);
      checksum += Tally.make().total;
      identities = identities && Tally.same(t) == t;
      if (Tally.greet("haxe") != "hello haxe" || !Tally.even(i * 2)) {
        throw "bad linked value";
      }
      try {
        Tally.fail();
      } catch (_:Dynamic) {
        failures++;
      }
    }
    Sys.println('stress $checksum $identities $failures');
  }
}
"#;

#[test]
fn a_wren_module_links_into_a_wasm_program() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("caribou-linked-wren");
    std::fs::create_dir_all(dir.join("src/calc")).unwrap();
    std::fs::write(dir.join("src/calc/tally.wren"), TALLY).unwrap();
    std::fs::write(dir.join("src/Main.hx"), CALLS_WREN).unwrap();
    std::fs::write(
        dir.join("hw.cbproj"),
        "[project]\nname = \"hw\"\nentry = \"haxe:Main\"\nlanguages = [\"haxe\", \"wren\"]\n",
    )
    .unwrap();
    let haxelib = dir.join("haxelib");
    std::fs::create_dir_all(&haxelib).unwrap();
    let status = Command::new("haxelib")
        .env("HAXELIB_PATH", &haxelib)
        .args(["dev", "caribou"])
        .arg(root.join("haxe"))
        .status()
        .expect("haxelib runs");
    assert!(status.success());

    let built = caribou(&dir, &haxelib, &["build", "--target", "wasm32-wasip1"]);
    let module = PathBuf::from(built.lines().last().expect("the module's path").trim());
    let ran = caribou(&dir, &haxelib, &["run", module.to_str().unwrap()]);
    let expected = [
        "wren module ran",
        "42",
        "hello haxe",
        "true",
        "caught: wren says no",
        "15",
        "2",
        "7",
        "true",
        "stress 2528 true 64",
    ];
    assert_eq!(ran.lines().collect::<Vec<_>>(), expected, "{ran}");

    // A wasm engine's locals are outside the core's conservative stack
    // scan. Collection on every allocation makes every linked conversion
    // prove that Rust adapter values are explicitly kept.
    let stressed = caribou_with_env(
        &dir,
        &haxelib,
        &["run", module.to_str().unwrap()],
        &[("ASH_GC_STRESS", "1")],
    );
    assert_eq!(
        stressed.lines().collect::<Vec<_>>(),
        [
            "wren module ran",
            "42",
            "hello haxe",
            "true",
            "caught: wren says no",
            "15",
            "2",
            "7",
            "true",
            "stress 2528 true 64",
        ],
        "{stressed}"
    );
}
