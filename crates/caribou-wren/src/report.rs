//! Wren's part of the run report (`caribou::report`): the tier each
//! function of the VM's modules reached, and the host methods the
//! installed classes bind as sites.

use std::collections::HashMap;

use caribou::report::{Function, Site, Tier};
use wren_lift::intern::SymbolId;
use wren_lift::runtime::engine::TierState;
use wren_lift::runtime::object::{Method, ObjClass, ObjHeader, ObjType};
use wren_lift::runtime::vm::VM;

use crate::heap::record_for;

/// Have the VM count how each function is entered from now on, per
/// tier, so the report can say how often as well as how. Costs a counter
/// per dispatch; off unless a report is wanted.
pub fn count_entries(vm: &mut VM) {
    vm.engine.collect_tier_stats = true;
}

/// Every function of the modules the program loaded, by name and tier,
/// with its entries when [`count_entries`] was on. wren_lift's own
/// modules are left out, and so are the classes installed for another
/// language's.
pub fn functions(vm: &VM) -> Vec<Function> {
    let engine = &vm.engine;
    let methods = method_names(vm);
    let mut out = Vec::new();
    for (idx, body) in engine.functions.iter().enumerate() {
        let Some(module) = engine.func_modules.get(idx).and_then(|m| m.as_deref()) else {
            continue;
        };
        if is_builtin(module) {
            continue;
        }
        let name = match methods.get(&(idx as u32)) {
            Some(name) => name.as_str(),
            None => vm.interner.resolve(body.mir().name),
        };
        let tier = match engine.tier_states.get(idx).copied().unwrap_or_default() {
            TierState::Interpreted => Tier::Interpreted,
            TierState::BaselineNative => Tier::Baseline,
            TierState::OptimizedNative => Tier::Optimized,
        };
        let entries = engine.collect_tier_stats.then(|| {
            let s = engine.tier_stats.get(idx).copied().unwrap_or_default();
            s.interpreted_entries + s.baseline_entries + s.optimized_entries
        });
        out.push(Function {
            name: format!("{module} {name}"),
            tier,
            entries,
        });
    }
    out
}

/// wren_lift's own modules, and the modules of installed classes, which
/// are named for the language they stand for: `haxe:game.Player`.
fn is_builtin(module: &str) -> bool {
    module == "core" || module.starts_with("wren_lift") || module.contains(':')
}

/// `Class.signature` per function id, for every method of every class
/// the VM's modules hold. A method's index in its class's table is its
/// signature's symbol, `static:` before a static's or a constructor's,
/// as wren_lift binds them.
fn method_names(vm: &VM) -> HashMap<u32, String> {
    let mut names = HashMap::new();
    for module in vm.engine.modules.values() {
        for &v in &module.vars {
            let Some(p) = v.as_object() else {
                continue;
            };
            if unsafe { (*(p as *const ObjHeader)).obj_type } != ObjType::Class {
                continue;
            }
            let class = p as *const ObjClass;
            let class_name = vm.interner.resolve(unsafe { (*class).name });
            for (i, m) in unsafe { &(*class).methods }.iter().enumerate() {
                let closure = match m {
                    Some(Method::Closure(c)) | Some(Method::Constructor(c)) => *c,
                    _ => continue,
                };
                let id = unsafe { (*(*closure).function).fn_id };
                let sig = vm.interner.resolve(SymbolId::from_raw(i as u32));
                names
                    .entry(id)
                    .or_insert_with(|| format!("{class_name}.{sig}"));
            }
        }
    }
    names
}

/// The host methods the program has called on the classes installed in
/// this VM, as sites. Nothing crosses boxed here: a Wren value is a
/// bridge value already.
pub fn sites(vm: &VM) -> Vec<Site> {
    let rec = record_for(vm.object_class as *mut u8);
    let imports = rec.imports().borrow();
    imports
        .targets()
        .map(|t| Site {
            name: t.label.clone(),
            direct: t.site.direct().is_some(),
            plain: t.site.plain(),
            boxed_in: 0,
            boxed_out: 0,
        })
        .filter(|site| site.direct || site.plain > 0)
        .collect()
}
