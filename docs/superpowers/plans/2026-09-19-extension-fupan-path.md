# Extension 复盘路径漂移修复计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use test-driven-development for each task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 MCP/extension 的复盘目录始终来自 `[路径设置]` 解析结果，消除与 CLI/daemon 的路径漂移。

**Architecture:** 在 `extension.rs` 中引入一个最小的 MCP 运行路径解析函数，`run_mcp` 和回归测试共同通过该 seam 获取 `VideoLibrary` 与 `fupan_dir` 所需的路径。新增测试使用独立临时项目和非默认配置目录，不触碰生产数据库、daemon 或产物。

**Tech Stack:** Rust 2024、tokio、sqlx SQLite、serde_json、现有 `crate::paths::fupan`。

**Spec:** `docs/superpowers/specs/2026-09-18-unified-actions-and-data-root-design.md`

## Global Constraints

- 不重启、不停止、不迁移生产 daemon 和生产数据库。
- 不删除、不覆盖现有数据库、WAL、日志或产物。
- 不执行 `git reset --hard`、`git checkout --`。
- 编辑前核对目标文件的 `stat` 与 `shasum`。
- 先让测试失败，再写最小实现。

---

### Task 1: 锁定 MCP 复盘目录来自配置

**Files:**
- Modify: `zunzhe-core/src/mcp.rs`（`lib.rs` 的 `pub mod mcp;` 才是实际编译入口）

**Interfaces:**
- Consumes: `crate::paths::fupan(project_dir: &Path) -> PathBuf`
- Produces: `fn mcp_fupan_dir(project_dir: &Path) -> PathBuf`（内部测试与运行共用）

- [x] **Step 1: Write the failing test**

在 `mcp.rs` 的测试模块加入：

```rust
#[test]
fn mcp_fupan_dir_honours_configured_output_directory() {
    let dir = std::env::temp_dir().join(format!(
        "zunzhe-extension-fupan-path-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("config")).unwrap();
    std::fs::write(
        dir.join("config").join("config.ini"),
        "[路径设置]\n复盘输出目录 = /tmp/zunzhe-extension-fupan-自定义\n",
    )
    .unwrap();

    assert_eq!(
        mcp_fupan_dir(&dir),
        std::path::PathBuf::from("/tmp/zunzhe-extension-fupan-自定义")
    );

    let _ = std::fs::remove_dir_all(&dir);
}
```

- [x] **Step 2: Run test to verify it fails**

```bash
cargo test -p zunzhe-core --lib mcp_fupan_dir_honours_configured_output_directory
```

Expected: FAIL because `mcp_fupan_dir` does not exist.

- [x] **Step 3: Write minimal implementation**

在 `run_mcp` 前加入：

```rust
fn mcp_fupan_dir(project_dir: &Path) -> std::path::PathBuf {
    crate::paths::fupan(project_dir)
}
```

并把 `run_mcp` 中的两处 `project_dir.join("fupan_output")` 替换为
`mcp_fupan_dir(project_dir)`。

- [x] **Step 4: Run test to verify it passes**

```bash
cargo test -p zunzhe-core --lib mcp_fupan_dir_honours_configured_output_directory
```

Expected: PASS.

- [x] **Step 5: Run related regression**

```bash
cargo test -p zunzhe-core --lib mcp_
cargo check -p zunzhe-core
```

Expected: PASS with no errors.

- [x] **Step 6: Record evidence**

在下一次进度报告中写明：失败命令、失败原因、修复命令、通过结果，以及未触碰生产状态的确认。
