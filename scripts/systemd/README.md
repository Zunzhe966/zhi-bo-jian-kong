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
`WorkingDirectory=/opt/zunzhe` 改成实际路径，并保证该用户对项目目录有写权限
（`logs/`、`fupan_output/`、`downloads/` 会由程序创建）。

## 3. 查看与排查

```bash
systemctl status zunzhe-core
journalctl -u zunzhe-core -n 100
sudo -u zunzhe /opt/zunzhe/zunzhe-core/target/release/zunzhe-core doctor
```

服务复用 `scripts/start-rust-daemon.sh`：每次启动先构建 release，再 `exec`
daemon，日志进入 journald；Rust 自身仍会把心跳写入
`logs/zunzhe-core.uptime.log`（daemon 心跳记录器按 10MB 轮转保留 3 份）、
把 SQLite/pid 写入 `fupan_output/run/`；桌面端/系统健康看门狗记录写入
`logs/zunzhe-core.watchdog.log`（按 5MB 轮转保留 5 份）。
注意：systemd 的 `Restart=always` 只负责进程退出后拉起；若心跳超时，
`doctor` 会把 `heartbeat_ok` 置为 false，可再加一个 cron/systemd timer
调用 `scripts/check-rust-daemon.sh` 做二级健康看门。
