//! A root's Zyntax modules as data: what the Haxe build macro reads.

use std::path::PathBuf;

use caribou::describe::MemberKind;

#[test]
fn describes_the_modules_under_a_root() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../caribou-interop/fixtures/src");
    std::fs::write(root.join("zynml.zsnap"), zynml::snapshot_bytes()).unwrap();
    let modules = caribou_zyntax::describe(&root).expect("describes");
    let scorer = modules
        .iter()
        .find(|m| m.module == "game/scorer")
        .unwrap_or_else(|| panic!("{modules:?}"));
    assert_eq!(scorer.lang, "zynml");
    assert!(scorer.path.as_deref().is_some_and(|p| p.ends_with("game/scorer.zynml")));
    let names: Vec<&str> = scorer.classes.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["Point", "Scorer"]);
    let class = &scorer.classes[1];
    let names: Vec<&str> = class.members.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, ["score", "weight", "perfect", "echo"]);
    let point = &scorer.classes[0];
    let fields: Vec<&str> = point.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(fields, ["x", "y"]);
    let members: Vec<(&str, MemberKind)> = point
        .members
        .iter()
        .map(|m| (m.name.as_str(), m.kind.clone()))
        .collect();
    assert_eq!(
        members,
        [
            ("new", MemberKind::Constructor),
            ("len", MemberKind::Method),
            ("origin", MemberKind::Factory),
            ("area", MemberKind::Static)
        ]
    );
}
