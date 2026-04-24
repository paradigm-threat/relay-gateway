//! One-shot CleverScript execution for server hooks (e.g. QUERY) without `RUN_STATE` persistence.

use crate::ast::{ExportDefaultDecl, Statement};
use crate::error::RuntimeError;
use crate::host::HostHandle;
use crate::runtime::Runtime;
use crate::run_state::{array_value, date_value, json_value, localStorage_value, object_value};
use crate::value::Value;

/// Run a CleverScript module whose **default export** is a function. The first parameter is bound
/// to `context` (as [`Value::from_json`]). Returns the value of the first `return` in that
/// function (typically `{ rows: [...] }` for QUERY).
///
/// When `relay` is set, it is installed as the global `Relay` object (e.g. `Relay.git.readFile`
/// implemented with [`Value::NativeFn`](crate::value::Value::NativeFn) / [`NativeFnClosure`](crate::value::NativeFnClosure)).
///
/// Does not use [`crate::run_state::run_hook_persistent`] so it does not touch thread-local hook
/// state used by client render persistence.
pub fn run_query_script(
    source: &str,
    host: HostHandle,
    context: &serde_json::Value,
    relay: Option<Value>,
) -> Result<Value, RuntimeError> {
    let program = crate::parse(source)?;
    let analysis = crate::analyze(&program)?;
    let mut runtime = Runtime::new(program, analysis).with_host(host);
    {
        let mut env = runtime.env.borrow_mut();
        env.set("localStorage", localStorage_value());
        env.set("Array", array_value());
        env.set("JSON", json_value());
        env.set("Object", object_value());
        env.set("Date", date_value());
        match relay {
            Some(r) => env.set("Relay", r),
            None => env.set("Relay", Value::Null),
        }
    }
    bind_default_export_first_param(&runtime, context)?;
    runtime.run()
}

fn bind_default_export_first_param(
    runtime: &Runtime,
    context: &serde_json::Value,
) -> Result<(), RuntimeError> {
    for stmt in &runtime.program.body {
        if let Statement::ExportDefault { declaration } = stmt {
            if let ExportDefaultDecl::Function { params, .. } = declaration.as_ref() {
                if let Some(p) = params.first() {
                    runtime
                        .env
                        .borrow_mut()
                        .set(&p.name, Value::from_json(context));
                }
            }
            break;
        }
    }
    Ok(())
}
