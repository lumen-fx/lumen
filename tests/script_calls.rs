//! `lumen_call` from C reaches a module's script functions once the app that
//! declares the module has been built: here `lumen-fs`, linked into this
//! test binary, answers `files::exists` and `files::list`.

use std::ffi::CString;
use std::path::PathBuf;
use std::ptr;

use lumen::{
    LumenKind, LumenStatus, LumenValue, LumenValueData, lumen_app_new, lumen_app_run_headless,
    lumen_call, lumen_value_free,
};

// Leaves `lumen-fs`'s install entry where the module loader looks for a
// bundled module.
use lumen_fs as _;

fn fixture() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lumen_script_calls_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("lumen.toml"),
        "[app]\nentry = \"main.lmn\"\n\n[mcp]\nport = 0\n\n\
         [dependencies]\nlumen-fs = { bundled = true }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src").join("main.lmn"),
        "<root><label text=\"hi\"/></root>\n",
    )
    .unwrap();
    std::fs::write(dir.join("src").join("note.txt"), "x").unwrap();
    dir
}

fn string(s: &CString) -> LumenValue {
    LumenValue {
        kind: LumenKind::String,
        as_: LumenValueData { string: s.as_ptr() },
    }
}

fn nil() -> LumenValue {
    LumenValue {
        kind: LumenKind::Nil,
        as_: LumenValueData { integer: 0 },
    }
}

#[test]
fn a_module_function_answers_a_call_from_c() {
    let dir = fixture();
    let path = CString::new(dir.to_str().unwrap()).unwrap();
    let app = unsafe { lumen_app_new(path.as_ptr()) };
    assert!(!app.is_null());
    assert_eq!(unsafe { lumen_app_run_headless(app, 2) }, LumenStatus::Ok);

    let ns = CString::new("files").unwrap();
    let exists = CString::new("exists").unwrap();
    let note = CString::new(dir.join("src").join("note.txt").to_str().unwrap()).unwrap();
    let mut out = nil();
    let status = unsafe { lumen_call(ns.as_ptr(), exists.as_ptr(), 1, &string(&note), &mut out) };
    assert_eq!(status, LumenStatus::Ok);
    assert_eq!(out.kind, LumenKind::Bool);
    assert_ne!(unsafe { out.as_.boolean }, 0, "the file is there");
    unsafe { lumen_value_free(&mut out) };

    let list = CString::new("list").unwrap();
    let src = CString::new(dir.join("src").to_str().unwrap()).unwrap();
    let status = unsafe { lumen_call(ns.as_ptr(), list.as_ptr(), 1, &string(&src), &mut out) };
    assert_eq!(status, LumenStatus::Ok);
    assert_eq!(out.kind, LumenKind::Array);
    let names: Vec<String> = unsafe {
        let view = out.as_.array;
        std::slice::from_raw_parts(view.items, view.len)
            .iter()
            .map(|v| {
                std::ffi::CStr::from_ptr(v.as_.string)
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    };
    assert_eq!(names, ["main.lmn", "note.txt"]);
    unsafe { lumen_value_free(&mut out) };

    let status = unsafe {
        lumen_call(
            ns.as_ptr(),
            exists.as_ptr(),
            0,
            ptr::null(),
            ptr::null_mut(),
        )
    };
    assert_eq!(status, LumenStatus::ErrBadArg, "exists takes a path");
    let _ = std::fs::remove_dir_all(&dir);
}
