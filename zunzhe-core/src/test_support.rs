use std::path::Path;
use std::time::{Duration, SystemTime};

/// 把文件修改时间设置为若干天前。仅用于测试模拟“超龄残留”。
pub fn set_mtime_days_ago(path: &Path, days: u64) {
    set_mtime_seconds_ago(path, days * 24 * 60 * 60);
}

/// 把文件修改时间设置为若干秒前。仅用于测试模拟不同新旧程度。
pub fn set_mtime_seconds_ago(path: &Path, seconds: u64) {
    let old_time = SystemTime::now() - Duration::from_secs(seconds);
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        let secs = old_time
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        let times = [
            libc::timespec {
                tv_sec: secs,
                tv_nsec: 0,
            },
            libc::timespec {
                tv_sec: secs,
                tv_nsec: 0,
            },
        ];
        assert_eq!(
            unsafe { libc::utimensat(libc::AT_FDCWD, c_path.as_ptr(), times.as_ptr(), 0) },
            0
        );
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;

        use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, GENERIC_WRITE};
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
            SetFileTime,
        };

        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        assert!(!handle.is_null(), "CreateFileW 打开测试文件失败");

        let unix_ticks = old_time
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            / 100;
        // Windows FILETIME 从 1601-01-01 起算，与 Unix 纪元相差 11644473600 秒。
        let filetime_ticks = unix_ticks + 116_444_736_000_000_000_u128;
        let time = FILETIME {
            dwLowDateTime: (filetime_ticks & 0xffff_ffff) as u32,
            dwHighDateTime: (filetime_ticks >> 32) as u32,
        };
        let ok = unsafe { SetFileTime(handle, std::ptr::null(), std::ptr::null(), &time) };
        unsafe {
            CloseHandle(handle);
        }
        assert!(ok != 0, "SetFileTime 设置测试文件时间失败");
    }
}
