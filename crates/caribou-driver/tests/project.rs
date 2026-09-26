//! A project declared by its `.cbproj`: `caribou run` in its directory
//! compiles the Haxe entry and runs it with the Wren modules it declares,
//! and `caribou build` writes the bundle to its `target/`.

use std::path::{Path, PathBuf};
use std::process::Command;

const CBPROJ: &str = r#"
[project]
name = "hudgame"
entry = "haxe:Main"

[languages.haxe]
roots = ["src"]

[languages.wren]
roots = ["src"]
"#;

const MAIN: &str = r#"
import game.hud.Hud;

class Main {
  static function main() {
    var h = new Hud(3);
    Sys.println("haxe asks wren: " + h.add(4));
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
fn a_project_file_runs_and_builds_its_program() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let fixtures = root.join("crates/caribou-interop/fixtures/src/game");
    let dir = std::env::temp_dir().join(format!("caribou-project-{}", std::process::id()));
    let game = dir.join("src/game");
    std::fs::create_dir_all(&game).unwrap();
    for file in ["hud.wren", "format.wren", "Player.hx"] {
        std::fs::copy(fixtures.join(file), game.join(file)).unwrap();
    }
    std::fs::write(dir.join("src/Main.hx"), MAIN).unwrap();
    std::fs::write(dir.join("hudgame.cbproj"), CBPROJ).unwrap();
    // Caribou's haxelib from this checkout, in a repository of the test's own.
    let haxelib = dir.join("haxelib");
    std::fs::create_dir_all(&haxelib).unwrap();
    let status = Command::new("haxelib")
        .env("HAXELIB_PATH", &haxelib)
        .args(["dev", "caribou"])
        .arg(root.join("haxe"))
        .status()
        .expect("haxelib runs");
    assert!(status.success());

    let ran = caribou(&dir, &haxelib, &["run"]);
    assert!(ran.contains("haxe asks wren: 7"), "{ran}");

    caribou(&dir, &haxelib, &["build"]);
    assert!(dir.join("target/hudgame.cb").is_file());
    std::fs::remove_dir_all(&dir).ok();
}
