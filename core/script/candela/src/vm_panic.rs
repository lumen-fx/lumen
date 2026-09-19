//! What a panic out of the candela VM is reported as, on either host.
//!
//! candela reports a script's own problems as `Err` diagnostics, so a panic
//! instead means an assertion inside the VM itself fired and the interpreter
//! state can no longer be trusted. One known way in: a diagnostic thrown
//! mid-execution (a call into a host function no module registered, say) can
//! leave values behind on the VM stack, and the next call then dies on an
//! internal type assertion. An app must survive its script, so a host catches
//! that panic at its call boundary, drops the program - every later probe
//! misses silently, the shape a failed load already has - and reports the one
//! diagnostic built here.
//!
//! Both hosts build the same diagnostic from the same payload: a failure
//! report should not let a reader tell which host produced it.

use candela_vm::Diagnostic;
use lumen_script::panic_payload_text;

/// The diagnostic code a contained VM panic reports under.
pub(crate) const VM_PANIC_CODE: &str = "vm_panic";

/// The diagnostic for a panic caught out of a call to `fn_name`.
pub(crate) fn vm_panic_diagnostic(
    fn_name: &str,
    payload: &(dyn std::any::Any + Send),
) -> Diagnostic {
    let detail = panic_payload_text(payload);
    Diagnostic {
        filename: String::new(),
        span: 0..0,
        message: format!(
            "the candela VM panicked calling `{fn_name}` ({detail}); its state \
             cannot be trusted after the panic, so the script is disabled"
        ),
        code: VM_PANIC_CODE.to_owned(),
    }
}
