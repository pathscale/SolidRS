//! The host: instantiates the guest `.wasm` under wasmi and calls its exports.
//!
//! This is the step that upgrades "compiles to wasm32" into "runs under
//! wasmi". Compiling an rlib never links; instantiating a module does — so a
//! missing panic handler, an unsupported `std` surface, or a stray syscall
//! import shows up *here* and nowhere earlier.
//!
//! It also reports the two numbers worth knowing about the result: the module
//! size, and the module's import list. An empty import list means the reactive
//! core needs no host functions at all: no WASI, no JS glue.

use std::error::Error;
use std::path::PathBuf;

use wasmi::{Engine, Linker, Module, Store};

fn main() -> Result<(), Box<dyn Error>> {
    let path: PathBuf = std::env::args()
        .nth(1)
        .ok_or("usage: host <guest.wasm>")?
        .into();

    let bytes = std::fs::read(&path)?;
    println!("module      : {}", path.display());
    println!("size        : {} bytes ({:.1} KiB)", bytes.len(), bytes.len() as f64 / 1024.0);

    let engine = Engine::default();
    // Validation + compilation. A module that referenced an unsupported
    // feature, or failed to link at build time, never reaches here.
    let module = Module::new(&engine, &bytes[..])?;

    let imports: Vec<String> = module
        .imports()
        .map(|i| format!("{}::{}", i.module(), i.name()))
        .collect();
    println!(
        "imports     : {}",
        if imports.is_empty() {
            "none (no host functions required)".to_string()
        } else {
            imports.join(", ")
        }
    );

    let mut store = Store::new(&engine, ());
    let linker = <Linker<()>>::new(&engine);
    // Instantiate *and* run the start section. A trap during instantiation —
    // a failed static initializer, an unsatisfied import — surfaces here, and
    // is exactly the class of failure an rlib build can never reveal.
    let instance = linker.instantiate_and_start(&mut store, &module)?;

    let mut failures = 0;

    for (name, expected) in [("run", 40), ("run_async", 7), ("run_dispose", 2)] {
        let func = instance.get_typed_func::<(), i32>(&store, name)?;
        let got = func.call(&mut store, ())?;
        let ok = got == expected;
        println!(
            "{name:<12}: {got:<6} expected {expected:<6} {}",
            if ok { "OK" } else { "FAIL" }
        );
        if !ok {
            failures += 1;
        }
    }

    if failures > 0 {
        return Err(format!("{failures} export(s) returned the wrong value").into());
    }
    println!("\nall exports ran under wasmi and returned the expected values");
    Ok(())
}
