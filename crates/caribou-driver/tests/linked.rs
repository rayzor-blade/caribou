//! A project built ahead of time for wasm, its plugin crate and its Wren
//! modules linked into the program: the program calls their members
//! directly, and `caribou run` runs the module. Needs the `llvm` feature,
//! which is what builds ahead of time.
#![cfg(feature = "llvm")]

use std::path::{Path, PathBuf};
use std::process::Command;

const MAIN: &str = r#"
import math.Math;

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
  }
}
"#;

fn caribou(dir: &Path, haxelib: &Path, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_caribou"))
        .args(args)
        .current_dir(dir)
        .env("HAXELIB_PATH", haxelib)
        .output()
        .expect("caribou runs");
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
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    // Kept between runs: the program's runtime builds from caribou's source.
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("caribou-linked");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/Main.hx"), MAIN).unwrap();
    let math = root.join("crates/caribou-interop/plugins/math");
    std::fs::write(
        dir.join("plug.cbproj"),
        format!(
            "[project]\nname = \"plug\"\nentry = \"haxe:Main\"\nlanguages = [\"haxe\"]\n\n\
             [plugins]\nmath = {{ path = {:?} }}\n",
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
    let lines: Vec<&str> = ran.lines().collect();
    assert_eq!(
        lines,
        ["5", "42", "true", "HÉLLO!", "5", "3.5", "caught: quotient by zero"],
        "{ran}"
    );
}

const TALLY: &str = r#"
System.print("wren module ran")

class Tally {
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
    let lines: Vec<&str> = ran.lines().collect();
    assert_eq!(
        lines,
        ["wren module ran", "42", "hello haxe", "true", "caught: wren says no"],
        "{ran}"
    );
}
