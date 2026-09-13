//! A Haxe program on the core: ash's interpreter, with its Cranelift tier
//! in hybrid mode, loaded from a `.hl` and driven from here. What ash's CLI
//! does for `--mode interp` and `--mode hybrid`, with the seam installed
//! first, and what the runner and the tests share.
//!
//! `load` installs the seam, initialises ash's standard library, decodes the
//! bytecode and builds the interpreter. `start` runs the program's entry
//! point: HashLink's entry function creates every class object and runs
//! the static initialisers before `main`, and ash registers its closure
//! runner, stub resolver and exception hooks only there, so nothing in the
//! program can be called from outside before it. `publish` then describes
//! each class from the decoded bytecode and publishes it to the registry.
//!
//! Every published callable is `Callable::Typed`: the interpreter's function
//! pointer for the bytecode function (a stub sentinel that `hlp_dyn_call`
//! routes to the closure runner, or the compiled entry once the tier has
//! promoted it) and the interpreter's own `hl_type` for it, both read from
//! the module context the interpreter built.

use std::collections::HashMap;
use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use ash_core::bytecode::{BytecodeDecoder, DecodedBytecode};
use ash_core::native_lib::{self, NativeFunctionResolver};
use ash_core::types::{HLFunction, HLType, HLTypeObj, TypeRef as HlRef};
use ash_interp::interpreter::{HLInterpreter, TierMode, TierPreset, TieredConfig};
use ash_interp::values::NanBoxedValue;
use ash_std::bytes::hlp_alloc_bytes;
use caribou::protocol::Callable;
use caribou::registry::{self, ClassIface, FieldIface, Interface, MethodIface, TypeRef};
use caribou_abi::hl::{self, hl_module_context, hl_type, vdynamic};

use crate::proto;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Mode {
    Interp,
    #[default]
    Hybrid,
}

/// What `load` takes.
#[derive(Clone, Debug)]
pub struct Options {
    pub mode: Mode,
    /// Install the core's heap and scheduler; `false` runs the program on
    /// ash's own, for comparison.
    pub install: bool,
    /// The program's own arguments.
    pub args: Vec<String>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            mode: Mode::Hybrid,
            install: true,
            args: Vec::new(),
        }
    }
}

/// A loaded program. The boxed parts stay where they are, and a started
/// program's are never freed: the interpreter's closure runner keeps raw
/// pointers to them, and so does every callable it published.
pub struct Program {
    path: PathBuf,
    mode: Mode,
    bytecode: ManuallyDrop<Arc<DecodedBytecode>>,
    resolver: ManuallyDrop<Box<NativeFunctionResolver>>,
    interpreter: ManuallyDrop<Box<HLInterpreter>>,
    started: bool,
}

impl Drop for Program {
    fn drop(&mut self) {
        if self.started {
            return;
        }
        unsafe {
            ManuallyDrop::drop(&mut self.interpreter);
            ManuallyDrop::drop(&mut self.resolver);
            ManuallyDrop::drop(&mut self.bytecode);
        }
    }
}

/// A program beside HDLLs makes ash dlopen the libhl beside the executable
/// and initialise that image's heap in the same call, so the image is
/// opened here first, staged beside the executable if it is not there, and
/// given the table before `init_std_library` sees it.
#[cfg(target_os = "macos")]
fn install_into_sibling_runtime() -> Result<()> {
    use std::ffi::CString;

    let exe = std::env::current_exe()?;
    let path = exe
        .parent()
        .ok_or_else(|| anyhow!("{} has no directory", exe.display()))?
        .join("libhl.dylib");
    if !path.exists() {
        native_lib::write_embedded_runtime(&path)
            .with_context(|| format!("staging {}", path.display()))?;
    }
    let c_path = CString::new(path.to_string_lossy().as_bytes())?;
    // Never closed: ash opens the same image next and keeps it for the
    // process.
    let handle = unsafe { libc::dlopen(c_path.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) };
    if handle.is_null() {
        let err = unsafe { std::ffi::CStr::from_ptr(libc::dlerror()) };
        bail!("dlopen {}: {}", path.display(), err.to_string_lossy());
    }
    let lookup = |name: &str| {
        let name = CString::new(name).ok()?;
        let sym = unsafe { libc::dlsym(handle, name.as_ptr()) };
        (!sym.is_null()).then_some(sym as usize)
    };
    let seam = unsafe { crate::Seam::from_lookup(lookup) }?;
    crate::install_into(seam)?;
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn install_into_sibling_runtime() -> Result<()> {
    bail!("the seam installs into a dlopened ash_std on macOS only");
}

/// Hand the program its argv, the way ash's CLI does before any mode runs.
fn sys_init(file: &Path, program_args: &[String]) -> Result<()> {
    let addr = native_lib::std_symbol_addr("hlp_sys_init")
        .ok_or_else(|| anyhow!("hlp_sys_init not found in ash_std"))?;
    type SysInit = unsafe extern "C" fn(*mut *mut u8, i32, *mut u8);
    let sys_init: SysInit = unsafe { std::mem::transmute(addr) };
    // NUL-terminated UTF-8; hlp_sys_init copies everything out.
    let mut bufs: Vec<Vec<u8>> = program_args
        .iter()
        .map(|a| {
            let mut b = a.as_bytes().to_vec();
            b.push(0);
            b
        })
        .collect();
    let mut ptrs: Vec<*mut u8> = bufs.iter_mut().map(|b| b.as_mut_ptr()).collect();
    let mut file_buf = file.to_string_lossy().as_bytes().to_vec();
    file_buf.push(0);
    unsafe { sys_init(ptrs.as_mut_ptr(), ptrs.len() as i32, file_buf.as_mut_ptr()) };
    Ok(())
}

/// Load `path`. Before ash's heap exists: `init_std_library` creates it.
pub fn load(path: &Path, options: Options) -> Result<Program> {
    if !path.exists() {
        bail!("Bytecode file not found: {}", path.display());
    }
    let static_std = native_lib::choose_std_linkage(path);
    if options.install {
        if static_std {
            crate::install()?;
        } else {
            install_into_sibling_runtime()?;
        }
    }
    native_lib::init_std_library()?;

    // The image ash will run through is the one that had to take the table.
    let installed_addr = native_lib::std_symbol_addr("hlp_rt_installed")
        .ok_or_else(|| anyhow!("hlp_rt_installed not found in ash_std"))?;
    let installed: unsafe extern "C" fn() -> bool = unsafe { std::mem::transmute(installed_addr) };
    if unsafe { installed() } != options.install {
        bail!(
            "the hosted ash_std {} the runtime table",
            if options.install {
                "did not take"
            } else {
                "already has"
            }
        );
    }

    sys_init(path, &options.args)?;

    let bytecode = Arc::new(BytecodeDecoder::decode(path)?);
    // The bridge's own natives bind first, by name, so no library is
    // looked for under them.
    let bridged = crate::import::bind(&bytecode)?;
    let mut resolver = Box::new(NativeFunctionResolver::new().with_host_natives(&bridged));
    let search_dir = path.parent().unwrap_or_else(|| Path::new("."));
    resolver.discover_and_load_libraries(search_dir, &bytecode.natives, true)?;
    let mut interpreter = Box::new(HLInterpreter::new(&bytecode, &resolver));
    crate::import::attach_types(&bytecode, &interpreter)?;
    if options.mode == Mode::Hybrid {
        // ash's CLI defaults: the Application preset, tier from ASH_TIER.
        let tier_mode = match std::env::var("ASH_TIER") {
            Ok(spec) => {
                TierMode::parse(&spec).ok_or_else(|| anyhow!("invalid ASH_TIER value {spec:?}"))?
            }
            Err(_) => TierMode::default(),
        };
        let cfg = TieredConfig {
            tier_mode,
            ..TierPreset::Application.to_config()
        };
        interpreter.enable_tiered(path, &resolver, &bytecode, cfg)?;
    }
    Ok(Program {
        path: path.to_owned(),
        mode: options.mode,
        bytecode: ManuallyDrop::new(bytecode),
        resolver: ManuallyDrop::new(resolver),
        interpreter: ManuallyDrop::new(interpreter),
        started: false,
    })
}

impl Program {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn bytecode(&self) -> &DecodedBytecode {
        &self.bytecode
    }

    /// The modules the program imports from other languages, as
    /// `(namespace, module)` pairs, read from its natives: what a driver
    /// configures its world from.
    pub fn imports(&self) -> Result<Vec<(String, String)>> {
        let mut out: Vec<(String, String)> = Vec::new();
        for native in self
            .bytecode
            .natives
            .iter()
            .filter(|n| n.lib == crate::import::LIB)
        {
            let (namespace, module, _, _) =
                crate::import::parse(&native.name).ok_or_else(|| {
                    anyhow!(
                        "`{}` does not name a member of a published class",
                        native.name
                    )
                })?;
            let pair = (namespace, module);
            if !out.contains(&pair) {
                out.push(pair);
            }
        }
        Ok(out)
    }

    /// Run the entry point: the static initialisers, then `main`, then the
    /// event loop if the program installed one.
    pub fn start(&mut self) -> Result<()> {
        self.started = true;
        self.interpreter
            .execute_entrypoint(&self.bytecode, &self.resolver)?;
        Ok(())
    }

    /// Publish every class of the program. What is published can be called
    /// only once the interpreter has registered its closure runner, which
    /// it does as the program starts, before its entry function runs; so a
    /// program may publish before `start`, and another language reaches
    /// its classes from the moment `main` runs.
    pub fn publish(&mut self) -> Result<Vec<Arc<Interface>>> {
        let bytecode: Arc<DecodedBytecode> = Arc::clone(&self.bytecode);
        publish_module(&bytecode, self)
    }

    /// Let a tier-chase thread finish what it holds, then let the program
    /// go; what it published stays callable.
    pub fn finish(self) {
        if self.mode == Mode::Hybrid {
            self.interpreter.quiesce_promotions();
        }
    }

    /// The interpreter's module context: its function pointer and function
    /// type per findex. The interpreter keeps it private, so it is read off
    /// the type of a `String` the program allocates through its own
    /// `String.__alloc__`, which every HashLink program has. Also records
    /// that type for the strings that cross.
    fn module_context(&mut self) -> Result<*mut hl_module_context> {
        let types = &self.bytecode.types;
        let (index, companion) = types
            .iter()
            .enumerate()
            .find_map(|(i, t)| {
                let obj = t.obj.as_ref()?;
                (obj.name == "$String").then_some((i, obj))
            })
            .ok_or_else(|| anyhow!("the program has no String class"))?;
        let flat = flat_fields(types, index);
        let alloc = bindings(companion)
            .find(|&(fid, _)| flat.get(fid) == Some(&"__alloc__"))
            .map(|(_, findex)| findex)
            .ok_or_else(|| anyhow!("String.__alloc__ is not in the program"))?;
        let empty = unsafe { hlp_alloc_bytes(2) };
        unsafe { *(empty as *mut u16) = 0 };
        let s = self.interpreter.call_function(
            &self.bytecode,
            &self.resolver,
            alloc as usize,
            &[
                NanBoxedValue::from_bytes_ptr(empty as usize),
                NanBoxedValue::from_i32(0),
            ],
        )?;
        let s = s.as_ptr() as *mut vdynamic;
        if s.is_null() {
            bail!("String.__alloc__ returned null");
        }
        let t = unsafe { (*s).t };
        proto::set_string_type(t);
        let m = unsafe { (*(*t).detail.obj).m };
        if m.is_null() {
            bail!("the String type carries no module context");
        }
        Ok(m)
    }
}

// ---------------------------------------------------------------------------
// Publishing
// ---------------------------------------------------------------------------

/// The registry's type for a bytecode type.
fn type_ref(types: &[HLType], t: &HlRef) -> TypeRef {
    let Some(ty) = types.get(t.0) else {
        return TypeRef::Dyn;
    };
    match ty.kind {
        hl::HVOID => TypeRef::Void,
        hl::HUI8 | hl::HUI16 | hl::HI32 | hl::HI64 => TypeRef::Int,
        hl::HF32 | hl::HF64 => TypeRef::Float,
        hl::HBOOL => TypeRef::Bool,
        hl::HFUN | hl::HMETHOD => TypeRef::Fun,
        hl::HARRAY => TypeRef::Array(Box::new(TypeRef::Dyn)),
        hl::HNULL => ty
            .tparam
            .as_ref()
            .map_or(TypeRef::Dyn, |inner| type_ref(types, inner)),
        hl::HOBJ | hl::HSTRUCT => match &ty.obj {
            Some(obj) if obj.name == "String" => TypeRef::Str,
            Some(obj) => TypeRef::Object(obj.name.clone()),
            None => TypeRef::Dyn,
        },
        _ => TypeRef::Dyn,
    }
}

/// Whether a type is published: not the runtime's own, not a companion,
/// and not `String`, which crosses as a value.
fn publishable(name: &str) -> bool {
    !(name.starts_with("hl.")
        || name.starts_with("haxe.")
        || name.contains('$')
        || name == "String")
}

/// The companion's name: `game.$Player` for `game.Player`.
fn companion_name(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((package, class)) => format!("{package}.${class}"),
        None => format!("${name}"),
    }
}

/// One type's object descriptors along its superclass chain, the subclass
/// first so an override wins.
fn chain(types: &[HLType], index: usize) -> Vec<&HLTypeObj> {
    let mut out = Vec::new();
    let mut next = Some(index);
    while let Some(t) = next {
        let Some(obj) = types.get(t).and_then(|ty| ty.obj.as_ref()) else {
            break;
        };
        out.push(obj);
        next = obj.super_.as_ref().map(|s| s.0);
    }
    out
}

/// A companion's bindings: pairs of field index, counted through the
/// inherited fields, and function index.
fn bindings(obj: &HLTypeObj) -> impl Iterator<Item = (usize, i32)> + '_ {
    obj.bindings
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| (pair[0] as usize, pair[1]))
}

/// Every field of a type, inherited first: what a binding's field index
/// counts through.
fn flat_fields(types: &[HLType], index: usize) -> Vec<&str> {
    chain(types, index)
        .iter()
        .rev()
        .flat_map(|obj| obj.fields.iter().map(|f| f.name.as_str()))
        .collect()
}

/// Publish every publishable class of `bytecode` as one module each,
/// named after the class: `game.Player` holds `Player`.
///
/// HashLink's shape: an instance type carries the fields and the instance
/// methods as protos; its companion type (`game.$Player`, an `hl.Class`)
/// carries the statics as function-typed fields, bound by its binding list
/// to their functions, and binds the inherited `__constructor__` field to
/// the constructor. A binding is a pair of field index, counted through the
/// inherited fields, and function index.
pub fn publish_module(
    bytecode: &DecodedBytecode,
    program: &mut Program,
) -> Result<Vec<Arc<Interface>>> {
    let m = program.module_context()?;
    let types = &bytecode.types;
    let functions: HashMap<i32, &HLFunction> =
        bytecode.functions.iter().map(|f| (f.findex, f)).collect();
    let by_name: HashMap<&str, usize> = types
        .iter()
        .enumerate()
        .filter_map(|(i, t)| Some((t.obj.as_ref()?.name.as_str(), i)))
        .collect();
    let lang = proto::lang();
    let signature = |findex: i32, skip_this: bool| -> Option<(Vec<TypeRef>, TypeRef)> {
        let f = functions.get(&findex)?;
        let fun = types.get(f.type_.0)?.fun.as_ref()?;
        let args = fun.args.iter().skip(usize::from(skip_this));
        Some((
            args.map(|a| type_ref(types, a)).collect(),
            type_ref(types, &fun.ret),
        ))
    };
    let target = |findex: i32| -> Option<Callable> {
        functions.contains_key(&findex).then(|| Callable::Typed {
            func: unsafe { *(*m).functions_ptrs.add(findex as usize) },
            signature: unsafe { *(*m).functions_types.add(findex as usize) },
            lang,
        })
    };
    // The instance type: the receiver of any function taking `this`.
    let this_type = |findex: i32| -> Option<*mut hl_type> {
        let sig = unsafe { *(*m).functions_types.add(findex as usize) };
        let fun = unsafe { (*sig).detail.fun.as_ref()? };
        (fun.nargs > 0).then(|| unsafe { *fun.args })
    };

    let mut published = Vec::new();
    for (index, ty) in types.iter().enumerate() {
        if ty.kind != hl::HOBJ {
            continue;
        }
        let Some(obj) = ty.obj.as_ref() else {
            continue;
        };
        if !publishable(&obj.name) {
            continue;
        }

        let mut fields: Vec<FieldIface> = Vec::new();
        let mut methods: Vec<MethodIface> = Vec::new();
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let mut instance_t: Option<*mut hl_type> = None;
        for level in chain(types, index) {
            for field in &level.fields {
                if !field.name.starts_with("__") && seen.insert(&field.name) {
                    fields.push(FieldIface {
                        name: field.name.clone(),
                        ty: type_ref(types, &field.type_),
                    });
                }
            }
            for p in &level.proto {
                if !seen.insert(&p.name) {
                    continue;
                }
                let (Some((params, ret)), Some(target)) =
                    (signature(p.findex, true), target(p.findex))
                else {
                    continue;
                };
                if std::ptr::eq(level, obj) && instance_t.is_none() {
                    instance_t = this_type(p.findex);
                }
                methods.push(MethodIface {
                    name: p.name.clone(),
                    is_static: false,
                    params,
                    ret,
                    target,
                });
            }
        }

        // Statics and the constructor, through the companion's bindings;
        // its own unbound fields are the class's static fields.
        let mut ctor = None;
        let mut statics: Vec<FieldIface> = Vec::new();
        if let Some(&ci) = by_name.get(companion_name(&obj.name).as_str())
            && let Some(companion) = types[ci].obj.as_ref()
        {
            let flat = flat_fields(types, ci);
            let inherited = flat.len() - companion.fields.len();
            let bound: Vec<usize> = bindings(companion).map(|(fid, _)| fid).collect();
            for (i, field) in companion.fields.iter().enumerate() {
                if !bound.contains(&(inherited + i)) && !field.name.starts_with("__") {
                    statics.push(FieldIface {
                        name: field.name.clone(),
                        ty: type_ref(types, &field.type_),
                    });
                }
            }
            for (fid, findex) in bindings(companion) {
                let Some(&name) = flat.get(fid) else {
                    continue;
                };
                let Some(target) = target(findex) else {
                    continue;
                };
                if name == "__constructor__" {
                    let (Some((params, _)), Some(t)) = (signature(findex, true), this_type(findex))
                    else {
                        continue;
                    };
                    let Callable::Typed {
                        func, signature, ..
                    } = target
                    else {
                        continue;
                    };
                    ctor = Some(MethodIface {
                        name: "new".to_owned(),
                        is_static: true,
                        params,
                        ret: TypeRef::Object(obj.name.clone()),
                        target: Callable::Dynamic(proto::constructor(t, func, signature)),
                    });
                } else if fid >= inherited {
                    let Some((params, ret)) = signature(findex, false) else {
                        continue;
                    };
                    methods.push(MethodIface {
                        name: name.to_owned(),
                        is_static: true,
                        params,
                        ret,
                        target,
                    });
                }
            }
        }

        let superclass = obj
            .super_
            .as_ref()
            .and_then(|s| types.get(s.0))
            .and_then(|t| t.obj.as_ref())
            .map(|o| o.name.clone());
        let iface = Interface {
            lang,
            module: obj.name.clone(),
            classes: vec![ClassIface {
                name: obj.name.rsplit('.').next().unwrap_or(&obj.name).to_owned(),
                type_name: obj.name.clone(),
                superclass,
                fields,
                statics,
                methods,
                ctor,
                class_object: proto::class_object(program.interpreter.c_type_of(index).cast()),
            }],
        };
        registry::publish(iface.clone()).map_err(|e| anyhow!("publishing {}: {e}", obj.name))?;
        published.push(Arc::new(iface));
    }
    Ok(published)
}
