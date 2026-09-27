//! Both halves of the WebGPU wire, end to end: the generated Rust compiles
//! and encodes calls; the generated JavaScript, under Node, decodes them
//! and makes the calls on a GPU that records them, answering replies.
//! Needs `rustc` and `node` on the path.

use std::path::Path;
use std::process::Command;

/// Encodes a small session's commands, and writes them after the reply
/// records and the bytes they point at, laid out as the program's memory.
const PROGRAM: &str = r#"
#[allow(dead_code, non_camel_case_types, clippy::all)]
mod wire {
    include!("gpu_wire.rs");
}
use wire::*;

fn main() {
    // Memory: two reply records at 0 and 16, their buffers at 64 and 96,
    // upload bytes at 128, commands from 256.
    let mut memory = vec![0u8; 256];
    for (record, buffer) in [(0usize, 64u32), (16, 96)] {
        memory[record + 8..record + 12].copy_from_slice(&buffer.to_le_bytes());
        memory[record + 12..record + 16].copy_from_slice(&32u32.to_le_bytes());
    }
    memory[128..132].copy_from_slice(&[1, 2, 3, 4]);
    // Three batches, as a program sends them: each after the promise the
    // one before made has settled.
    let mut batches = Vec::new();
    let mut e = Encoder::new();
    e.gpu_request_adapter(Handle(1), Handle(2), 0, &None);
    batches.push(std::mem::take(&mut e.bytes));
    e.gpu_adapter_request_device(Handle(2), Handle(3), 16, &None);
    batches.push(std::mem::take(&mut e.bytes));
    e.gpu_device_get_queue(Handle(3), Handle(4));
    e.gpu_device_create_buffer(
        Handle(3),
        Handle(5),
        &GPUBufferDescriptor { label: Some("uniforms".into()), size: 1024, usage: 0x48, mapped_at_creation: None },
    );
    e.gpu_queue_write_buffer(Handle(4), &Handle(5), &0, &Bytes { address: 128, len: 4 }, &None, &None);
    batches.push(std::mem::take(&mut e.bytes));
    let mut layout = Vec::new();
    for batch in batches {
        layout.push(format!("{} {}", memory.len(), batch.len()));
        memory.extend_from_slice(&batch);
    }
    std::fs::write("memory.bin", &memory).unwrap();
    println!("{}", layout.join(" "));
}
"#;

/// A GPU whose objects record every call made on them, promises
/// resolving at once, and the run printed once they have.
const HARNESS: &str = r#"
import { readFileSync } from "node:fs";
import { Wire, execute } from "./gpu_agent.mjs";

const bytes = readFileSync("memory.bin");
const buffer = new SharedArrayBuffer(bytes.length);
new Uint8Array(buffer).set(bytes);
const layout = process.argv.slice(2).map(Number);

const log = [];
const ASYNC = new Set(["requestAdapter", "requestDevice", "mapAsync", "onSubmittedWorkDone"]);
function object(name) {
  return new Proxy({ name }, {
    get(target, key) {
      if (key === "name") return name;
      if (key === "then") return undefined;
      if (key === "queue") return object(`${name}.queue`);
      return (...args) => {
        log.push([name, key, args.map(describe)]);
        const made = object(`${name}.${key}`);
        return ASYNC.has(key) ? Promise.resolve(made) : made;
      };
    },
  });
}
function describe(v) {
  if (v instanceof Uint8Array) return Array.from(v);
  if (v && typeof v === "object" && "name" in v) return `@${v.name}`;
  return v;
}

const wire = new Wire({ buffer }, new Map([[1, object("gpu")]]));
for (let i = 0; i < layout.length; i += 2) {
  execute(wire, layout[i], layout[i + 1]);
  await new Promise((r) => setTimeout(r, 0));
}
const state = new Int32Array(buffer, 0, buffer.byteLength >> 2);
console.log(JSON.stringify({ log, replies: [state[0], state[4]], handles: [...wire.objects.keys()] }));
"#;

#[test]
fn rust_encodes_and_javascript_decodes_the_webgpu_wire() {
    let idl = include_str!("../../../plugins/cb_gpu/spec/webgpu.idl");
    let wire = caribou_bindgen::wire::wire(idl).unwrap();
    let dir = std::env::temp_dir().join(format!("caribou-wire-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("gpu_wire.rs"), &wire.rust).unwrap();
    std::fs::write(dir.join("gpu_agent.mjs"), &wire.js).unwrap();
    std::fs::write(dir.join("main.rs"), PROGRAM).unwrap();
    std::fs::write(dir.join("harness.mjs"), HARNESS).unwrap();

    run(
        &dir,
        Command::new("rustc").args(["--edition", "2024", "-O", "main.rs", "-o", "encode"]),
    );
    let layout = run(&dir, &mut Command::new(dir.join("encode")));
    let mut command = Command::new("node");
    command.arg("harness.mjs").args(layout.split_whitespace());
    let out = run(&dir, &mut command);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        out.trim(),
        r#"{"log":[["gpu","requestAdapter",[null]],["gpu.requestAdapter","requestDevice",[null]],["gpu.requestAdapter.requestDevice","createBuffer",[{"label":"uniforms","size":1024,"usage":72}]],["gpu.requestAdapter.requestDevice.queue","writeBuffer",["@gpu.requestAdapter.requestDevice.createBuffer",0,[1,2,3,4],null,null]]],"replies":[1,1],"handles":[1,2,3,4,5]}"#
    );
}

fn run(dir: &Path, command: &mut Command) -> String {
    let out = command.current_dir(dir).output().expect("the tool runs");
    assert!(
        out.status.success(),
        "{:?}: {}{}",
        command,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}
