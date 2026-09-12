//! A minimal runner: ash's interpreter, with its Cranelift tier in hybrid
//! mode, over the core. It does what ash's CLI does for `--mode interp` and
//! `--mode hybrid`, with the seam installed first. It exists to prove the
//! seam, not to replace that CLI.
//!
//!     caribou-ash [--mode interp|hybrid] [--no-install] <file.hl> [args...]
//!
//! `--no-install` runs the same program on ash's own heap and scheduler.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use ash_core::bytecode::BytecodeDecoder;
use ash_core::native_lib::{self, NativeFunctionResolver};
use ash_interp::interpreter::{HLInterpreter, TierMode, TierPreset, TieredConfig};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Interp,
    Hybrid,
}

struct Args {
    mode: Mode,
    install: bool,
    file: PathBuf,
    program_args: Vec<String>,
}

fn parse_args() -> Result<Args> {
    let mut mode = Mode::Hybrid;
    let mut install = true;
    let mut file = None;
    let mut program_args = Vec::new();
    let mut argv = std::env::args().skip(1);
    while let Some(arg) = argv.next() {
        if file.is_some() {
            program_args.push(arg);
            continue;
        }
        match arg.as_str() {
            "--mode" => {
                mode = match argv.next().as_deref() {
                    Some("interp") => Mode::Interp,
                    Some("hybrid") => Mode::Hybrid,
                    other => bail!("--mode takes interp or hybrid, not {other:?}"),
                };
            }
            "--no-install" => install = false,
            _ if arg.starts_with("--") => bail!("unknown flag {arg}"),
            _ => file = Some(PathBuf::from(arg)),
        }
    }
    let file = file.ok_or_else(|| {
        anyhow!("usage: caribou-ash [--mode interp|hybrid] [--no-install] <file.hl> [args...]")
    })?;
    Ok(Args {
        mode,
        install,
        file,
        program_args,
    })
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
    let seam = unsafe { caribou_ash::Seam::from_lookup(lookup) }?;
    caribou_ash::install_into(seam)?;
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn install_into_sibling_runtime() -> Result<()> {
    bail!("this runner installs into a dlopened ash_std on macOS only");
}

fn run() -> Result<()> {
    let args = parse_args()?;
    if !args.file.exists() {
        bail!("Bytecode file not found: {}", args.file.display());
    }

    // Before ash's heap exists: `init_std_library` creates it.
    let static_std = native_lib::choose_std_linkage(&args.file);
    if args.install {
        if static_std {
            caribou_ash::install()?;
        } else {
            install_into_sibling_runtime()?;
        }
    }
    native_lib::init_std_library()?;

    // The image ash will run through is the one that had to take the table.
    let installed_addr = native_lib::std_symbol_addr("hlp_rt_installed")
        .ok_or_else(|| anyhow!("hlp_rt_installed not found in ash_std"))?;
    let installed: unsafe extern "C" fn() -> bool = unsafe { std::mem::transmute(installed_addr) };
    if unsafe { installed() } != args.install {
        bail!(
            "the hosted ash_std {} the runtime table",
            if args.install {
                "did not take"
            } else {
                "already has"
            }
        );
    }

    sys_init(&args.file, &args.program_args)?;

    let bytecode = Arc::new(BytecodeDecoder::decode(&args.file)?);
    let mut native_resolver = NativeFunctionResolver::new();
    let search_dir = args.file.parent().unwrap_or_else(|| Path::new("."));
    native_resolver.discover_and_load_libraries(search_dir, &bytecode.natives, true)?;

    let mut interpreter = HLInterpreter::new(&bytecode, &native_resolver);
    match args.mode {
        Mode::Interp => {
            interpreter.execute_entrypoint(&bytecode, &native_resolver)?;
        }
        Mode::Hybrid => {
            // ash's CLI defaults: the Application preset, tier from ASH_TIER.
            let tier_mode = match std::env::var("ASH_TIER") {
                Ok(spec) => TierMode::parse(&spec)
                    .ok_or_else(|| anyhow!("invalid ASH_TIER value {spec:?}"))?,
                Err(_) => TierMode::default(),
            };
            let cfg = TieredConfig {
                tier_mode,
                ..TierPreset::Application.to_config()
            };
            interpreter.enable_tiered(&args.file, &native_resolver, &bytecode, cfg)?;
            interpreter.execute_entrypoint(&bytecode, &native_resolver)?;
            interpreter.quiesce_promotions();
            // A tier-chase thread may still read what the interpreter owns;
            // ash leaks it for the same reason.
            std::mem::forget(interpreter);
        }
    }
    Ok(())
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

fn main() {
    if let Err(e) = run() {
        // An uncaught HL exception is the program's failure and already
        // reads as HashLink prints it.
        let text = format!("{e:#}");
        if text.starts_with("Uncaught exception:") {
            eprintln!("{text}");
        } else {
            eprintln!("Error: {text}");
        }
        std::process::exit(1);
    }
}
