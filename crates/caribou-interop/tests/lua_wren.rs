//! A Wren program using a Lua class, imported as any class of the world
//! is: `Counter.new` makes an instance, its `:` functions are its
//! methods, its fields are its getters and setters, the class's other
//! fields are its static getters and setters, and its other functions
//! its static methods, each typed by the module's LuaLS annotations. A
//! Wren function goes to Lua and is called there, a typed array is a
//! buffer Lua reads in place, a Lua function comes back for Wren to
//! call, an instance a method returns is a `Counter`, and several
//! results are a list of them.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use caribou::registry::{self, Namespace, TupleField, TypeRef};
use caribou::world::{Config, World};
use caribou_zyntax::Frontend;
use wren_lift::runtime::engine::{ExecutionMode, InterpretResult};
use wren_lift::runtime::vm::{VM, VMConfig};

const USE: &str = r#"
import "game:counter" for Counter
var c = Counter.new(3)
System.print(c.n)
System.print(c.bump(4))
System.print(c.bump(9))
System.print(Counter.LIMIT)
Counter.LIMIT = 20
System.print(c.bump(5))
c.n = 2
System.print(c.sum {|i| i * 10 })
System.print(c.label)
System.print(Counter.checksum(ByteArray.fromList([97, 98, 99])))
var add = Counter.adder(5)
System.print(add.call(2))
var d = c.next()
System.print(d.bump(1))
System.print(c.state())
System.print(Counter.parse("12")[0])
System.print(Counter.parse("x")[1])
"#;

#[test]
fn a_wren_program_uses_a_lua_class() {
    caribou_ash::install().expect("ash takes the table in a fresh process");
    caribou_wren::install().expect("wren_lift takes the table in a fresh process");
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/lua/src");
    let world = World::new(Config {
        namespaces: vec![Namespace {
            name: "game".to_owned(),
            langs: vec!["wren".to_owned(), "lua".to_owned()],
            modules: None,
        }],
        roots: vec![root],
        ..Config::default()
    });
    world
        .register(Box::new(caribou_wren::Runtime::new()))
        .expect("wren registers");
    world
        .register(Box::new(caribou_zyntax::Runtime::new(vec![Frontend::new(
            Box::new(caribou_lua::Lua::new()),
        )])))
        .expect("lua registers");

    let errors = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&errors);
    let mut config = VMConfig {
        execution_mode: ExecutionMode::Interpreter,
        error_fn: Some(Box::new(move |_kind, _module, _line, message: &str| {
            sink.borrow_mut().push(message.to_owned());
        })),
        ..VMConfig::default()
    };
    caribou_wren::import::configure(&mut config);
    let mut vm = VM::new(config);
    vm.krio_fiber_active = true;
    vm.output_buffer = Some(String::new());
    let result = caribou_wren::with_vm(&mut vm, |vm| vm.interpret("use", USE));
    let output = vm.take_output();
    assert_eq!(
        result,
        InterpretResult::Success,
        "{:?} {output:?}",
        errors.borrow()
    );
    assert_eq!(
        output,
        "3\n7\n10\n10\n15\n30\ncount\n294\n7\n4\n[2, count]\n12\nnot a number: x\n"
    );

    // The class as it is published: what the chunk's types know of it,
    // typed by its annotations.
    let iface = registry::lookup("game", "counter").expect("published");
    let counter = &iface.classes[0];
    assert_eq!(counter.name, "Counter");
    let own = TypeRef::Object("lua.game/counter.Counter".to_owned());
    assert_eq!(counter.type_name, "lua.game/counter.Counter");
    let ctor = counter.ctor.as_ref().expect("new constructs");
    assert_eq!((&ctor.params[..], &ctor.ret), (&[TypeRef::Int][..], &own));
    let int_fn = TypeRef::Function {
        params: vec![TypeRef::Int],
        ret: Box::new(TypeRef::Int),
    };
    // Several results are a tuple of them, each named.
    let field = |name: &str, ty: TypeRef| TupleField {
        name: name.to_owned(),
        ty,
    };
    let state = TypeRef::Tuple(vec![
        field("count", TypeRef::Int),
        field("label", TypeRef::Str),
    ]);
    let parsed = TypeRef::Tuple(vec![
        field("count", TypeRef::Dyn),
        field("error", TypeRef::Dyn),
    ]);
    let members: Vec<(&str, bool, &[TypeRef], &TypeRef)> = counter
        .methods
        .iter()
        .map(|m| (m.name.as_str(), m.is_static, &m.params[..], &m.ret))
        .collect();
    assert_eq!(
        members,
        [
            ("parse", true, &[TypeRef::Str][..], &parsed),
            ("checksum", true, &[TypeRef::Dyn][..], &TypeRef::Int),
            ("adder", true, &[TypeRef::Int][..], &int_fn),
            ("bump", false, &[TypeRef::Int][..], &TypeRef::Int),
            ("sum", false, &[int_fn.clone()][..], &TypeRef::Int),
            ("next", false, &[][..], &own),
            ("state", false, &[][..], &state),
        ]
    );
    let fields: Vec<(&str, &TypeRef)> = counter
        .fields
        .iter()
        .map(|f| (f.name.as_str(), &f.ty))
        .collect();
    assert_eq!(fields, [("n", &TypeRef::Int), ("label", &TypeRef::Str)]);
    let statics: Vec<(&str, &TypeRef)> = counter
        .statics
        .iter()
        .map(|f| (f.name.as_str(), &f.ty))
        .collect();
    assert_eq!(statics, [("LIMIT", &TypeRef::Int)]);
}
