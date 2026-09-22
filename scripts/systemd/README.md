# Linux systemd 常驻（可选，云服务器也适用）

在 Ubuntu/Debian/CentOS 等 Linux 服务器上把 `zunzhe-core` 作为 24×30
常驻服务运行，并由 systemd 自动拉起，不需要桌面端看门狗。

## 1. 放置项目

把项目放到 `/opt/zunzhe`，确保 release 二进制已构建：

```bash
cd /opt/zunzhe/zunzhe-core
cargo build --release
```

## 2. 安装单元

```bash
sudo cp /opt/zunzhe/scripts/systemd/zunzhe-core.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now zunzhe-core
```

如果服务需要以当前用户运行，请把 `User=zunzhe` 与
`WorkingDirectory=/opt/zunzhe` 改成实际路径。

> **产物不落在项目目录。** 程序与产物是物理分离的两回事，由 `config/config.ini`
> 的 `[路径设置]` 决定落点（见 `zunzhe-core/src/paths.rs` 头部说明）：
>
> | 键 | 留空时 |
> |---|---|
> | `产物根目录` | `~/Documents/尊者产物` |
> | `日志目录` | `<产物根目录>/logs` |
> | `短视频下载目录` | `<产物根目录>/downloads` |
> | `复盘输出目录` | `<产物根目录>/fupan_output` |
> | `数据库文件` | `<产物根目录>/database/zunzhe-core.db` |
>
> **该用户需要对「产物根目录」有写权限**，不是对项目目录。设了自定义产物根
> （例如外置盘）时，请确认已挂载且可写，否则 daemon 会在启动时写入失败。

## 3. 查看与排查

```bash
systemctl status zunzhe-core
journalctl -u zunzhe-core -n 100
sudo -u zunzhe /opt/zunzhe/zunzhe-core/target/release/zunzhe-core doctor
```

服务复用 `scripts/start-rust-daemon.sh`（**仓库里这一份就是正规版；装机版
macOS 侧是 `~/Library/LaunchAgents/zunzhe-daemon.sh`，两者行为应保持一致**）：
`exec` 后 daemon 的 stdout/stderr 进入 journald；Rust 自身把心跳写入
`<日志目录>/zunzhe-core.uptime.log`，把 pid / 心跳 / 维护标记写入
`<产物根>/fupan_output/run/`。**日志目录一律由 `[路径设置]` 的 `日志目录`
解析，不要在 daemon 启动脚本里再往项目根 `logs/` 写**——那会重新制造
「日志散落多处、源码目录被污染」的分叉。

排查日志位置是否被搞乱，跑：

```bash
bash scripts/audit-launchd-log-paths.sh   # macOS：比对配置/plist/已装载三处
```

注意：systemd 的 `Restart=always` 只负责进程退出后拉起；若心跳超时，
`doctor` 会把 `heartbeat_ok` 置为 false，可再加一个 cron/systemd timer
调用 `scripts/check-rust-daemon.sh` 做二级健康看门。
