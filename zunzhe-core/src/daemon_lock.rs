//! 单实例守护锁：同一项目根只允许一个 Rust daemon。
//!
//! 锁文件放 `fupan_output/run/zunzhe-core.lock`，进程退出时内核自动释放，
//! 不会因为残留 pid 文件而永久占锁。

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use fs2::FileExt;

/// 已持有独占锁的句柄；drop 时释放锁。
pub struct DaemonLock {
    file: File,
}

impl DaemonLock {
    /// 尝试立即获取独占锁。已被其它进程持有则返回 `Ok(None)`，
    /// 文件打不开/系统错误返回 `Err`。
    pub fn try_acquire(project_dir: &Path) -> io::Result<Option<Self>> {
        let path = lock_file(project_dir);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)?;
        if !try_lock_exclusive(&file)? {
            return Ok(None);
        }
        Ok(Some(Self { file }))
    }

    /// 阻塞等待锁（用于 daemon 启动；正常守护下另一实例会很快让出）。
    pub fn acquire(project_dir: &Path) -> io::Result<Self> {
        let path = lock_file(project_dir);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)?;
        file.lock_exclusive()?;
        Ok(Self { file })
    }
}

impl Drop for DaemonLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

pub fn lock_file(project_dir: &Path) -> PathBuf {
    crate::paths::run_dir(project_dir).join("zunzhe-core.lock")
}

/// 尝试立即加独占锁；已有持有者返回 `Ok(false)`，IO 错误返回 `Err`。
#[cfg(unix)]
fn try_lock_exclusive(file: &File) -> io::Result<bool> {
    let fd = std::os::unix::io::AsRawFd::as_raw_fd(file);
    let ret = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
    if ret == 0 {
        return Ok(true);
    }
    let err = io::Error::last_os_error();
    if err.kind() == io::ErrorKind::WouldBlock {
        return Ok(false);
    }
    Err(err)
}

/// 尝试立即加独占锁；Windows 上“文件正被另一进程锁定”映射为 `Ok(false)`。
#[cfg(windows)]
fn try_lock_exclusive(file: &File) -> io::Result<bool> {
    let result = file.try_lock_exclusive();
    match result {
        Ok(()) => Ok(true),
        Err(err) if err.raw_os_error() == Some(33) => Ok(false),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_instance_lock_is_exclusive() {
        let dir = std::env::temp_dir().join(format!("zunzhe-lock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let first = DaemonLock::try_acquire(&dir).unwrap();
        assert!(first.is_some(), "第一个实例应能拿到锁");
        let second = DaemonLock::try_acquire(&dir).unwrap();
        assert!(second.is_none(), "第二个实例在同一项目根不应拿到锁");
        drop(first);

        let third = DaemonLock::try_acquire(&dir).unwrap();
        assert!(third.is_some(), "锁释放后应能重新获取");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
