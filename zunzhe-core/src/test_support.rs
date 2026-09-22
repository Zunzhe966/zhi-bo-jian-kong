use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Create a self-contained project root whose configured product paths stay
/// inside that root. Tests must not inherit `~/Documents/尊者产物` or touch the
/// real source tree just because a fixture happens to create `fupan_output/`.
pub fn isolated_project(name: &str) -> PathBuf {
    let project = std::env::temp_dir().join(format!(
        "zunzhe-test-{name}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&project);
    std::fs::create_dir_all(project.join("config")).unwrap();
    let media_root = project.join("products");
    std::fs::write(
        project.join("config").join("config.ini"),
        format!(
            "[路径设置]\n产物根目录 = {}\n数据库文件 = {}\n",
            media_root.display(),
            project.join("zunzhe-core.db").display()
        ),
    )
    .unwrap();
    std::fs::write(
        project.join("config").join("fupan_config.ini"),
        "[视频监控]\n",
    )
    .unwrap();
    project
}

/// 写一个最小、可被 ffmpeg/PyAV 打开的有效 WAV 夹具。
///
/// 复盘入队会真实校验媒体可解码，测试不能用 `b"fixture"` 冒充 .mp4；
/// 这个 0 采样 WAV 让文件/任务/API 契约测试仍聚焦各自目标。
pub fn write_valid_wav(path: &Path) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    const WAV: &[u8] = b"RIFF\x24\x00\x00\x00WAVEfmt \x10\x00\x00\x00\x01\x00\x01\x00\x40\x1f\x00\x00\x80\x3e\x00\x00\x02\x00\x10\x00data\x00\x00\x00\x00";
    std::fs::write(path, WAV).unwrap();
}

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
