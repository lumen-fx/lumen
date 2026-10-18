//! The operating system, in words, for a report.

/// The OS name and version as the report states it: `Ubuntu 24.04.1 LTS
/// (Linux 6.8.0-45-generic)`, `macOS 15.1`, `Windows 10.0.22631`. Falls
/// back to the bare OS family when the version cannot be read.
pub fn description() -> String {
    let detail = imp::detail();
    let arch = std::env::consts::ARCH;
    match detail {
        Some(detail) => format!("{detail}, {arch}"),
        None => format!("{}, {arch}", std::env::consts::OS),
    }
}

#[cfg(target_os = "linux")]
mod imp {
    pub(super) fn detail() -> Option<String> {
        let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .ok()
            .map(|s| s.trim().to_string());
        let distro = std::fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|s| pretty_name(&s));
        match (distro, kernel) {
            (Some(d), Some(k)) => Some(format!("{d} (Linux {k})")),
            (None, Some(k)) => Some(format!("Linux {k}")),
            (Some(d), None) => Some(d),
            (None, None) => None,
        }
    }

    /// `PRETTY_NAME` out of an os-release file, unquoted.
    pub(super) fn pretty_name(os_release: &str) -> Option<String> {
        os_release.lines().find_map(|line| {
            let value = line.strip_prefix("PRETTY_NAME=")?;
            let value = value.trim().trim_matches('"').trim_matches('\'');
            (!value.is_empty()).then(|| value.to_string())
        })
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::CStr;

    pub(super) fn detail() -> Option<String> {
        let version = sysctl_string(c"kern.osproductversion")?;
        Some(format!("macOS {version}"))
    }

    fn sysctl_string(name: &CStr) -> Option<String> {
        let mut len: libc::size_t = 0;
        // SAFETY: a size query with a null buffer, then a read into a buffer
        // of the size the first call reported.
        unsafe {
            if libc::sysctlbyname(
                name.as_ptr(),
                std::ptr::null_mut(),
                &mut len,
                std::ptr::null_mut(),
                0,
            ) != 0
                || len == 0
            {
                return None;
            }
            let mut buf = vec![0u8; len];
            if libc::sysctlbyname(
                name.as_ptr(),
                buf.as_mut_ptr().cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            ) != 0
            {
                return None;
            }
            buf.truncate(len);
            let text = CStr::from_bytes_until_nul(&buf).ok()?;
            Some(text.to_string_lossy().into_owned())
        }
    }
}

#[cfg(windows)]
mod imp {
    use windows_sys::Wdk::System::SystemServices::RtlGetVersion;
    use windows_sys::Win32::System::SystemInformation::OSVERSIONINFOW;

    pub(super) fn detail() -> Option<String> {
        // SAFETY: RtlGetVersion fills the struct whose size it is handed and
        // reads nothing else; unlike GetVersionEx it is not lied to by the
        // compatibility shims.
        let info = unsafe {
            let mut info: OSVERSIONINFOW = std::mem::zeroed();
            info.dwOSVersionInfoSize = std::mem::size_of::<OSVERSIONINFOW>() as u32;
            if RtlGetVersion(&mut info) != 0 {
                return None;
            }
            info
        };
        // Windows 11 still reports major version 10; the build number is
        // what tells them apart.
        let name = if info.dwMajorVersion == 10 && info.dwBuildNumber >= 22_000 {
            "Windows 11"
        } else if info.dwMajorVersion == 10 {
            "Windows 10"
        } else {
            "Windows"
        };
        Some(format!(
            "{name} ({}.{}.{})",
            info.dwMajorVersion, info.dwMinorVersion, info.dwBuildNumber
        ))
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod imp {
    pub(super) fn detail() -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_description_names_the_architecture() {
        let text = super::description();
        assert!(text.ends_with(std::env::consts::ARCH), "{text}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pretty_name_is_read_unquoted() {
        let file = "NAME=\"Arch Linux\"\nPRETTY_NAME=\"Arch Linux\"\nID=arch\n";
        assert_eq!(super::imp::pretty_name(file).as_deref(), Some("Arch Linux"));
        assert_eq!(super::imp::pretty_name("ID=x\n"), None);
    }
}
