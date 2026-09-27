//! The mailbox between a program and its agent, on the threads they run on
//! in a page: the program is wasm over shared memory, blocking on its main
//! thread while the agent, a worker, serves its batches and settles its
//! replies. Needs `rustc` with the `wasm32-wasip1-threads` target, and
//! `node`.

use std::path::Path;
use std::process::Command;

/// A session as a program runs one: each batch handed over and waited for,
/// each promise's reply waited for through the settled count.
const PROGRAM: &str = r#"
#![cfg_attr(all(target_arch = "wasm32", target_feature = "atomics"), feature(stdarch_wasm_atomic_wait))]
#[allow(dead_code, non_camel_case_types, clippy::all)]
mod wire {
    include!("gpu_wire.rs");
}
use std::sync::atomic::{AtomicI32, Ordering::SeqCst};
use wire::*;

static MAILBOX: Mailbox = Mailbox::new();

#[repr(C)]
struct Reply {
    state: AtomicI32,
    len: u32,
    address: u32,
    cap: u32,
}

impl Reply {
    fn new(buffer: &mut [u8]) -> Self {
        Reply { state: AtomicI32::new(0), len: 0, address: buffer.as_mut_ptr() as u32, cap: buffer.len() as u32 }
    }
    fn at(&self) -> u32 {
        self as *const Reply as u32
    }
    fn settle(&self, seen: &mut i32) -> i32 {
        while self.state.load(SeqCst) == 0 {
            *seen = MAILBOX.wait_settled(*seen);
        }
        self.state.load(SeqCst)
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn mailbox() -> u32 {
    &MAILBOX as *const Mailbox as u32
}

/// The replies' states and the buffer's size, as decimal digits.
#[unsafe(no_mangle)]
pub extern "C" fn session() -> u64 {
    let mut seen = 0;
    let mut e = Encoder::new();
    let (mut a, mut b, mut c, mut d) = ([0u8; 16], [0u8; 16], [0u8; 16], [0u8; 64]);
    let (adapter, device, size, range) = (Reply::new(&mut a), Reply::new(&mut b), Reply::new(&mut c), Reply::new(&mut d));

    e.gpu_request_adapter(Handle(1), Handle(2), adapter.at(), &None);
    MAILBOX.send(&e.bytes);
    e.bytes.clear();
    let adapter = adapter.settle(&mut seen);

    e.gpu_adapter_request_device(Handle(2), Handle(3), device.at(), &None);
    MAILBOX.send(&e.bytes);
    e.bytes.clear();
    let device = device.settle(&mut seen);

    let upload = [1u8, 2, 3, 4];
    e.gpu_device_get_queue(Handle(3), Handle(4));
    e.gpu_device_create_buffer(
        Handle(3),
        Handle(5),
        &GPUBufferDescriptor { label: None, size: 1024, usage: 0x48, mapped_at_creation: None },
    );
    e.gpu_queue_write_buffer(Handle(4), &Handle(5), &0, &Bytes { address: upload.as_ptr() as u32, len: 4 }, &None, &None);
    // Answered as they run, so settled once the batch is done.
    e.gpu_buffer_get_size(Handle(5), size.at());
    e.gpu_buffer_get_mapped_range(Handle(5), range.at(), &None, &None);
    e.release(Handle(2));
    MAILBOX.send(&e.bytes);
    let bytes = c;
    let size_value = u64::from_le_bytes(bytes[..8].try_into().unwrap());
    let states = [adapter, device, size.state.load(SeqCst), range.state.load(SeqCst)];
    states.iter().fold(0u64, |n, &s| n * 10 + s as u64) * 10_000 + size_value
}
"#;

/// The page's side: the program's thread, and a worker holding a GPU that
/// records its calls. `size` answers, `getMappedRange` throws, and the rest
/// make objects that record in turn.
const HARNESS: &str = r#"
import { readFileSync } from "node:fs";
import { once } from "node:events";
import { Worker } from "node:worker_threads";

const module = await WebAssembly.compile(readFileSync("program.wasm"));
const memory = new WebAssembly.Memory({ initial: 32, maximum: 32, shared: true });
const wasi = new Proxy({}, { get: () => () => 0 });
const instance = await WebAssembly.instantiate(module, {
  env: { memory },
  wasi_snapshot_preview1: wasi,
  wasi: { "thread-spawn": () => -1 },
});
instance.exports._initialize?.();

const agent = new Worker(new URL("./agent.mjs", import.meta.url), {
  workerData: { memory, address: instance.exports.mailbox() },
});
await once(agent, "message");
const result = instance.exports.session();
agent.postMessage("report");
const [report] = await once(agent, "message");
await agent.terminate();
console.log(JSON.stringify({ result: String(result), ...report }));
"#;

const AGENT: &str = r#"
import { parentPort, workerData } from "node:worker_threads";
import { Wire, serve } from "./gpu_agent.mjs";

const log = [];
const ASYNC = new Set(["requestAdapter", "requestDevice"]);
function object(name) {
  return new Proxy({ name }, {
    get(target, key) {
      if (key === "name") return name;
      if (key === "then") return undefined;
      if (key === "queue") return object(`${name}.queue`);
      if (key === "size") return 1024;
      if (key === "getMappedRange") return () => { throw new Error("not mapped"); };
      return (...args) => {
        log.push([name, key, args.map((v) => (v instanceof Uint8Array ? Array.from(v) : v && v.name ? `@${v.name}` : v))]);
        const made = object(`${name}.${key}`);
        return ASYNC.has(key) ? Promise.resolve(made) : made;
      };
    },
  });
}

const wire = new Wire(workerData.memory, new Map([[1, object("gpu")]]));
parentPort.on("message", () => parentPort.postMessage({ log, handles: [...wire.objects.keys()] }));
parentPort.postMessage("ready");
serve(wire, workerData.address);
"#;

#[test]
fn a_program_hands_batches_to_its_agent_and_waits_for_replies() {
    let idl = include_str!("../../../plugins/cb_gpu/spec/webgpu.idl");
    let wire = caribou_bindgen::wire::wire(idl).unwrap();
    let dir = std::env::temp_dir().join(format!("caribou-mailbox-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (name, text) in [
        ("gpu_wire.rs", wire.rust.as_str()),
        ("gpu_agent.mjs", &wire.js),
        ("program.rs", PROGRAM),
        ("harness.mjs", HARNESS),
        ("agent.mjs", AGENT),
    ] {
        std::fs::write(dir.join(name), text).unwrap();
    }

    run(
        &dir,
        Command::new("rustc").args([
            "--edition",
            "2024",
            "-O",
            "--target",
            "wasm32-wasip1-threads",
            "--crate-type",
            "cdylib",
            "-Clink-arg=--initial-memory=2097152",
            "-Clink-arg=--max-memory=2097152",
            "program.rs",
            "-o",
            "program.wasm",
        ]),
    );
    let out = run(&dir, Command::new("node").arg("harness.mjs"));
    std::fs::remove_dir_all(&dir).ok();

    // Both promises resolved (1), the size answered (1) and the mapped
    // range rejected (2); the size is 1024.
    assert_eq!(
        out.trim(),
        r#"{"result":"11121024","log":[["gpu","requestAdapter",[null]],["gpu.requestAdapter","requestDevice",[null]],["gpu.requestAdapter.requestDevice","createBuffer",[{"size":1024,"usage":72}]],["gpu.requestAdapter.requestDevice.queue","writeBuffer",["@gpu.requestAdapter.requestDevice.createBuffer",0,[1,2,3,4],null,null]]],"handles":[1,3,4,5]}"#
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
