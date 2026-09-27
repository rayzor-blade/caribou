//! The wire between a plugin and a browser's implementation of a WebIDL
//! API, both halves generated from one IDL: Rust for the plugin, which
//! encodes each call into bytes in the program's memory, and JavaScript
//! for the agent that holds the API, which decodes and makes the call.
//!
//! A command is its operation's number and its length, then its fields:
//! the receiver's handle, the handle its interface result is kept under,
//! the address of its reply record when it has a reply, then its
//! arguments. Handles are the plugin's, so making an object needs no round
//! trip. A value is encoded by its IDL type: integers and floats at their
//! widths, little-endian; a boolean as a byte; a string as its UTF-8
//! length and bytes; an enum as its value's index; an interface as its
//! handle, zero for none; bytes as an address and length in the program's
//! memory, read where they are; an optional or nullable value as a byte
//! saying whether it is present; a sequence as a count and its items; a
//! record as a count and its key and value pairs; a union as the index of
//! its alternative and the value. A dictionary's members follow in order,
//! its ancestors' first, each optional one flagged, so a member left out
//! stays out and takes the API's default.
//!
//! A reply record is four words: its state (0 pending, 1 done, 2
//! rejected, 3 too small), the length written, and the address and
//! capacity of the caller's buffer, which takes the result encoded as
//! above (bytes as they are), or a rejection's message. The agent stores the state last and
//! notifies it.
//!
//! The program hands batches of commands to the agent through a mailbox in
//! its memory, which the agent serves; the plugin's `Mailbox` gives the
//! layout. After the IDL's operations comes one that forgets a handle.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use crate::idl::{Int, Model, Operation, Ty};

/// Both halves of the wire.
pub struct Wire {
    /// Rust, for a plugin to `include!` inside a module of its own.
    pub rust: String,
    /// An ES module the agent imports.
    pub js: String,
}

/// Generate the wire for every operation and attribute of `webidl` that
/// carries only values the wire has encodings for.
pub fn wire(webidl: &str) -> Result<Wire, String> {
    Ok(generate(&crate::idl::parse(webidl)?).0)
}

/// The wire for `model`, and the names of the dictionaries, enums and
/// unions it has types for.
pub(crate) fn generate(model: &Model) -> (Wire, BTreeSet<String>) {
    let ops = operations(model);
    let mut g = Gen {
        model,
        rust: String::new(),
        js: String::new(),
        named: BTreeSet::new(),
    };
    g.prelude();
    for op in &ops {
        for a in &op.args {
            g.need(&a.ty);
        }
        g.need(&op.reply_ty);
    }
    g.ops(&ops);
    (
        Wire {
            rust: g.rust,
            js: g.js,
        },
        g.named,
    )
}

/// What a command does, as both halves name it.
struct Op {
    /// `GPUDevice.createBuffer`, or `.get_label` for an attribute.
    idl: String,
    /// The Rust method, `gpu_device_create_buffer`.
    method: String,
    call: Call,
    args: Vec<ArgOp>,
    /// Kept under a handle the plugin chose: the interface the call makes.
    makes: bool,
    /// Whether the call has a reply record, and what the reply carries.
    replies: bool,
    promise: bool,
    reply_ty: Ty,
}

enum Call {
    Method(String),
    Get(String),
    Set(String),
    Values,
}

struct ArgOp {
    name: String,
    ty: Ty,
    optional: bool,
}

/// Every operation, attribute and setlike the wire carries, numbered in
/// declaration order.
fn operations(model: &Model) -> Vec<Op> {
    let mut out = Vec::new();
    for i in &model.interfaces {
        let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
        for o in &i.operations {
            let n = seen.entry(&o.name).or_default();
            *n += 1;
            let suffix = if *n > 1 {
                format!("_{n}")
            } else {
                String::new()
            };
            if let Some(op) = operation(model, i.name.as_str(), o, &suffix) {
                out.push(op);
            }
        }
        for a in &i.attributes {
            if !carried(model, &a.ty) {
                continue;
            }
            out.push(result_op(
                &i.name,
                &format!("get_{}", snake(&a.name)),
                &format!("{}.{}", i.name, a.name),
                Call::Get(a.name.clone()),
                Vec::new(),
                &a.ty,
            ));
            if !a.readonly {
                out.push(Op {
                    idl: format!("{}.{} =", i.name, a.name),
                    method: format!("{}_set_{}", snake(&i.name), snake(&a.name)),
                    call: Call::Set(a.name.clone()),
                    args: vec![ArgOp {
                        name: "value".into(),
                        ty: a.ty.clone(),
                        optional: false,
                    }],
                    makes: false,
                    replies: false,
                    promise: false,
                    reply_ty: Ty::Undefined,
                });
            }
        }
        if let Some(element) = &i.setlike {
            out.push(result_op(
                &i.name,
                "values",
                &format!("{}.values", i.name),
                Call::Values,
                Vec::new(),
                &Ty::Sequence(Box::new(element.clone())),
            ));
        }
    }
    out
}

fn operation(model: &Model, interface: &str, o: &Operation, suffix: &str) -> Option<Op> {
    if !carried(model, &o.ret) || o.args.iter().any(|a| !carried(model, &a.ty)) {
        return None;
    }
    let args = o
        .args
        .iter()
        .map(|a| ArgOp {
            name: a.name.clone(),
            ty: if a.variadic {
                Ty::Sequence(Box::new(a.ty.clone()))
            } else {
                a.ty.clone()
            },
            optional: a.optional && !a.variadic,
        })
        .collect();
    Some(result_op(
        interface,
        &format!("{}{suffix}", snake(&o.name)),
        &format!("{interface}.{}", o.name),
        Call::Method(o.name.clone()),
        args,
        &o.ret,
    ))
}

/// An op returning `ret`: an interface is kept under a handle; anything
/// else, and every promise, replies.
fn result_op(interface: &str, name: &str, idl: &str, call: Call, args: Vec<ArgOp>, ret: &Ty) -> Op {
    let (promise, inner) = match ret {
        Ty::Promise(inner) => (true, (**inner).clone()),
        other => (false, other.clone()),
    };
    let makes = matches!(&inner, Ty::Interface(_))
        || matches!(&inner, Ty::Nullable(t) if matches!(**t, Ty::Interface(_)));
    // What the reply's buffer carries: an interface is its handle's
    // presence, when it may be absent.
    let reply_ty = match &inner {
        Ty::Interface(_) | Ty::Undefined => Ty::Undefined,
        Ty::Nullable(t) if matches!(**t, Ty::Interface(_)) => Ty::Boolean,
        other => other.clone(),
    };
    let replies = promise || reply_ty != Ty::Undefined;
    Op {
        idl: idl.to_owned(),
        method: format!("{}_{name}", snake(interface)),
        call,
        args,
        makes,
        replies,
        promise,
        reply_ty,
    }
}

/// Whether the wire has an encoding for every value of `ty`: a dictionary
/// is carried when its required members are, and an optional member that
/// is not stays off the wire.
pub(crate) fn carried(model: &Model, ty: &Ty) -> bool {
    carried_in(model, ty, &mut BTreeSet::new())
}

fn carried_in(model: &Model, ty: &Ty, seen: &mut BTreeSet<String>) -> bool {
    match ty {
        Ty::Opaque(_) => false,
        Ty::Sequence(t) | Ty::Record(t) | Ty::Nullable(t) | Ty::Promise(t) => {
            carried_in(model, t, seen)
        }
        Ty::Union(_, alternatives) => alternatives.iter().all(|a| carried_in(model, a, seen)),
        Ty::Dictionary(name) => {
            if !seen.insert(name.clone()) {
                return true;
            }
            model
                .dictionary_members(name)
                .into_iter()
                .all(|m| !m.required || carried_in(model, &m.ty, seen))
        }
        _ => true,
    }
}

struct Gen<'m> {
    model: &'m Model,
    rust: String,
    js: String,
    /// Named types already emitted: enums, dictionaries, unions.
    named: BTreeSet<String>,
}

impl Gen<'_> {
    fn prelude(&mut self) {
        self.rust.push_str(RUST_PRELUDE);
        self.js.push_str(JS_PRELUDE);
    }

    /// Emit the named types `ty` reaches, once each.
    fn need(&mut self, ty: &Ty) {
        match ty {
            Ty::Enum(name) => {
                if self.named.insert(name.clone()) {
                    self.enumeration(name);
                }
            }
            Ty::Dictionary(name) => {
                if self.named.insert(name.clone()) {
                    let members: Vec<_> = self
                        .model
                        .dictionary_members(name)
                        .into_iter()
                        .filter(|m| carried(self.model, &m.ty))
                        .cloned()
                        .collect();
                    for m in &members {
                        self.need(&m.ty);
                    }
                    self.dictionary(name, &members);
                }
            }
            Ty::Union(_, alternatives) => {
                let name = union_name(ty);
                if self.named.insert(name.clone()) {
                    for a in alternatives {
                        self.need(a);
                    }
                    self.union(&name, alternatives);
                }
            }
            Ty::Sequence(t) | Ty::Record(t) | Ty::Nullable(t) | Ty::Promise(t) => self.need(t),
            _ => {}
        }
    }

    fn enumeration(&mut self, name: &str) {
        let values = &self.model.enums[name];
        let r = &mut self.rust;
        let _ = writeln!(
            r,
            "\n/// `{name}`.\n#[derive(Clone, Copy, Debug, PartialEq, Eq)]\npub enum {name} {{"
        );
        for v in values {
            let _ = writeln!(r, "    /// `\"{v}\"`.\n    {},", variant(v));
        }
        let _ = writeln!(
            r,
            "}}\n\nimpl Encode for {name} {{\n    fn encode(&self, e: &mut Encoder) {{\n        e.u32(*self as u32);\n    }}\n}}"
        );
        let _ = writeln!(
            r,
            "\nimpl {name} {{\n    /// The value at `index` in the IDL's order.\n    pub fn from_index(index: u32) -> Option<Self> {{\n        Some(match index {{"
        );
        for (i, v) in values.iter().enumerate() {
            let _ = writeln!(r, "            {i} => Self::{},", variant(v));
        }
        let _ = writeln!(r, "            _ => return None,\n        }})\n    }}\n}}");
        let _ = writeln!(
            r,
            "\nimpl Decode for {name} {{\n    fn decode(d: &mut Decoder) -> Option<Self> {{\n        Self::from_index(d.u32()?)\n    }}\n}}"
        );
        let list = values
            .iter()
            .map(|v| format!("{v:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(self.js, "\nconst {name} = [{list}];");
    }

    fn dictionary(&mut self, name: &str, members: &[crate::idl::Member]) {
        let mut def =
            format!("\n/// `{name}`.\n#[derive(Clone, Debug, PartialEq)]\npub struct {name} {{\n");
        let (e, d) = if members.is_empty() {
            ("_", "_")
        } else {
            ("e", "d")
        };
        let mut enc =
            format!("\nimpl Encode for {name} {{\n    fn encode(&self, {e}: &mut Encoder) {{\n");
        let mut dec = format!(
            "\nimpl Decode for {name} {{\n    fn decode({d}: &mut Decoder) -> Option<Self> {{\n        Some(Self {{\n"
        );
        let mut read = format!("\nfunction read_{name}(r) {{\n  const o = {{}};\n");
        let mut write = format!("\nfunction write_{name}(w, o) {{\n");
        for m in members {
            let field = field(&m.name);
            let ty = if m.required {
                rust_ty(&m.ty)
            } else {
                format!("Option<{}>", rust_ty(&m.ty))
            };
            let _ = writeln!(def, "    pub {field}: {ty},");
            let _ = writeln!(enc, "        self.{field}.encode(e);");
            let _ = writeln!(dec, "            {field}: Decode::decode(d)?,");
            let key = js_key(&m.name);
            if m.required {
                let _ = writeln!(read, "  o{key} = {};", js_read(&m.ty));
                let _ = writeln!(write, "  {}", js_write(&m.ty, &format!("o{key}")));
            } else {
                let _ = writeln!(read, "  if (r.u8()) o{key} = {};", js_read(&m.ty));
                let _ = writeln!(
                    write,
                    "  if (o{key} === undefined) w.u8(0); else {{ w.u8(1); {} }}",
                    js_write(&m.ty, &format!("o{key}"))
                );
            }
        }
        def.push_str("}\n");
        enc.push_str("    }\n}\n");
        dec.push_str("        })\n    }\n}\n");
        read.push_str("  return o;\n}\n");
        write.push_str("}\n");
        self.rust.push_str(&def);
        self.rust.push_str(&enc);
        self.rust.push_str(&dec);
        self.js.push_str(&read);
        self.js.push_str(&write);
    }

    fn union(&mut self, name: &str, alternatives: &[Ty]) {
        let mut def = format!(
            "\n/// A union: `{name}`.\n#[derive(Clone, Debug, PartialEq)]\npub enum {name} {{\n"
        );
        let mut enc = format!(
            "\nimpl Encode for {name} {{\n    fn encode(&self, e: &mut Encoder) {{\n        match self {{\n"
        );
        let mut dec = format!(
            "\nimpl Decode for {name} {{\n    fn decode(d: &mut Decoder) -> Option<Self> {{\n        Some(match d.u8()? {{\n"
        );
        let mut read = format!("\nfunction read_{name}(r) {{\n  switch (r.u8()) {{\n");
        // A value names its alternative by what it is: an object by its
        // interface or its fields, an array by being one.
        let mut write = format!("\nfunction write_{name}(w, v) {{\n");
        for (i, a) in alternatives.iter().enumerate() {
            let variant = alternative_name(a);
            let _ = writeln!(def, "    {variant}({}),", rust_ty(a));
            let _ = writeln!(
                enc,
                "            Self::{variant}(v) => {{\n                e.u8({i});\n                v.encode(e);\n            }}"
            );
            let _ = writeln!(
                dec,
                "            {i} => Self::{variant}(Decode::decode(d)?),"
            );
            let _ = writeln!(read, "    case {i}: return {};", js_read(a));
            let _ = writeln!(
                write,
                "  if ({}) {{ w.u8({i}); {} return; }}",
                js_is(a, "v"),
                js_write(a, "v")
            );
        }
        def.push_str("}\n");
        enc.push_str("        }\n    }\n}\n");
        dec.push_str("            _ => return None,\n        })\n    }\n}\n");
        read.push_str("  }\n  throw new Error(\"bad union alternative\");\n}\n");
        let _ = writeln!(
            write,
            "  throw new Error(\"no alternative of {name} fits\");\n}}"
        );
        self.rust.push_str(&def);
        self.rust.push_str(&enc);
        self.rust.push_str(&dec);
        self.js.push_str(&read);
        self.js.push_str(&write);
    }

    fn ops(&mut self, ops: &[Op]) {
        let mut methods = String::from("\nimpl Encoder {\n");
        let mut table = String::from("\nexport const OPS = [\n");
        for (n, op) in ops.iter().enumerate() {
            // Rust: one method per op.
            let mut params = String::from("&mut self, this: Handle");
            let mut body =
                format!("        let at = self.begin({n});\n        this.encode(self);\n");
            if op.makes {
                params.push_str(", result: Handle");
                body.push_str("        result.encode(self);\n");
            }
            if op.replies {
                params.push_str(", reply: u32");
                body.push_str("        self.u32(reply);\n");
            }
            for a in &op.args {
                let ty = if a.optional {
                    format!("Option<{}>", rust_ty(&a.ty))
                } else {
                    rust_ty(&a.ty)
                };
                let _ = write!(params, ", {}: &{ty}", field(&a.name));
                let _ = writeln!(body, "        {}.encode(self);", field(&a.name));
            }
            body.push_str("        self.end(at);\n");
            let _ = writeln!(
                methods,
                "    /// `{}`.\n    pub fn {}({params}) {{\n{body}    }}",
                op.idl, op.method
            );

            // JavaScript: the op's decoder, at its number.
            let mut js = format!(
                "  // {n}: {}\n  (wire, r) => {{\n    const self = wire.get(r.u32());\n",
                op.idl
            );
            if op.makes {
                js.push_str("    const result = r.u32();\n");
            }
            if op.replies {
                js.push_str("    const reply = r.u32();\n");
            }
            let mut names = Vec::new();
            for (i, a) in op.args.iter().enumerate() {
                let read = if a.optional {
                    format!("r.u8() ? {} : undefined", js_read(&a.ty))
                } else {
                    js_read(&a.ty)
                };
                let _ = writeln!(js, "    const a{i} = {read};");
                names.push(format!("a{i}"));
            }
            let args = names.join(", ");
            let call = match &op.call {
                Call::Method(m) => format!("self.{m}({args})"),
                Call::Get(a) => format!("self{}", js_key(a)),
                Call::Set(a) => format!("(self{} = {args})", js_key(a)),
                Call::Values => "Array.from(self.values())".to_owned(),
            };
            let keep = if op.makes {
                "if (v) wire.set(result, v); "
            } else {
                ""
            };
            let answer = match &op.reply_ty {
                Ty::Undefined => "null".to_owned(),
                Ty::Bytes => "new Uint8Array(v)".to_owned(),
                Ty::Boolean if op.makes => "encode((w) => w.u8(v ? 1 : 0))".to_owned(),
                ty => format!("encode((w) => {{ {} }})", js_write(ty, "v")),
            };
            if op.promise {
                let _ = writeln!(
                    js,
                    "    {call}.then((v) => {{ {keep}wire.reply(reply, 1, {answer}); }}, (e) => wire.reply(reply, 2, message(e)));"
                );
            } else if op.replies {
                let _ = writeln!(
                    js,
                    "    let v;\n    try {{ v = {call}; }} catch (e) {{ wire.reply(reply, 2, message(e)); return; }}\n    {keep}wire.reply(reply, 1, {answer});"
                );
            } else if op.makes {
                let _ = writeln!(js, "    const v = {call};\n    {keep}");
            } else {
                let _ = writeln!(js, "    {call};");
            }
            js.push_str("  },\n");
            table.push_str(&js);
        }
        let n = ops.len();
        let _ = writeln!(
            methods,
            "    /// Forget the object kept under `this`.\n    pub fn release(&mut self, this: Handle) {{\n        let at = self.begin({n});\n        this.encode(self);\n        self.end(at);\n    }}"
        );
        let _ = writeln!(
            table,
            "  // {n}: release\n  (wire, r) => {{\n    wire.objects.delete(r.u32());\n  }},"
        );
        methods.push_str("}\n");
        table.push_str("];\n");
        self.rust.push_str(&methods);
        self.js.push_str(&table);
        let _ = writeln!(
            self.rust,
            "\n/// The number of operations the wire carries.\npub const OPS: u32 = {};",
            ops.len() + 1
        );
    }
}

/// The Rust type of `ty`.
fn rust_ty(ty: &Ty) -> String {
    match ty {
        Ty::Undefined => "()".into(),
        Ty::Boolean => "bool".into(),
        Ty::Integer(i) => int_name(*i).into(),
        Ty::Float(true) => "f64".into(),
        Ty::Float(false) => "f32".into(),
        Ty::String => "String".into(),
        Ty::Bytes => "Bytes".into(),
        Ty::Enum(n) | Ty::Dictionary(n) => n.clone(),
        Ty::Interface(_) => "Handle".into(),
        Ty::Sequence(t) => format!("Vec<{}>", rust_ty(t)),
        Ty::Record(t) => format!("Vec<(String, {})>", rust_ty(t)),
        Ty::Nullable(t) => format!("Option<{}>", rust_ty(t)),
        Ty::Union(..) => union_name(ty),
        Ty::Promise(t) => rust_ty(t),
        Ty::Opaque(n) => unreachable!("{n} is not carried"),
    }
}

pub(crate) fn int_name(i: Int) -> &'static str {
    match i {
        Int::I8 => "i8",
        Int::U8 => "u8",
        Int::I16 => "i16",
        Int::U16 => "u16",
        Int::I32 => "i32",
        Int::U32 => "u32",
        Int::I64 => "i64",
        Int::U64 => "u64",
    }
}

/// A union's Rust name: the typedef that names it, else its alternatives'.
pub(crate) fn union_name(ty: &Ty) -> String {
    match ty {
        Ty::Union(Some(name), _) => name.clone(),
        Ty::Union(None, alternatives) => alternatives
            .iter()
            .map(alternative_name)
            .collect::<Vec<_>>()
            .join("Or"),
        other => alternative_name(other),
    }
}

/// The variant a union names an alternative by.
pub(crate) fn alternative_name(ty: &Ty) -> String {
    match ty {
        Ty::Boolean => "Boolean".into(),
        Ty::Integer(i) => pascal(int_name(*i)),
        Ty::Float(true) => "Double".into(),
        Ty::Float(false) => "Float".into(),
        Ty::String => "String".into(),
        Ty::Bytes => "Bytes".into(),
        Ty::Enum(n) | Ty::Dictionary(n) | Ty::Interface(n) => n.clone(),
        Ty::Sequence(t) => format!("SequenceOf{}", alternative_name(t)),
        Ty::Record(t) => format!("RecordOf{}", alternative_name(t)),
        Ty::Nullable(t) => format!("Nullable{}", alternative_name(t)),
        Ty::Union(..) => union_name(ty),
        Ty::Promise(t) => alternative_name(t),
        Ty::Undefined => "Undefined".into(),
        Ty::Opaque(n) => n.clone(),
    }
}

/// The JavaScript expression reading a value of `ty` from `r`.
fn js_read(ty: &Ty) -> String {
    match ty {
        Ty::Undefined => "undefined".into(),
        Ty::Boolean => "r.u8() !== 0".into(),
        Ty::Integer(i) => format!("r.{}()", int_name(*i)),
        Ty::Float(true) => "r.f64()".into(),
        Ty::Float(false) => "r.f32()".into(),
        Ty::String => "r.str()".into(),
        Ty::Bytes => "r.bytes()".into(),
        Ty::Enum(n) => format!("{n}[r.u32()]"),
        Ty::Dictionary(n) => format!("read_{n}(r)"),
        Ty::Interface(_) => "r.object()".into(),
        Ty::Sequence(t) => format!("r.seq(() => {})", js_read(t)),
        Ty::Record(t) => format!("r.record(() => {})", js_read(t)),
        Ty::Nullable(t) => format!("(r.u8() ? {} : null)", js_read(t)),
        Ty::Union(..) => format!("read_{}(r)", union_name(ty)),
        Ty::Promise(t) => js_read(t),
        Ty::Opaque(n) => unreachable!("{n} is not carried"),
    }
}

/// The JavaScript statement writing `v`, of `ty`, to `w`.
fn js_write(ty: &Ty, v: &str) -> String {
    match ty {
        Ty::Undefined => String::new(),
        Ty::Boolean => format!("w.u8({v} ? 1 : 0);"),
        Ty::Integer(i) => format!("w.{}({v});", int_name(*i)),
        Ty::Float(true) => format!("w.f64({v});"),
        Ty::Float(false) => format!("w.f32({v});"),
        Ty::String => format!("w.str({v});"),
        Ty::Bytes => "throw new Error(\"bytes do not come back\");".into(),
        Ty::Enum(n) => format!("w.u32({n}.indexOf({v}));"),
        Ty::Dictionary(n) => format!("write_{n}(w, {v});"),
        Ty::Interface(_) => "throw new Error(\"objects come back under handles\");".into(),
        Ty::Sequence(t) => format!("w.seq({v}, (x) => {{ {} }});", js_write(t, "x")),
        Ty::Record(t) => format!("w.record({v}, (x) => {{ {} }});", js_write(t, "x")),
        Ty::Nullable(t) => format!(
            "if ({v} == null) w.u8(0); else {{ w.u8(1); {} }}",
            js_write(t, v)
        ),
        Ty::Union(..) => format!("write_{}(w, {v});", union_name(ty)),
        Ty::Promise(t) => js_write(t, v),
        Ty::Opaque(n) => unreachable!("{n} is not carried"),
    }
}

/// A JavaScript test for whether `v` is a value of the union alternative
/// `ty`.
fn js_is(ty: &Ty, v: &str) -> String {
    match ty {
        Ty::Undefined => format!("{v} === undefined"),
        Ty::Boolean => format!("typeof {v} === \"boolean\""),
        Ty::Integer(_) | Ty::Float(_) => format!("typeof {v} === \"number\""),
        Ty::String | Ty::Enum(_) => format!("typeof {v} === \"string\""),
        Ty::Sequence(_) => format!("Array.isArray({v})"),
        Ty::Interface(n) => format!("typeof {n} !== \"undefined\" && {v} instanceof {n}"),
        _ => format!("typeof {v} === \"object\""),
    }
}

/// A Rust field or argument name for an IDL member.
pub(crate) fn field(name: &str) -> String {
    let s = snake(name);
    const KEYWORDS: &[&str] = &[
        "type", "loop", "match", "ref", "move", "use", "in", "self", "fn", "mod", "struct", "enum",
        "const", "static", "where", "impl", "trait", "box", "yield", "async", "await", "dyn",
        "abstract", "final", "override",
    ];
    if KEYWORDS.contains(&s.as_str()) {
        format!("r#{s}")
    } else {
        s
    }
}

/// A JavaScript property access for an IDL member.
fn js_key(name: &str) -> String {
    format!(".{name}")
}

/// `createBindGroupLayout` as `create_bind_group_layout`, `GPUDevice` as
/// `gpu_device`, `maxTextureDimension1D` as `max_texture_dimension1d`.
pub(crate) fn snake(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if c.is_ascii_uppercase() {
            let prev = i.checked_sub(1).map(|p| chars[p]);
            let next = chars.get(i + 1).copied();
            let boundary = match prev {
                Some(p) if p.is_ascii_lowercase() => true,
                Some(p) if p.is_ascii_uppercase() => next.is_some_and(|n| n.is_ascii_lowercase()),
                _ => false,
            };
            if boundary {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn pascal(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_ascii_uppercase().to_string() + c.as_str())
        .unwrap_or_default()
}

/// An enum value as a variant: `"high-performance"` as `HighPerformance`,
/// `"2d-array"` as `V2dArray`.
fn variant(value: &str) -> String {
    let name: String = value
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|p| !p.is_empty())
        .map(pascal)
        .collect();
    if name.is_empty() {
        "Empty".into()
    } else if name.starts_with(|c: char| c.is_ascii_digit()) {
        format!("V{name}")
    } else {
        name
    }
}

/// What every generated Rust half starts with.
const RUST_PRELUDE: &str = r#"// Generated by caribou-bindgen from WebIDL: the plugin's half of the wire.
// See caribou-bindgen's `wire` module for the encoding.

/// An object the agent holds, by the handle the plugin gave it; zero is
/// none.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Handle(pub u32);

/// Bytes in the program's memory, which the agent reads where they are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bytes {
    pub address: u32,
    pub len: u32,
}

/// Commands, encoded one after another.
#[derive(Default)]
pub struct Encoder {
    pub bytes: Vec<u8>,
}

impl Encoder {
    pub fn new() -> Self {
        Self::default()
    }
    fn begin(&mut self, op: u32) -> usize {
        self.u32(op);
        let at = self.bytes.len();
        self.u32(0);
        at
    }
    fn end(&mut self, at: usize) {
        let len = (self.bytes.len() - at - 4) as u32;
        self.bytes[at..at + 4].copy_from_slice(&len.to_le_bytes());
    }
    pub fn u8(&mut self, v: u8) {
        self.bytes.push(v);
    }
    pub fn u32(&mut self, v: u32) {
        self.bytes.extend_from_slice(&v.to_le_bytes());
    }
}

/// Where the program hands the agent its commands: six words in the
/// program's memory, which the agent serves. `sent` counts the batches
/// handed over and `done` those the agent has run, one in flight at a
/// time, at `address` for `len` bytes; `settled` counts the replies the
/// agent has settled. `wake`, when set, is the address of a word the agent
/// adds one to and notifies after settling one: how a program that waits
/// for replies other than by sleeping on `settled` hears of them.
#[repr(C)]
#[derive(Default)]
pub struct Mailbox {
    pub sent: core::sync::atomic::AtomicI32,
    pub done: core::sync::atomic::AtomicI32,
    pub address: core::sync::atomic::AtomicU32,
    pub len: core::sync::atomic::AtomicU32,
    pub settled: core::sync::atomic::AtomicI32,
    pub wake: core::sync::atomic::AtomicU32,
}

impl Mailbox {
    pub const fn new() -> Self {
        use core::sync::atomic::{AtomicI32, AtomicU32};
        Self {
            sent: AtomicI32::new(0),
            done: AtomicI32::new(0),
            address: AtomicU32::new(0),
            len: AtomicU32::new(0),
            settled: AtomicI32::new(0),
            wake: AtomicU32::new(0),
        }
    }

    /// Hand `batch` to the agent and wait until it has run it. Its bytes,
    /// and any its commands point at, are the program's again after.
    pub fn send(&self, batch: &[u8]) {
        use core::sync::atomic::Ordering::SeqCst;
        if batch.is_empty() {
            return;
        }
        self.address.store(batch.as_ptr() as usize as u32, SeqCst);
        self.len.store(batch.len() as u32, SeqCst);
        let sent = self.sent.fetch_add(1, SeqCst).wrapping_add(1);
        notify(&self.sent);
        loop {
            let done = self.done.load(SeqCst);
            if done == sent {
                return;
            }
            wait(&self.done, done);
        }
    }

    /// Wait until the agent has settled a reply beyond the `seen`th, and
    /// return how many it has settled.
    pub fn wait_settled(&self, seen: i32) -> i32 {
        use core::sync::atomic::Ordering::SeqCst;
        loop {
            let settled = self.settled.load(SeqCst);
            if settled != seen {
                return settled;
            }
            wait(&self.settled, seen);
        }
    }
}

/// Sleep while `word` holds `value`: the agent notifies it. On wasm with
/// atomics the including crate enables `stdarch_wasm_atomic_wait`.
#[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
fn wait(word: &core::sync::atomic::AtomicI32, value: i32) {
    unsafe { core::arch::wasm32::memory_atomic_wait32(word.as_ptr(), value, -1) };
}

#[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
fn notify(word: &core::sync::atomic::AtomicI32) {
    unsafe { core::arch::wasm32::memory_atomic_notify(word.as_ptr(), u32::MAX) };
}

// Without shared wasm memory no agent can serve the mailbox.
#[cfg(not(all(target_arch = "wasm32", target_feature = "atomics")))]
fn wait(_: &core::sync::atomic::AtomicI32, _: i32) {
    core::hint::spin_loop();
}

#[cfg(not(all(target_arch = "wasm32", target_feature = "atomics")))]
fn notify(_: &core::sync::atomic::AtomicI32) {}

/// A value as the wire encodes it.
pub trait Encode {
    fn encode(&self, e: &mut Encoder);
}

macro_rules! scalars {
    ($($t:ty),*) => {$(
        impl Encode for $t {
            fn encode(&self, e: &mut Encoder) {
                e.bytes.extend_from_slice(&self.to_le_bytes());
            }
        }
        impl Decode for $t {
            fn decode(d: &mut Decoder) -> Option<Self> {
                Some(<$t>::from_le_bytes(d.take(size_of::<$t>())?.try_into().ok()?))
            }
        }
    )*};
}
scalars!(i8, u8, i16, u16, i32, u32, i64, u64, f32, f64);

impl Encode for () {
    fn encode(&self, _: &mut Encoder) {}
}
impl Encode for bool {
    fn encode(&self, e: &mut Encoder) {
        e.u8(u8::from(*self));
    }
}
impl Encode for String {
    fn encode(&self, e: &mut Encoder) {
        e.u32(self.len() as u32);
        e.bytes.extend_from_slice(self.as_bytes());
    }
}
impl Encode for Handle {
    fn encode(&self, e: &mut Encoder) {
        e.u32(self.0);
    }
}
impl Encode for Bytes {
    fn encode(&self, e: &mut Encoder) {
        e.u32(self.address);
        e.u32(self.len);
    }
}
impl<T: Encode> Encode for Option<T> {
    fn encode(&self, e: &mut Encoder) {
        match self {
            Some(v) => {
                e.u8(1);
                v.encode(e);
            }
            None => e.u8(0),
        }
    }
}
impl<T: Encode> Encode for Vec<T> {
    fn encode(&self, e: &mut Encoder) {
        e.u32(self.len() as u32);
        for v in self {
            v.encode(e);
        }
    }
}
impl<T: Encode> Encode for (String, T) {
    fn encode(&self, e: &mut Encoder) {
        self.0.encode(e);
        self.1.encode(e);
    }
}

/// A reply's bytes, read back.
pub struct Decoder<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Decoder<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let out = self.bytes.get(self.at..self.at + n)?;
        self.at += n;
        Some(out)
    }
    pub fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    pub fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
}

/// A value read back from a reply.
pub trait Decode: Sized {
    fn decode(d: &mut Decoder) -> Option<Self>;
}

impl Decode for () {
    fn decode(_: &mut Decoder) -> Option<Self> {
        Some(())
    }
}
impl Decode for bool {
    fn decode(d: &mut Decoder) -> Option<Self> {
        Some(d.u8()? != 0)
    }
}
impl Decode for String {
    fn decode(d: &mut Decoder) -> Option<Self> {
        let n = d.u32()? as usize;
        String::from_utf8(d.take(n)?.to_vec()).ok()
    }
}
impl Decode for Handle {
    fn decode(d: &mut Decoder) -> Option<Self> {
        Some(Handle(d.u32()?))
    }
}
impl Decode for Bytes {
    fn decode(d: &mut Decoder) -> Option<Self> {
        Some(Bytes { address: d.u32()?, len: d.u32()? })
    }
}
impl<T: Decode> Decode for Option<T> {
    fn decode(d: &mut Decoder) -> Option<Self> {
        Some(if d.u8()? != 0 { Some(T::decode(d)?) } else { None })
    }
}
impl<T: Decode> Decode for Vec<T> {
    fn decode(d: &mut Decoder) -> Option<Self> {
        (0..d.u32()?).map(|_| T::decode(d)).collect()
    }
}
impl<T: Decode> Decode for (String, T) {
    fn decode(d: &mut Decoder) -> Option<Self> {
        Some((String::decode(d)?, T::decode(d)?))
    }
}
"#;

/// What every generated JavaScript half starts with.
const JS_PRELUDE: &str = r#"// Generated by caribou-bindgen from WebIDL: the agent's half of the wire.
// See caribou-bindgen's `wire` module for the encoding.

const utf8 = new TextDecoder();
const toUtf8 = new TextEncoder();

/// The objects the plugin names by handle, and the program's memory.
export class Wire {
  constructor(memory, objects) {
    this.memory = memory;
    this.objects = objects ?? new Map();
  }
  get(handle) {
    return handle === 0 ? undefined : this.objects.get(handle);
  }
  set(handle, object) {
    this.objects.set(handle, object);
  }
  /// Answer the reply record at `at`: its state, and the bytes, when the
  /// caller's buffer takes them.
  reply(at, state, bytes) {
    const buffer = this.memory.buffer;
    const view = new DataView(buffer);
    const n = bytes ? bytes.length : 0;
    const cap = view.getUint32(at + 12, true);
    view.setUint32(at + 4, n, true);
    if (n > cap) {
      state = 3;
    } else if (n > 0) {
      new Uint8Array(buffer, view.getUint32(at + 8, true), n).set(bytes);
    }
    const words = new Int32Array(buffer, 0, buffer.byteLength >> 2);
    Atomics.store(words, at >> 2, state);
    Atomics.notify(words, at >> 2);
    if (this.mailbox !== undefined) {
      Atomics.add(words, (this.mailbox >> 2) + 4, 1);
      Atomics.notify(words, (this.mailbox >> 2) + 4);
      const wake = Atomics.load(words, (this.mailbox >> 2) + 5) >>> 0;
      if (wake !== 0) {
        Atomics.add(words, wake >> 2, 1);
        Atomics.notify(words, wake >> 2);
      }
    }
  }
  /// A command that threw, which has no reply to carry it.
  error(e) {
    console.error(e);
  }
}

class Reader {
  constructor(wire, at, end) {
    this.wire = wire;
    this.buffer = wire.memory.buffer;
    this.view = new DataView(this.buffer);
    this.at = at;
    this.end = end;
  }
  i8() { const v = this.view.getInt8(this.at); this.at += 1; return v; }
  u8() { const v = this.view.getUint8(this.at); this.at += 1; return v; }
  i16() { const v = this.view.getInt16(this.at, true); this.at += 2; return v; }
  u16() { const v = this.view.getUint16(this.at, true); this.at += 2; return v; }
  i32() { const v = this.view.getInt32(this.at, true); this.at += 4; return v; }
  u32() { const v = this.view.getUint32(this.at, true); this.at += 4; return v; }
  i64() { const v = this.view.getBigInt64(this.at, true); this.at += 8; return Number(v); }
  u64() { const v = this.view.getBigUint64(this.at, true); this.at += 8; return Number(v); }
  f32() { const v = this.view.getFloat32(this.at, true); this.at += 4; return v; }
  f64() { const v = this.view.getFloat64(this.at, true); this.at += 8; return v; }
  str() {
    const n = this.u32();
    // TextDecoder takes no view of shared memory: the bytes are copied.
    const s = utf8.decode(new Uint8Array(this.buffer, this.at, n).slice());
    this.at += n;
    return s;
  }
  bytes() {
    const address = this.u32();
    const len = this.u32();
    return new Uint8Array(this.buffer, address, len);
  }
  object() { return this.wire.get(this.u32()); }
  seq(item) { const n = this.u32(); const out = []; for (let i = 0; i < n; i++) out.push(item()); return out; }
  record(value) { const n = this.u32(); const out = {}; for (let i = 0; i < n; i++) { const k = this.str(); out[k] = value(); } return out; }
}

class Writer {
  constructor() { this.bytes = []; }
  push(view, n) { for (let i = 0; i < n; i++) this.bytes.push(view.getUint8(i)); }
  num(n, set) { const view = new DataView(new ArrayBuffer(n)); set(view); this.push(view, n); }
  i8(v) { this.num(1, (d) => d.setInt8(0, v)); }
  u8(v) { this.num(1, (d) => d.setUint8(0, v)); }
  i16(v) { this.num(2, (d) => d.setInt16(0, v, true)); }
  u16(v) { this.num(2, (d) => d.setUint16(0, v, true)); }
  i32(v) { this.num(4, (d) => d.setInt32(0, v, true)); }
  u32(v) { this.num(4, (d) => d.setUint32(0, v, true)); }
  i64(v) { this.num(8, (d) => d.setBigInt64(0, BigInt(v), true)); }
  u64(v) { this.num(8, (d) => d.setBigUint64(0, BigInt(v), true)); }
  f32(v) { this.num(4, (d) => d.setFloat32(0, v, true)); }
  f64(v) { this.num(8, (d) => d.setFloat64(0, v, true)); }
  str(v) { const b = toUtf8.encode(v); this.u32(b.length); for (const x of b) this.bytes.push(x); }
  seq(v, item) { const items = Array.from(v); this.u32(items.length); for (const x of items) item(x); }
  record(v, value) { const keys = Object.keys(v); this.u32(keys.length); for (const k of keys) { this.str(k); value(v[k]); } }
}

function encode(write) {
  const w = new Writer();
  write(w);
  return Uint8Array.from(w.bytes);
}

function message(e) {
  return toUtf8.encode(String(e && e.message !== undefined ? e.message : e));
}

/// Run the commands in the program's memory from `at` to `at + len`.
export function execute(wire, at, len) {
  const end = at + len;
  while (at < end) {
    const view = new DataView(wire.memory.buffer);
    const op = view.getUint32(at, true);
    const n = view.getUint32(at + 4, true);
    const r = new Reader(wire, at + 8, at + 8 + n);
    try {
      OPS[op](wire, r);
    } catch (e) {
      wire.error(e);
    }
    at += 8 + n;
  }
}

/// Serve, from this worker, the program its starter posts it: the
/// program's memory and its mailbox's address, and whatever else the
/// starter gives. `roots` makes, from that message, the objects the plugin
/// names by the first handles; `setup`, if given, then prepares the wire.
export function start(roots, setup) {
  const first = async ({ data }) => {
    if (data.memory === undefined) return;
    self.removeEventListener("message", first);
    const wire = new Wire(data.memory, await roots(data));
    if (setup) setup(wire);
    serve(wire, data.address);
  };
  self.addEventListener("message", first);
}

/// Serve the program's mailbox at `address`: run each batch it hands
/// over, then tell it the batch is done. The wait does not block, so the
/// API's promises settle between batches.
export async function serve(wire, address) {
  wire.mailbox = address;
  let seen = 0;
  for (;;) {
    const words = new Int32Array(wire.memory.buffer, address, 6);
    const waited = Atomics.waitAsync(words, 0, seen);
    if (waited.async) await waited.value;
    const sent = Atomics.load(words, 0);
    if (sent === seen) continue;
    execute(wire, words[2] >>> 0, words[3] >>> 0);
    // What the plugin's agent waits for before the batch counts as run,
    // such as the frame it drew reaching the page.
    if (wire.after) await wire.after();
    seen = sent;
    Atomics.store(words, 1, seen);
    Atomics.notify(words, 1);
  }
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_rust_and_the_idl() {
        assert_eq!(snake("createBindGroupLayout"), "create_bind_group_layout");
        assert_eq!(snake("GPUDevice"), "gpu_device");
        assert_eq!(snake("maxTextureDimension1D"), "max_texture_dimension1d");
        assert_eq!(variant("high-performance"), "HighPerformance");
        assert_eq!(variant("2d-array"), "V2dArray");
    }

    #[test]
    fn the_window_plugins_browser_apis_parse_and_generate() {
        let idl = include_str!("../../../plugins/cb_window/spec/window.idl");
        let model = crate::idl::parse(idl).unwrap();
        // Partials from other specifications joined their bases.
        let element = model
            .interfaces
            .iter()
            .find(|i| i.name == "Element")
            .unwrap();
        assert!(
            element
                .operations
                .iter()
                .any(|o| o.name == "requestFullscreen")
        );
        assert!(
            element
                .operations
                .iter()
                .any(|o| o.name == "requestPointerLock")
        );
        let wire = wire(idl).unwrap();
        assert!(
            wire.rust.contains(
                "pub fn offscreen_canvas_set_width(&mut self, this: Handle, value: &u64)"
            )
        );
        assert!(wire.js.contains("(self.title = a0)"));
    }

    #[test]
    fn the_webgpu_wire_generates() {
        let wire = wire(include_str!("../../../plugins/cb_gpu/spec/webgpu.idl")).unwrap();
        assert!(wire.rust.contains("pub fn gpu_device_create_buffer(&mut self, this: Handle, result: Handle, descriptor: &GPUBufferDescriptor)"));
        assert!(wire.rust.contains("pub fn gpu_request_adapter(&mut self, this: Handle, result: Handle, reply: u32, options: &Option<GPURequestAdapterOptions>)"));
        assert!(wire.js.contains("self.createBuffer(a0)"));
        assert!(wire.js.contains("self.requestAdapter(a0).then("));
    }
}
