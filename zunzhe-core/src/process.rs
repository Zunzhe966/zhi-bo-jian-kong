use std::process::{ExitStatus, Output};
use std::time::Duration;

use tokio::process::{Child, Command};

/// 外部子进程执行的统一结果。
#[derive(Debug)]
pub enum ChildError {
    Io(std::io::Error),
    Timeout,
}

impl std::fmt::Display for ChildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "子进程 IO 错误: {e}"),
            Self::Timeout => write!(f, "子进程执行超时"),
        }
    }
}

impl std::error::Error for ChildError {}

/// 统一管理外部子进程生命周期。
///
/// `tokio::process::Child::kill_on_drop(true)` 只会杀掉直接父进程，不会自动清理
/// 子进程再启动的 ffmpeg/Electron 等孙进程。这个包装器在 Unix 上把外部命令放进
/// 独立进程组，超时时按进程组发送 SIGTERM/SIGKILL，Windows 则使用 taskkill /T。
pub struct ManagedChild {
    child: Child,
    pid: Option<u32>,
}

impl ManagedChild {
    /// 启动命令并放入独立进程组。调用方应把 `kill_on_drop(true)` 视为额外保险，
    /// 真正的超时清理由本结构负责。
    pub fn spawn(cmd: &mut Command) -> std::io::Result<Self> {
        #[cfg(unix)]
        {
            // `0` 表示使用子进程 PID 作为 PGID，因此 PGID 可直接由 `child.id()` 得到。
            cmd.process_group(0);
        }
        let child = cmd.spawn()?;
        let pid = child.id();
        Ok(Self { child, pid })
    }

    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// 等待进程结束并返回完整输出；超时会先终止整个进程组。
    pub async fn wait_with_output(self, timeout: Duration) -> Result<Output, ChildError> {
        let pid = self.pid;
        match tokio::time::timeout(timeout, self.child.wait_with_output()).await {
            Ok(Ok(output)) => Ok(output),
            Ok(Err(e)) => Err(ChildError::Io(e)),
            Err(_) => {
                terminate_process(pid);
                Err(ChildError::Timeout)
            }
        }
    }

    /// 等待进程退出并返回状态；超时会先终止整个进程组。
    pub async fn wait(mut self, timeout: Duration) -> Result<ExitStatus, ChildError> {
        let pid = self.pid;
        match tokio::time::timeout(timeout, self.child.wait()).await {
            Ok(Ok(status)) => Ok(status),
            Ok(Err(e)) => Err(ChildError::Io(e)),
            Err(_) => {
                terminate_process(pid);
                Err(ChildError::Timeout)
            }
        }
    }
}

/// 判断 PID 是否仍存活。仅用于诊断和测试，不参与正常回收主路径。
pub fn process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        if pid == 0 || pid > i32::MAX as u32 {
            return false;
        }
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}")])
            .output()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout).lines().any(|line| {
                    let mut parts = line.split_whitespace();
                    let _image = parts.next();
                    parts
                        .next()
                        .and_then(|p| p.parse::<u32>().ok())
                        .map(|p| p == pid)
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    }
}

/// 终止一个子进程树。Unix 下要求该进程由 `ManagedChild::spawn` 启动并成为独立
/// 进程组；Windows 下通过 taskkill /T 终止子进程树。
pub fn terminate_process(pid: Option<u32>) {
    let Some(pid) = pid else {
        return;
    };
    terminate_process_by_id(pid);
}

fn terminate_process_by_id(pid: u32) {
    #[cfg(unix)]
    {
        if pid == 0 || pid > i32::MAX as u32 {
            return;
        }
        unsafe {
            let _ = libc::kill(-(pid as i32), libc::SIGTERM);
        }
        std::thread::sleep(Duration::from_millis(500));
        unsafe {
            let _ = libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_pid_is_not_alive() {
        assert!(!process_alive(0));
        assert!(!process_alive(u32::MAX));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn timeout_also_kills_grandchild_process() {
        let dir = std::env::temp_dir().join(format!("zunzhe-proc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let pid_file = dir.join("grandchild.pid");
        let script = format!("sleep 30 & echo $! > '{}'; wait", pid_file.display());

        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(script);
        let child = ManagedChild::spawn(&mut cmd).unwrap();
        assert!(matches!(
            child.wait(Duration::from_millis(150)).await,
            Err(ChildError::Timeout)
        ));

        // 给 SIGTERM/SIGKILL 留出执行时间，避免测试自身比清理更快。
        std::thread::sleep(Duration::from_millis(300));
        let grandchild = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap();
        assert!(
            !process_alive(grandchild),
            "超时后孙进程 {grandchild} 应被进程组信号回收"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
