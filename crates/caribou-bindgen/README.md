# caribou-bindgen

Build-time generation of Caribou plugin wrappers and exports. See
[the GPU plugin](../../plugins/cb_gpu/README.md) for a working consumer.

`generate(namespace, declaration, webidl)` returns Rust source to include
from `OUT_DIR`. The namespace is a parameter, independent of the backend's
crate name. Resource traits contain signatures annotated with
`#[native(function)]`; enum and constant declarations can import a named
WebIDL declaration using `#[idl("Name")]`.

Resource methods use explicit `this: &Resource` parameters. Supported carriers
are Caribou `Text`, `Buffer`, `Future<T>`, `Enum<T>`, `Box<Resource>` results and
borrowed resource parameters, plus numeric/boolean scalars. Backends receive integer
resource handles and declared native enum codes. Generated objects use the
normal `plugin!` class metadata and finalizers; the backend defines explicit
native resource lifetime operations.

`Future<T>` is the shared eventual-result carrier. A backend creates one with
`Future::new()`, roots it as `Rooted<Future<T>>` while work is outstanding, and
calls `resolve(Value)`, `resolve_boxed(Box<T>)`, or `reject(Value)` from its
completion callback. The core wakes every waiting language fiber. Annotating a resource method with
`#[idl("Interface.operation")]` imports its WebIDL return contract;
`Promise<T>` is exposed as the shared typed `Future<T>` carrier. A declaration
still chooses the plugin method name, arguments and native backend function.

The WebIDL reader extracts enum strings, numeric constant namespaces,
dictionary members and operation return contracts. It
is deliberately not a general WebIDL-to-Rust interface translator: an IDL
interface cannot specify how a Rust backend owns GPU resources or implements
asynchronous operations. Those choices belong in the resource declaration
and backend.
