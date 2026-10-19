//! The readable half of a native fault, read back out of its minidump.

use std::borrow::Cow;

use lumen_crash_report::{Fault, Frame};
use minidump::format::MINIDUMP_EXCEPTION;
use minidump::system_info::Os;
use minidump::{
    Minidump, MinidumpException, MinidumpMiscInfo, MinidumpModuleList, MinidumpSystemInfo,
    MinidumpThreadList, MinidumpThreadNames, Module,
};

/// How many stack entries a report lists.
const MAX_FRAMES: usize = 32;
/// How far up the stack the scan reads, in pointer-sized words.
const MAX_WORDS: u64 = 4096;

/// The fault a minidump records: what it was, where, in which module and
/// thread, and the addresses on the crashing thread's stack that fall in a
/// loaded module (a scan, not an unwind: most of them are return addresses,
/// some are stale).
pub fn fault(bytes: &[u8]) -> Option<Fault> {
    let dump = Minidump::read(bytes).ok()?;
    let exception: MinidumpException = dump.get_stream().ok()?;
    let system: MinidumpSystemInfo = dump.get_stream().ok()?;
    let misc: Option<MinidumpMiscInfo> = dump.get_stream().ok();
    let modules: Option<MinidumpModuleList> = dump.get_stream().ok();
    let module_of = |address: u64| -> Option<(String, u64)> {
        let module = modules.as_ref()?.module_at_address(address)?;
        Some((
            base_name(module.code_file()),
            address - module.base_address(),
        ))
    };

    let (description, address) = match sent_signal(system.os, &exception.raw.exception_record) {
        // The address of a sent signal is only where the thread was.
        Some(description) => (description, None),
        None => (
            exception
                .get_crash_reason(system.os, system.cpu)
                .to_string(),
            Some(exception.get_crash_address(system.os, system.cpu)),
        ),
    };
    let context = exception.context(&system, misc.as_ref());
    let ip = context.as_ref().map(|c| c.get_instruction_pointer());
    let sp = context.as_ref().map(|c| c.get_stack_pointer());

    let thread_id = exception.thread_id;
    let thread = dump
        .get_stream::<MinidumpThreadNames>()
        .ok()
        .and_then(|names| names.get_name(thread_id).map(Cow::into_owned))
        .map(|name| format!("{name} ({thread_id})"))
        .unwrap_or_else(|| thread_id.to_string());

    let mut stack = Vec::new();
    if let Some(ip) = ip {
        stack.push(Frame {
            address: ip,
            module: module_of(ip),
        });
    }
    if let (Some(sp), Ok(threads), Some(memory)) = (
        sp,
        dump.get_stream::<MinidumpThreadList>(),
        dump.get_memory(),
    ) && let Some(stack_memory) = threads
        .get_thread(thread_id)
        .and_then(|t| t.stack_memory(&memory))
    {
        let end = stack_memory.base_address() + stack_memory.size();
        let mut at = sp;
        let mut words = 0;
        while at + 8 <= end && words < MAX_WORDS && stack.len() < MAX_FRAMES {
            if let Some(word) = stack_memory.get_memory_at_address::<u64>(at)
                && let Some(module) = module_of(word)
            {
                stack.push(Frame {
                    address: word,
                    module: Some(module),
                });
            }
            at += 8;
            words += 1;
        }
    }

    Some(Fault {
        description,
        address,
        module: ip
            .and_then(module_of)
            .map(|(name, offset)| format!("{name} + {offset:#x}")),
        thread: Some(thread),
        stack,
    })
}

/// The fault in words when `record` is a signal that was sent rather than
/// a fault: `SIGSEGV / EXC_SOFT_SIGNAL`. Only macOS dumps record one apart,
/// as `EXC_SOFTWARE` with `EXC_SOFT_SIGNAL` and the signal's number in the
/// subcode (`SIGABRT`, or one from `kill` or `raise`); a fault there is a
/// Mach exception of its own.
fn sent_signal(os: Os, record: &MINIDUMP_EXCEPTION) -> Option<String> {
    const EXC_SOFTWARE: u32 = 5;
    const EXC_SOFT_SIGNAL: u32 = 0x10003;
    if os != Os::MacOs
        || record.exception_code != EXC_SOFTWARE
        || record.exception_flags != EXC_SOFT_SIGNAL
        || record.number_parameters < 3
    {
        return None;
    }
    // The numbers are macOS's.
    let subcode = record.exception_information[2];
    let name = match subcode {
        4 => "SIGILL",
        5 => "SIGTRAP",
        6 => "SIGABRT",
        8 => "SIGFPE",
        10 => "SIGBUS",
        11 => "SIGSEGV",
        _ => return Some(format!("signal {subcode} / EXC_SOFT_SIGNAL")),
    };
    Some(format!("{name} / EXC_SOFT_SIGNAL"))
}

/// The file name of a module path, either separator.
fn base_name(path: Cow<'_, str>) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(&path).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_module_path_is_cut_to_its_file_name() {
        assert_eq!(
            base_name("/usr/lib/libvulkan.so.1".into()),
            "libvulkan.so.1"
        );
        assert_eq!(
            base_name("C:\\Windows\\System32\\nvoglv64.dll".into()),
            "nvoglv64.dll"
        );
        assert_eq!(base_name("plain".into()), "plain");
    }

    #[test]
    fn a_sent_signal_on_macos_is_named_by_its_number() {
        let record = |kind: u32, code: u32, signal: u64| {
            let mut record = MINIDUMP_EXCEPTION {
                exception_code: kind,
                exception_flags: code,
                number_parameters: 3,
                ..Default::default()
            };
            record.exception_information[2] = signal;
            record
        };
        let named = |signal| sent_signal(Os::MacOs, &record(5, 0x10003, signal));
        for (signal, name) in [
            (4, "SIGILL"),
            (5, "SIGTRAP"),
            (6, "SIGABRT"),
            (8, "SIGFPE"),
            (10, "SIGBUS"),
            (11, "SIGSEGV"),
            (30, "signal 30"),
        ] {
            assert_eq!(named(signal), Some(format!("{name} / EXC_SOFT_SIGNAL")));
        }
        // A bad access is a fault, not a sent signal.
        assert_eq!(sent_signal(Os::MacOs, &record(1, 1, 0)), None);
        // Another system's dump keeps its own reading.
        assert_eq!(sent_signal(Os::Linux, &record(5, 0x10003, 11)), None);
        let mut short = record(5, 0x10003, 11);
        short.number_parameters = 2;
        assert_eq!(sent_signal(Os::MacOs, &short), None);
    }

    #[test]
    fn bytes_that_are_not_a_minidump_give_nothing() {
        assert!(fault(b"not a dump").is_none());
    }
}
