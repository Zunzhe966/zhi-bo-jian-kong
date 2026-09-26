//! 产物存储路径解析：把「文件到底存在哪」从写死改成可配置。
//!
//! 这里刻意把「程序住在哪」和「产物放在哪」拆成两个概念：
//!
//! - **项目目录**（`project_dir`）：程序与配置所在。`config/`、`.venv/`、`src/`、
//!   `fupan/` 都在这里，由 `config::find_project_root` 往上找 `config/config.ini`
//!   决定。它**不该让用户随便改**——改了 daemon 立刻找不到配置和 Python 环境。
//! - **产物根目录**（`media_root`）：程序跑出来的东西。下载的短视频、转写文本、
//!   知识点、精华切片、封面、直播录像、运行态文件，全部落在它下面。
//!   它由 `config.ini` 的 `[路径设置] 产物根目录` 决定；**留空时落到
//!   `~/Documents/尊者产物`，不再退回项目目录**。
//!
//!   为什么不退回项目目录：打包成 `.app` 之后，项目目录就是源码所在。把录像和
//!   转写文本丢进源码里，用户既找不到（得知道项目在哪），也容易在换代程序时被
//!   连带清理。「程序」和「产物」必须在物理上分开——这是结构性保证，不靠自觉。
//!
//! 配置项（`[路径设置]` 节）：
//!
//! | 键 | 作用 | 留空时 |
//! |---|---|---|
//! | `产物根目录` | 产物总根 | `~/Documents/尊者产物` |
//! | `短视频下载目录` | 覆盖短视频下载落点 | `<产物根>/downloads` |
//! | `复盘输出目录` | 覆盖转写/知识点/切片落点 | `<产物根>/fupan_output` |
//! | `日志目录` | 覆盖日志落点 | `<产物根>/logs` |
//! | `数据库文件` | 覆盖 SQLite 库位置 | `<产物根>/database/zunzhe-core.db`（旧库存在且新库不存在时沿用旧库） |
//!
//! 路径写法支持三种：`~` 或 `~/xxx`（家目录）、绝对路径、相对项目目录的相对路径。
//!
//! 为什么不做成 `net::init_with_config` 那样的「只初始化一次」：daemon 每轮 tick
//! 都重读配置（见 `Watchlist::new(project_dir)`），用户改完配置不该还要重启才生效。
//! 这里改成「按项目目录 + `config.ini` 的 (mtime, 长度)」做进程内缓存，
//! 每次调用只多一次 `stat`，配置一改就自动重新解析。
//!
//! 注意：本模块的解析**永不失败**。读不到配置、路径非法、家目录取不到，统统退回
//! 旧行为。原因很直接——路径解析一旦能失败，就会被调用方 `?` 成「监控整个挂了」，
//! 而一个存储路径配错不该让 daemon 起不来。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

/// `config.ini` 里承载路径配置的节名。
pub const SECTION: &str = "路径设置";

/// 产物根目录留空时的默认文件夹名，落在 `~/Documents/` 下。
///
/// 选 `Documents` 而不是 `Application Support`：产物（录像、转写文本）是**给用户看的**
/// 东西，得让用户自己在访达里点得到；`Application Support` 是藏起来的。
pub const DEFAULT_MEDIA_DIR_NAME: &str = "尊者产物";

/// 解析出来的全部产物路径。字段名即用途，避免调用方再拼字符串。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoragePaths {
    /// 程序与配置所在目录（不可配置）。
    pub project_dir: PathBuf,
    /// 产物总根目录（可配置）。
    pub media_root: PathBuf,
    /// 短视频下载落点（历史字段：默认 `<产物根>/downloads`，仅作可信根/旧产物位置保留；
    /// 新下载落点见 `video_root`）。
    pub downloads: PathBuf,
    /// 短视频产物根：新下载落在 `{video_root}/{主播}/{分类}/{标题}/`。
    /// 等于 `[路径设置] 短视频下载目录` 覆盖值；未覆盖时为 `media_root`。
    pub video_root: PathBuf,
    /// 复盘输出落点（转写/知识点/切片/封面都在这下面）。
    pub fupan: PathBuf,
    /// 日志落点（daemon 心跳、核心 stdout/stderr 轮转文件）。
    pub logs: PathBuf,
    /// SQLite 库文件。
    pub database: PathBuf,
    /// `[录制设置] 直播保存路径` 的解析结果；未填为 `None`。
    pub live_save: Option<PathBuf>,
}

impl StoragePaths {
    /// 封面缓存目录（`<复盘输出>/covers`）。
    pub fn covers(&self) -> PathBuf {
        self.fupan.join("covers")
    }

    /// 运行态文件目录（`<复盘输出>/run`）：pid、锁、心跳、Cookie、协议文件都在这。
    pub fn run(&self) -> PathBuf {
        self.fupan.join("run")
    }

    /// 日志目录（`<产物根>/logs` 或配置覆盖）。
    pub fn logs(&self) -> PathBuf {
        self.logs.clone()
    }

    /// 直播录像落点：`直播保存路径` 优先，否则落在产物根（布局
    /// `{recordings}/{主播}/直播/{日期} {场次}/`，见产品手册第 3 章）。
    pub fn recordings(&self) -> PathBuf {
        self.live_save
            .clone()
            .unwrap_or_else(|| self.media_root.clone())
    }

    /// 是否用的默认产物落点（`~/Documents/尊者产物`），也就是用户没自定义过。
    ///
    /// doctor / 状态页用它提示用户当前有没有改过存放位置。
    /// 判据是「等于默认产物目录」，**不是**「等于项目目录」——后者是历史行为，
    /// 现在产物出现在项目目录里反而是异常（升级前留下的老布局）。
    pub fn is_default_layout(&self) -> bool {
        self.media_root == default_media_root()
    }

    /// 产物是否仍落在项目目录内部（升级前的旧布局）。
    ///
    /// 这不算错，但值得提示：程序换代或挪目录时，这里的文件容易被用户找不到。
    pub fn products_inside_project(&self) -> bool {
        self.media_root == self.project_dir
    }

    /// 供桌面端 `status.paths` 上报，界面据此打开正确目录。
    ///
    /// 「打开目录」按钮曾经因为白名单只认项目目录、而实际路径在别处而静默失效；
    /// 这里把全部可打开目录一次性上报，界面按需取用，不用猜。
    pub fn as_json(&self) -> serde_json::Value {
        serde_json::json!({
            "project_dir": self.project_dir.display().to_string(),
            "media_root": self.media_root.display().to_string(),
            "downloads": self.downloads.display().to_string(),
            "video_root": self.video_root.display().to_string(),
            "fupan_output": self.fupan.display().to_string(),
            "logs": self.logs().display().to_string(),
            "recordings": self.recordings().display().to_string(),
            "covers": self.covers().display().to_string(),
            "run": self.run().display().to_string(),
            "database": self.database.display().to_string(),
            "customized": !self.is_default_layout(),
        })
    }
}

/// 进程内缓存：单项目进程只留最近一次解析结果。
type CacheEntry = ((PathBuf, Option<(SystemTime, u64)>), Arc<StoragePaths>);

static CACHE: RwLock<Option<CacheEntry>> = RwLock::new(None);

/// 解析（并缓存）某个项目目录对应的产物路径。
///
/// 调用方拿到的是 `Arc`，可以随便 clone 下去；解析失败一律退回默认布局。
pub fn storage(project_dir: &Path) -> Arc<StoragePaths> {
    let project_dir = to_absolute(project_dir);
    let stamp = config_stamp(&project_dir);

    {
        let guard = CACHE.read().unwrap_or_else(|e| e.into_inner());
        if let Some((key, value)) = guard.as_ref()
            && key.0 == project_dir
            && key.1 == stamp
        {
            return value.clone();
        }
    }

    let resolved = Arc::new(resolve(&project_dir));
    let mut guard = CACHE.write().unwrap_or_else(|e| e.into_inner());
    *guard = Some(((project_dir, stamp), resolved.clone()));
    resolved
}

/// 产物总根目录。
pub fn media_root(project_dir: &Path) -> PathBuf {
    storage(project_dir).media_root.clone()
}

/// 短视频下载目录。
pub fn downloads(project_dir: &Path) -> PathBuf {
    storage(project_dir).downloads.clone()
}

/// 短视频产物根：新下载落在 `{video_root}/{主播}/{分类}/{标题}/`。
pub fn video_root(project_dir: &Path) -> PathBuf {
    storage(project_dir).video_root.clone()
}

/// 复盘输出目录（转写/知识点/切片）。
pub fn fupan(project_dir: &Path) -> PathBuf {
    storage(project_dir).fupan.clone()
}

/// 日志目录。
pub fn logs(project_dir: &Path) -> PathBuf {
    storage(project_dir).logs()
}

/// 封面缓存目录。
pub fn covers(project_dir: &Path) -> PathBuf {
    storage(project_dir).covers()
}

/// 运行态文件目录（pid/锁/心跳/Cookie）。
pub fn run_dir(project_dir: &Path) -> PathBuf {
    storage(project_dir).run()
}

/// SQLite 库文件路径。
pub fn database(project_dir: &Path) -> PathBuf {
    storage(project_dir).database.clone()
}

/// 把产物目录一次性建齐。
///
/// 用户把产物根指到一个全新目录（甚至另一个卷）时，daemon 第一轮就得能直接写，
/// 否则会先报一堆「创建下载目录失败」。启动路径调用一次即可，幂等。
pub fn ensure_dirs(project_dir: &Path) -> std::io::Result<()> {
    let p = storage(project_dir);
    // 项目目录自己是 find_project_root 定的，必然存在；这里只管产物侧。
    // downloads/短视频 不再是新产物落点（新布局直接在产物根下建主播目录，
    // 首次下载/录制时按需创建），这里不再预建，避免产物根下出现空壳目录。
    let mut dirs = vec![
        p.media_root.clone(),
        p.fupan.clone(),
        p.logs(),
        p.covers(),
        p.run(),
    ];
    // 用户显式覆盖过短视频下载目录时，才需要预建它（默认等于 media_root，已建）。
    if p.video_root != p.media_root {
        dirs.push(p.video_root.clone());
    }
    if let Some(parent) = p.database.parent() {
        // 库文件留空时 parent 就是项目目录，重复 create 无害。
        dirs.push(parent.to_path_buf());
    }
    for dir in dirs {
        std::fs::create_dir_all(&dir)?;
    }
    Ok(())
}

/// 升级兼容：旧库仍在项目根、新数据根尚无数据库时，用 SQLite 在线备份把旧库
/// 复制到新位置。复制后旧文件保持原样，绝不移动或删除；下次统一打开新库。
///
/// 必须在 `open_pool` 之前执行。SQLite 的 `VACUUM INTO` 会自动处理 WAL 中
/// 尚未 checkpoint 的数据，比直接复制 `zunzhe-core.db` 少了漏掉最新事务的风险。
pub async fn migrate_legacy_database_if_needed(
    project_dir: &Path,
    target_database: &Path,
) -> anyhow::Result<Option<std::path::PathBuf>> {
    let legacy_database = project_dir.join("zunzhe-core.db");
    if target_database == legacy_database || target_database.is_file() || !legacy_database.is_file()
    {
        return Ok(None);
    }

    if let Some(parent) = target_database.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = target_database.with_extension("db.migrating");
    let _ = std::fs::remove_file(&temporary);

    let options = crate::db::open_options(&legacy_database).read_only(true);
    let source = sqlx::SqlitePool::connect_with(options)
        .await
        .map_err(|e| anyhow::anyhow!("打开旧数据库失败: {e}"))?;
    let escaped = temporary.to_string_lossy().replace('\'', "''");
    let backup = sqlx::query(&format!("VACUUM INTO '{escaped}'"))
        .execute(&source)
        .await;
    source.close().await;
    if let Err(e) = backup {
        let _ = std::fs::remove_file(&temporary);
        return Err(anyhow::anyhow!("迁移旧数据库失败: {e}"));
    }
    if let Err(e) = std::fs::rename(&temporary, target_database) {
        let _ = std::fs::remove_file(&temporary);
        return Err(anyhow::anyhow!(
            "启用迁移后的数据库失败: {} ({e})",
            target_database.display()
        ));
    }
    Ok(Some(legacy_database))
}

/// 把用户写的路径文本展开成 `PathBuf`；空串返回 `None`（表示「用默认」）。
///
/// 公开出来是给 `doctor` 与配置校验复用，避免两处各写一套 `~` 展开规则。
pub fn expand(project_dir: &Path, raw: &str) -> Option<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let expanded = expand_tilde(raw);
    let path = PathBuf::from(expanded);
    if path.is_absolute() {
        Some(path)
    } else {
        // 相对路径按「相对项目目录」解释：用户写 `downloads` 时直觉是项目里的那个，
        // 而不是 daemon 当前工作目录里的（后者随 launchd/cli 启动方式漂移）。
        Some(project_dir.join(path))
    }
}

/// 展开 `~` / `~/xxx`。取不到家目录时原样返回，让后续按相对路径处理。
pub fn expand_tilde(raw: &str) -> String {
    let raw = raw.trim();
    if raw == "~" {
        return home_dir().unwrap_or_else(|| raw.to_string());
    }
    if let Some(rest) = raw.strip_prefix("~/")
        && let Some(home) = home_dir()
    {
        return format!("{home}/{rest}");
    }
    raw.to_string()
}

fn home_dir() -> Option<String> {
    for key in ["HOME", "USERPROFILE"] {
        if let Ok(v) = std::env::var(key) {
            let v = v.trim().to_string();
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
}

/// 产物根目录留空时的默认落点：`~/Documents/尊者产物`。
///
/// 取不到家目录时退回相对路径，由调用方按项目目录解释——那样虽又回到项目目录下，
/// 但总比让解析崩掉强（本模块的解析永不失败）。
pub fn default_media_root() -> PathBuf {
    match home_dir() {
        Some(h) => PathBuf::from(h)
            .join("Documents")
            .join(DEFAULT_MEDIA_DIR_NAME),
        None => PathBuf::from(DEFAULT_MEDIA_DIR_NAME),
    }
}

/// 真正的解析逻辑：读配置，逐项决定落点。
///
/// 刻意**只读 `config/config.ini`**，不走 `config::Config::load`。后者会连带读
/// `fupan_config.ini`，读不到就直接整体报错——而 `Config::load` 一失败，产物路径
/// 就会被静默忽略、全部退回默认布局。一个「复盘参数文件缺失」不该让用户的
/// 存放位置配置凭空失效，所以这里把依赖收窄到只跟路径有关的那个文件。
fn resolve(project_dir: &Path) -> StoragePaths {
    let ini = read_main_ini(project_dir);
    let read = |key: &str| -> String {
        ini.as_ref()
            .map(|f| f.get_case_insensitive(SECTION, key))
            .unwrap_or_default()
    };

    // 留空 = 落到 `~/Documents/尊者产物`，**不再退回项目目录**。
    // 项目目录就是源码所在，产物混进去等于让用户在源码堆里找录像。
    let media_root = expand(project_dir, &read("产物根目录")).unwrap_or_else(default_media_root);
    let downloads = expand(project_dir, &read("短视频下载目录"))
        .unwrap_or_else(|| media_root.join("downloads"));
    // 布局：短视频落在 `{video_root}/{主播}/{分类}/{标题}/`（分类 ∈ 短视频/批量下载）。
    // 用户显式覆盖 `短视频下载目录` 时以覆盖值为根，否则直接以产物根为根——
    // 产物根下一级是主播目录，不再经过 downloads/ 这层历史目录。
    let video_root = expand(project_dir, &read("短视频下载目录"))
        .unwrap_or_else(|| media_root.clone());
    let fupan = expand(project_dir, &read("复盘输出目录"))
        .unwrap_or_else(|| media_root.join("fupan_output"));
    // 运行态数据也必须跟数据根走，不能再默认写进源码目录。日志可以安全切换，
    // 不做旧目录回退；数据库则必须识别升级前的项目根库，否则重启后会打开一个
    // 空的新库，关注列表、任务和下载历史都会像“消失”一样。
    let logs = expand(project_dir, &read("日志目录")).unwrap_or_else(|| media_root.join("logs"));
    let default_database = media_root.join("database").join("zunzhe-core.db");
    let database = expand(project_dir, &read("数据库文件")).unwrap_or(default_database);
    // 录像落点由 `[录制设置]` 决定，与 `[路径设置]` 分属两节；只读取值，不解析。
    let live_save = ini.as_ref().and_then(|f| {
        let raw = f.get_case_insensitive("录制设置", "直播保存路径(不填则默认)");
        expand(project_dir, &raw)
    });

    StoragePaths {
        project_dir: project_dir.to_path_buf(),
        media_root,
        downloads,
        video_root,
        fupan,
        logs,
        database,
        live_save,
    }
}

/// 读主 `config.ini`；没有就退到 `.example`（首次运行、`config/` 只有模板的场景）。
fn read_main_ini(project_dir: &Path) -> Option<crate::config::IniFile> {
    let config_dir = project_dir.join("config");
    crate::config::IniFile::load(&config_dir.join("config.ini"))
        .or_else(|_| crate::config::IniFile::load(&config_dir.join("config.ini.example")))
        .ok()
}

/// `config.ini` 的 (mtime, 长度)。用它当缓存指纹：文件一改就重新解析。
fn config_stamp(project_dir: &Path) -> Option<(SystemTime, u64)> {
    let meta = std::fs::metadata(project_dir.join("config").join("config.ini")).ok()?;
    let mtime = meta.modified().ok()?;
    Some((mtime, meta.len()))
}

/// 相对路径统一转成绝对路径再当缓存键，避免 `./x` 与 `x` 各占一份缓存。
fn to_absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

/// 尽量规范化：能 `canonicalize` 就用它，否则「规范化最近的存在祖先 + 剩余相对段」。
///
/// 直接 `canonicalize` 在两种情况下会失败并让人退回原样：
/// - 目标目录还不存在（产物根指到全新目录、下载目录尚未创建）；
/// - macOS 上 `std::env::temp_dir()` 是 `/var/...`，而 `/var` 是指向 `/private/var`
///   的符号链接，只有真正存在的那一段才会被解析。
///
/// 后者会让「改写历史任务路径」这类功能写出半截未解析的路径，导致同一条记录在
/// 规范化比较时对不上。这里逐级往上找第一个能解析的祖先，再把剩下的段拼回去，
/// 无论目录存不存在都能给出稳定的绝对路径。
pub fn canonical_lenient(path: &Path) -> PathBuf {
    if let Ok(resolved) = path.canonicalize() {
        return resolved;
    }
    let mut suffix: Vec<std::ffi::OsString> = Vec::new();
    let mut current = path.to_path_buf();
    while let Some(parent) = current.parent().map(Path::to_path_buf) {
        if let Some(name) = current.file_name() {
            suffix.push(name.to_os_string());
        }
        if let Ok(resolved) = parent.canonicalize() {
            let mut out = resolved;
            for seg in suffix.iter().rev() {
                out.push(seg);
            }
            return out;
        }
        current = parent;
    }
    path.to_path_buf()
}

/// 状态页/doctor 用的路径清单：把所有可打开目录一次列全。
///
/// 桌面端「打开录制目录 / 打开复盘目录」按钮拿这份清单做白名单校验；
/// 以前按钮静默失效就是因为白名单写死了项目目录，而真实产物可能在别处。
pub fn known_roots(project_dir: &Path) -> HashMap<String, PathBuf> {
    let p = storage(project_dir);
    let mut map = HashMap::new();
    map.insert("project_dir".to_string(), p.project_dir.clone());
    map.insert("media_root".to_string(), p.media_root.clone());
    map.insert("downloads".to_string(), p.downloads.clone());
    map.insert("fupan_output".to_string(), p.fupan.clone());
    map.insert("logs".to_string(), p.logs());
    map.insert("covers".to_string(), p.covers());
    map.insert("run".to_string(), p.run());
    map
}

/// 允许交给复盘流水线的媒体根目录清单。
///
/// 复盘会调用本地 Python 处理这些文件，所以必须留一道边界：不是「本机任意路径」
/// 都能入队。但边界必须跟着配置走——以前它写死成 `<项目根>/downloads` 与
/// `<项目根>/fupan_output`，于是：
/// - 用户把产物根改到别的目录后，录完的直播永远进不了复盘；
/// - 用户在 `[录制设置] 直播保存路径` 填了自定义目录后，同样被拒。
///
/// 规则：
/// - 当前配置解析出来的 `downloads` / `fupan_output` 永远可信；
/// - **升级前的老位置（`<项目>/downloads`、`<项目>/fupan_output`）也一并纳入**：
///   产物改落点之后，用户手里那批老录像仍在项目目录里，不能因为搬家就读不了了；
/// - `产物根目录` 被改成别处时，把它本身也纳入（否则自定义根下的录像仍进不来）；
/// - `[录制设置] 直播保存路径` 填了也纳入（这是用户显式指定的录像落点）。
///
/// 刻意不无条件纳入 `media_root`：若用户把它指回项目目录，纳入它等于把整个项目目录
/// 都当成可信媒体来源，比原来的边界宽。
pub fn trusted_media_roots(project_dir: &Path) -> Vec<PathBuf> {
    let p = storage(project_dir);
    let mut roots = vec![p.downloads.clone(), p.fupan.clone()];
    // 升级前的旧布局：产物曾经就落在项目目录里。这些老文件不该因为改了存放位置
    // 就变成「不可复盘」——搬家是搬家，已存在文件的可读性不能跟着一起丢。
    // 只作为「可读来源」纳入，不影响新写入落点。
    roots.push(p.project_dir.join("downloads"));
    roots.push(p.project_dir.join("fupan_output"));
    // media_root 现在默认落在 `~/Documents/尊者产物`，纳入它不会把项目目录整片纳进来。
    // 只有用户把它显式指回项目目录时才跳过——那种情况等价于旧布局，边界照旧从严。
    if p.media_root != p.project_dir && p.media_root.parent().is_some() {
        roots.push(p.media_root.clone());
    }
    if let Some(live_save) = p.live_save.clone() {
        roots.push(live_save);
    }
    roots.sort();
    roots.dedup();
    roots
}

/// 判断某个媒体路径是否允许入队复盘。
///
/// 绝对路径必须落在某个可信根之下；相对路径只要不含 `..` 就放行（与旧行为一致：
/// 相对路径由下游按项目根解释，逃不出项目）。
///
/// 比对时**同时**看原样路径和宽松规范化后的路径。macOS 上 `/tmp` 是指向
/// `/private/tmp` 的符号链接：`Config::load` 的 `current_dir()` 会给出
/// `/private/tmp/...`，而用户在 config.ini 里手写的是 `/tmp/...`。只比字符串前缀
/// 会把同一目录判成两个，录像永远进不了复盘——正是这次要修掉的那类静默失败。
pub fn is_trusted_media_path(project_dir: &Path, file: &Path) -> bool {
    if !file.is_absolute() {
        return !file
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir));
    }
    let file_resolved = canonical_lenient(file);
    trusted_media_roots(project_dir)
        .iter()
        .any(|root| file.starts_with(root) || file_resolved.starts_with(canonical_lenient(root)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 建一个带最小配置的临时项目目录；`paths_ini` 是 `[路径设置]` 节内容。
    fn temp_project(name: &str, paths_ini: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "zunzhe-paths-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("config")).unwrap();
        let mut body = String::from("[录制设置]\n");
        body.push_str("[路径设置]\n");
        body.push_str(paths_ini);
        std::fs::write(root.join("config").join("config.ini"), body).unwrap();
        root
    }

    #[test]
    fn empty_config_puts_products_outside_the_project() {
        let root = temp_project("default", "");
        let p = storage(&root);
        assert_eq!(p.media_root, default_media_root());
        assert_ne!(
            p.media_root, root,
            "产物绝不能落在项目目录（源码）里——这正是这次要修掉的行为"
        );
        assert_eq!(p.downloads, default_media_root().join("downloads"));
        assert_eq!(p.video_root, default_media_root());
        assert_eq!(p.fupan, default_media_root().join("fupan_output"));
        assert_eq!(
            p.logs,
            default_media_root().join("logs"),
            "默认日志必须跟随产物根，不能新写到源码目录"
        );
        assert_eq!(
            p.database,
            default_media_root().join("database").join("zunzhe-core.db"),
            "默认数据库必须跟随产物根，旧库兼容由独立测试覆盖"
        );
        assert!(p.is_default_layout());
        assert!(!p.products_inside_project());
    }

    #[test]
    fn pointing_media_root_back_at_project_is_flagged() {
        let root = temp_project("backtoproject", "");
        let body = format!("[录制设置]\n[路径设置]\n产物根目录 = {}\n", root.display());
        std::fs::write(root.join("config").join("config.ini"), body).unwrap();
        let p = storage(&root);
        assert_eq!(p.media_root, root);
        assert!(!p.is_default_layout());
        assert!(
            p.products_inside_project(),
            "显式指回项目目录应被识别为旧布局，好让状态页提示用户"
        );
    }

    #[test]
    fn media_root_moves_all_products() {
        let root = temp_project("root", "产物根目录 = /tmp/我的产物\n");
        let p = storage(&root);
        assert_eq!(p.media_root, PathBuf::from("/tmp/我的产物"));
        assert_eq!(p.downloads, PathBuf::from("/tmp/我的产物/downloads"));
        assert_eq!(p.video_root, PathBuf::from("/tmp/我的产物"));
        assert_eq!(p.fupan, PathBuf::from("/tmp/我的产物/fupan_output"));
        assert_eq!(p.logs, PathBuf::from("/tmp/我的产物/logs"));
        assert_eq!(
            p.database,
            PathBuf::from("/tmp/我的产物/database/zunzhe-core.db")
        );
        assert!(!p.is_default_layout());
    }

    #[test]
    fn legacy_database_does_not_change_the_configured_database_path() {
        let root = temp_project("legacy-db", "产物根目录 = /tmp/zunzhe-legacy-products\n");
        let legacy = root.join("zunzhe-core.db");
        std::fs::write(&legacy, b"legacy").unwrap();

        // 路径解析必须稳定：无论旧库在不在，界面和目标路径始终指向新数据根。
        // 旧库的首次复制由 migrate_legacy_database_if_needed 单独负责。
        let new_database = PathBuf::from("/tmp/zunzhe-legacy-products/database/zunzhe-core.db");
        assert_eq!(storage(&root).database, new_database);
        let _ = std::fs::remove_dir_all("/tmp/zunzhe-legacy-products");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn tilde_expands_to_home() {
        let Some(home) = home_dir() else {
            return;
        };
        let root = temp_project("tilde", "产物根目录 = ~/尊者产物\n");
        assert_eq!(
            storage(&root).media_root,
            PathBuf::from(format!("{home}/尊者产物"))
        );
    }

    #[test]
    fn relative_path_is_relative_to_project() {
        let root = temp_project("rel", "产物根目录 = 数据/产物\n");
        assert_eq!(storage(&root).media_root, root.join("数据").join("产物"));
    }

    #[test]
    fn per_directory_override_beats_media_root() {
        let root = temp_project(
            "override",
            "产物根目录 = /tmp/根\n短视频下载目录 = /tmp/只改下载\n",
        );
        let p = storage(&root);
        assert_eq!(p.downloads, PathBuf::from("/tmp/只改下载"));
        assert_eq!(p.fupan, PathBuf::from("/tmp/根/fupan_output"));
    }

    #[test]
    fn custom_database_file_is_honoured() {
        let root = temp_project("db", "数据库文件 = /tmp/别的库.db\n");
        assert_eq!(storage(&root).database, PathBuf::from("/tmp/别的库.db"));
    }

    #[test]
    fn custom_logs_directory_is_honoured() {
        let root = temp_project("logs", "日志目录 = /tmp/别的日志\n");
        assert_eq!(storage(&root).logs, PathBuf::from("/tmp/别的日志"));
    }

    #[test]
    fn config_change_is_picked_up_without_restart() {
        let root = temp_project("reload", "产物根目录 = /tmp/先\n");
        assert_eq!(storage(&root).media_root, PathBuf::from("/tmp/先"));
        // 改了配置（mtime/长度变了）必须立刻生效，不能等重启。
        std::fs::write(
            root.join("config").join("config.ini"),
            "[路径设置]\n产物根目录 = /tmp/后\n",
        )
        .unwrap();
        assert_eq!(storage(&root).media_root, PathBuf::from("/tmp/后"));
    }

    #[test]
    fn missing_project_root_falls_back_to_defaults() {
        let root = std::env::temp_dir().join(format!(
            "zunzhe-paths-noconfig-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let p = storage(&root);
        // 连 config 都没有时，也必须落默认产物目录，而不是退回项目目录。
        assert_eq!(p.downloads, default_media_root().join("downloads"));
        assert_eq!(p.video_root, default_media_root());
        assert_eq!(p.fupan, default_media_root().join("fupan_output"));
        assert_ne!(p.media_root, root);
    }

    #[test]
    fn ensure_dirs_creates_every_product_dir() {
        let root = temp_project("ensure", "产物根目录 = \n");
        let media = root.join("自定义产物");
        std::fs::write(
            root.join("config").join("config.ini"),
            format!("[路径设置]\n产物根目录 = {}\n", media.display()),
        )
        .unwrap();
        ensure_dirs(&root).unwrap();
        for dir in [
            media.clone(),
            media.join("fupan_output"),
            media.join("fupan_output").join("covers"),
            media.join("fupan_output").join("run"),
        ] {
            assert!(dir.is_dir(), "应创建目录: {}", dir.display());
        }
        assert!(
            !media.join("downloads").exists(),
            "新布局不再预建 downloads 空壳目录（产物根下一级只能是主播目录）"
        );
        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&media).ok();
    }

    #[test]
    fn known_roots_lists_openable_directories() {
        let root = temp_project("roots", "产物根目录 = /tmp/产物根\n");
        let roots = known_roots(&root);
        assert_eq!(roots["media_root"], PathBuf::from("/tmp/产物根"));
        assert_eq!(roots["downloads"], PathBuf::from("/tmp/产物根/downloads"));
        assert_eq!(roots["project_dir"], root);
        assert_eq!(roots["logs"], PathBuf::from("/tmp/产物根/logs"));
    }

    #[test]
    fn trusted_media_paths_follow_the_configured_root() {
        let root = temp_project("trust", "产物根目录 = /tmp/zz-trust-根\n");
        // 自定义产物根下的录像必须被接受（旧实现只认 <项目根>/downloads，会拒掉它）。
        assert!(is_trusted_media_path(
            &root,
            Path::new("/tmp/zz-trust-根/downloads/B站直播/主播/x.ts")
        ));
        // 界外路径仍然拒绝，边界没有因为「可配置」而消失。
        assert!(!is_trusted_media_path(&root, Path::new("/etc/passwd")));
        assert!(!is_trusted_media_path(&root, Path::new("../../etc/passwd")));
        // 默认布局下 downloads / fupan_output 依旧可信。
        let plain = temp_project("trust-default", "");
        assert!(is_trusted_media_path(
            &plain,
            &plain.join("downloads").join("短视频").join("a.mp4")
        ));
        assert!(is_trusted_media_path(
            &plain,
            &plain.join("fupan_output").join("主播").join("b.txt")
        ));
    }

    #[test]
    fn trusted_media_path_accepts_custom_live_save_dir() {
        // 用户显式指定的录像目录（[录制设置] 直播保存路径）必须被接受，
        // 否则「录完自动复盘」这条链路在他改过录像位置后必然断掉。
        let root = temp_project("live-save", "产物根目录 = \n");
        std::fs::write(
            root.join("config").join("config.ini"),
            "[录制设置]\n直播保存路径(不填则默认) = /tmp/zz-live-save\n[路径设置]\n产物根目录 = \n",
        )
        .unwrap();
        assert!(is_trusted_media_path(
            &root,
            Path::new("/tmp/zz-live-save/抖音/主播/x.ts")
        ));
    }

    #[test]
    fn trusted_media_path_tolerates_tmp_symlink_alias() {
        // macOS: /tmp -> /private/tmp。配置里写 /tmp、调用方给 /private/tmp
        // 必须认为是同一处，否则录像会被白名单误拒。
        let root = temp_project("symlink", "产物根目录 = /tmp/zz-symlink-根\n");
        assert!(is_trusted_media_path(
            &root,
            Path::new("/private/tmp/zz-symlink-根/downloads/a.ts")
        ));
    }
}
