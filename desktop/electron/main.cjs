const { app, BrowserWindow, Tray, Menu, ipcMain, shell, nativeImage, Notification, protocol, net } = require('electron')
const path = require('path')
const { pathToFileURL } = require('url')
const { spawn, execFileSync } = require('child_process')
const fs = require('fs')
const os = require('os')
const { consumeNotifyQueue } = require('./local-notify.cjs')
const { healthcheckInvocation } = require('./healthcheck.cjs')
const pathUtils = require('./path-utils.cjs')
const updater = require('./update-run.cjs')
const { localFileUrl, parseLocalFileUrl, LOCAL_FILE_SCHEME } = require('./local-file-url.cjs')

// 兼容性加固：GPU 进程在虚拟机/远程桌面/低配环境下可能起不来，
// 若不禁用会触发 "GPU process isn't usable. Goodbye." 直接退出，导致窗口永不显示。
// 必须在创建任何窗口之前调用。同时显式禁用 GPU 进程并回退到软件渲染，
// 保证无 GPU 环境（远程桌面/虚拟机/老显卡）也能稳定弹出界面。
app.disableHardwareAcceleration()
app.commandLine.appendSwitch('disable-gpu')
app.commandLine.appendSwitch('enable-software-rasterizer')

// electron/ 在 desktop/ 下，Rust 核心在项目根目录（再上一级）。
// 打包态（app.isPackaged=true）下应用包内只读，所有可写数据（config/logs/
// downloads/fupan_output）都落到用户数据目录，Rust 二进制从 resourcesPath 读取。
// 注意：所有依赖 electronApp.getPath() 的值必须在 app ready 之后才可靠，
// 因此这里只用惰性 getter，避免顶层求值导致主进程启动即崩溃。
const PROJECT_DIR = path.resolve(__dirname, '..', '..')
function isPackaged() { return app.isPackaged }

// ---------- 数据目录解析（不再写死） ----------
// 打包态下 GUI 必须复用 daemon 真正读写的那个项目数据目录，否则界面读到的是空的
// zunzhe-core.db，主播/视频/复盘列表全部显示为空。
//
// 旧实现把目录硬编码成 `~/Projects/尊者监控直播短视频`，后果有两个：
//   1. 这个 .app 只在本机可用，换台机器/换个盘就废；
//   2. 用户没有任何办法把它指到自己想要的目录。
//
// 现在的解析顺序（先命中先用，`isPackaged()` 为 false 时直接用项目目录）：
//   1. 环境变量 ZUNZHE_PROJECT_DIR
//   2. `<userData>/数据目录.txt` 的第一行（托盘菜单「选择数据目录…」写入）
//   3. 自动探测：~/Projects/…、~/Desktop/…、挂载卷 /Volumes/*/…
//   4. app.getPath('userData')（首次运行、还没有任何项目数据时的兜底）
const DATA_DIR_POINTER_NAME = '数据目录.txt'
const PROJECT_NAME = '尊者监控直播短视频'
let dataDirCache = null
// 数据目录是否真的找到了（而不是掉到 userData 兜底）。
// 为 true 时**禁止**播种配置/建库——否则会凭空长出一套空数据。
let dataDirMissing = false
let dataDirTried = []

function dataDirPointerFile() {
  return path.join(app.getPath('userData'), DATA_DIR_POINTER_NAME)
}

// 路径判断的纯逻辑（含 ~ 别名、符号链接、候选顺序）都在 path-utils.cjs，
// 那边有独立单测，见 desktop/electron/path-utils.test.cjs。
const looksLikeProject = (dir) => pathUtils.looksLikeProject(dir)

function candidateDataDirs() {
  return pathUtils.candidateDataDirs({
    envDir: process.env.ZUNZHE_PROJECT_DIR,
    pointerFile: dataDirPointerFile(),
    home: require('os').homedir(),
    projectName: PROJECT_NAME,
  })
}

function runtimeDir() {
  if (!isPackaged()) return PROJECT_DIR
  if (dataDirCache) return dataDirCache
  const cands = candidateDataDirs()
  dataDirTried = cands
  const hit = pathUtils.firstProjectDir(cands)
  if (hit) {
    dataDirMissing = false
    dataDirCache = hit
    rememberDataDirPointer(hit)
  } else {
    // 一个候选都不像项目目录。
    //
    // 以前这里**静默**落到 app.getPath('userData') —— 那里通常躺着一套空的
    // zunzhe-core.db，于是界面全空、看起来「数据全没了」；更糟的是接着还会
    // 在那儿新建配置、daemon 再写一套新库，凭空长出一份分叉的数据。
    // 现在只标记出来，由 bootDataDirGuard() 明确报错并让用户指定，绝不静默初始化。
    dataDirMissing = true
    dataDirCache = app.getPath('userData')
    console.error('[数据目录] 所有候选都不可用，已暂停初始化，等用户指定:', cands)
  }
  return dataDirCache
}

/**
 * 固化数据目录：把最终命中的目录写进 `<userData>/数据目录.txt`。
 *
 * 不固化就会「每次启动都重新探测」——探测结果随插拔盘/符号链接状态变化，
 * 变一次用户就看到一次数据消失。写死指针后，除了用户主动改，
 * 启动路径永远是同一条。
 */
function rememberDataDirPointer(dir) {
  try {
    // 存真实路径而不是符号链接路径：候选里可能命中 `~/Projects/xxx` 这种指向
    // 移动硬盘的软链，一旦软链被删/断掉，指针就指向一个不存在的地方，
    // 下次启动会直接报「找不到数据目录」。realpath 拿到的才是数据的真实位置。
    let real = dir
    try { real = fs.realpathSync(dir) } catch (e) { /* 解析不了就退回原值 */ }
    const f = dataDirPointerFile()
    if (pathUtils.readPointerFile(f) === real) return
    fs.mkdirSync(path.dirname(f), { recursive: true })
    fs.writeFileSync(f,
      `# 界面读取的数据目录（首次命中后自动写入；托盘「选择数据目录…」可改）\n${real}\n`, 'utf8')
    console.log(`[数据目录] 已固化: ${real}`)
  } catch (e) {
    console.warn('[数据目录] 写指针文件失败（不影响本次运行）:', e.message)
  }
}

function resDir() { return isPackaged() ? process.resourcesPath : PROJECT_DIR }

// ---------- 可信路径 / 可打开目录 ----------
// 「打开目录」按钮曾经点了一直没反应：白名单写死成 [应用包目录, userData]，
// 而界面上要打开的路径是 `~/Projects/…`，不在白名单里，handler 直接返回字符串
// 又不抛异常，于是连报错都没有。
//
// 现在可信根由核心回答：启动时问一次 `zunzhe-core paths`，拿到产物目录的真实位置
// （用户可能把 `[路径设置] 产物根目录` 指到了任何地方），界面按钮就能开到正确位置。
// 纯路径判断放在 path-utils.cjs 里，那部分有独立单测。
let allowedRoots = []

function isTrustedPath(p) {
  return pathUtils.isTrustedPath(p, allowedRoots)
}

function resolveOpenablePath(p) {
  return pathUtils.resolveOpenablePath(p, allowedRoots)
}

/** 刷新可信根（启动时 + 配置变更后）。失败不影响既有白名单。 */
async function refreshAllowedRoots() {
  const roots = [
    path.resolve(PROJECT_DIR),
    path.resolve(app.getPath('userData')),
    path.resolve(runtimeDir()),
  ]
  try {
    const reported = await runCli(['paths'])
    for (const key of ['project_dir', 'media_root', 'downloads', 'fupan_output',
      'recordings', 'covers', 'run', 'database']) {
      const value = reported && reported[key]
      if (typeof value === 'string' && value.trim()) roots.push(path.resolve(value))
    }
    const db = reported && reported.database
    if (typeof db === 'string' && db.trim()) roots.push(path.dirname(path.resolve(db)))
    pathsCache = reported || pathsCache
  } catch (e) {
    console.warn('[paths] 未能解析产物目录，沿用默认白名单:', e.message)
  }
  allowedRoots = Array.from(new Set(roots))
  return allowedRoots
}

// ---------- 封面图解析 ----------
// 两条路径共用：`cover-url` 只负责把路径换成协议 URL，真正读文件发生在
// 下面的协议处理器里，那里会再拿 allowedRoots 校验一次。
const COVER_MIME = {
  '.jpg': 'image/jpeg', '.jpeg': 'image/jpeg', '.png': 'image/png',
  '.webp': 'image/webp', '.gif': 'image/gif', '.bmp': 'image/bmp',
}
// 单张封面超过 8MB 直接放弃：封面都是缩略图，真出现这种文件说明路径指错了，
// 与其把它交给渲染进程解码，不如让界面显示「无缓存」。
const COVER_MAX_BYTES = 8 * 1024 * 1024

function resolveCoverFile(p) {
  if (typeof p !== 'string' || !p.trim()) return null
  const raw = p.trim()
  const candidates = []
  if (path.isAbsolute(raw)) {
    candidates.push(raw)
  } else {
    // 相对路径：先按产物根解析，再按封面目录解析，最后按各可信根兜一遍。
    const roots = []
    if (pathsCache && typeof pathsCache.media_root === 'string' && pathsCache.media_root) {
      roots.push(pathsCache.media_root)
    }
    if (pathsCache && typeof pathsCache.covers === 'string' && pathsCache.covers) {
      roots.push(pathsCache.covers)
    }
    roots.push(...allowedRoots)
    for (const r of roots) candidates.push(path.join(r, raw))
  }
  for (const c of candidates) {
    const abs = path.resolve(c)
    if (!isTrustedPath(abs, allowedRoots)) continue
    let st
    try { st = fs.statSync(abs) } catch { continue }
    if (!st.isFile() || st.size <= 0 || st.size > COVER_MAX_BYTES) continue
    if (!COVER_MIME[path.extname(abs).toLowerCase()]) continue
    return abs
  }
  return null
}

// 历史视频封面等本地文件通过 `local-file://local/<编码后的绝对路径>` 提供。
// 不用 `file://` 直连：Electron 默认 webSecurity 会拦跨源 file 子资源；
// 也不把图片读成 base64 塞进 IPC：一屏几十张封面会把一次 status 的
// JSON-RPC 撑到好几 MB。协议只放行可信根（产物根 / 下载 / 复盘 / 封面 /
// 数据库目录，见 refreshAllowedRoots）之内的真实图片文件，其余一律拒绝，
// 避免把任意本地路径暴露给渲染进程。
function registerLocalFileProtocol() {
  protocol.handle(LOCAL_FILE_SCHEME, async (request) => {
    const resolved = parseLocalFileUrl(request.url, LOCAL_FILE_SCHEME)
    if (!resolved) return new Response('bad request', { status: 400 })
    if (!isTrustedPath(resolved, allowedRoots)) return new Response('forbidden', { status: 403 })
    if (!COVER_MIME[path.extname(resolved).toLowerCase()]) {
      return new Response('unsupported media type', { status: 415 })
    }
    try {
      if (!fs.statSync(resolved).isFile()) return new Response('not found', { status: 404 })
    } catch {
      return new Response('not found', { status: 404 })
    }
    return net.fetch(pathToFileURL(resolved).toString())
  })
}

const CORE_NAME = process.platform === 'win32' ? 'zunzhe-core.exe' : 'zunzhe-core'
function rustExe() {
  return isPackaged()
    ? path.join(resDir(), 'bin', CORE_NAME)
    : path.join(PROJECT_DIR, 'zunzhe-core', 'target', 'release', CORE_NAME)
}
const RUST_DEBUG_EXE = path.join(PROJECT_DIR, 'zunzhe-core', 'target', 'debug', CORE_NAME)
const DIST_HTML = path.join(__dirname, '..', 'dist', 'index.html')
const ASSETS = path.join(__dirname, '..', 'assets')
const ICON_ICO = path.join(ASSETS, 'icon.ico')
const ICON_PNG = path.join(ASSETS, 'icon.png')
const ICON_TRAY = path.join(ASSETS, 'icon-32.png')
const APP_NAME = '尊者直播监控'

// 必须在 app ready 之前声明特权 scheme，之后才能用 protocol.handle 接管它。
protocol.registerSchemesAsPrivileged([
  { scheme: LOCAL_FILE_SCHEME, privileges: { secure: true, supportFetchAPI: true, stream: true } },
])
// 核心上报的产物目录（`zunzhe-core paths` 的输出）。通知队列等运行态文件必须跟着
// 它走：用户把 `[路径设置] 产物根目录` 指到别处后，核心写队列的位置也变了，
// 界面若还按 <数据目录>/fupan_output 去找，就会永远读不到通知。
let pathsCache = null

function runDirResolved() {
  if (pathsCache && typeof pathsCache.run === 'string' && pathsCache.run) {
    return pathsCache.run
  }
  return path.join(runtimeDir(), 'fupan_output', 'run')
}
function notifyQueuePath() { return path.join(runDirResolved(), 'pending_notify.json') }
function notifySeenFilePath() { return path.join(runDirResolved(), 'desktop_seen_notify_ids.json') }
// 打包态下默认配置模板从 resourcesPath/config 复制；开发态直接用项目 config。
function configSrcDir() { return isPackaged() ? path.join(resDir(), 'config') : path.join(PROJECT_DIR, 'config') }

let mainWindow = null
let tray = null
let watchdogTimer = null
let notifyTimer = null
let updateReqTimer = null
let statusCache = null
let statusCacheAt = 0
const STATUS_CACHE_TTL_MS = 60000
// 这些命令会改变监控状态/文件列表，成功后必须让下次 status 重新全量扫描，
// 否则界面点“添加/下载/复盘”后可能继续显示旧快照。
const STATUS_INVALIDATING_COMMANDS = new Set([
  'add-live', 'remove-live', 'delete-live', 'record-live',
  'add-video', 'remove-video', 'download-video',
  'monitor-add', 'monitor-remove', 'monitor-set', 'check-now',
  'process', 'check-videos', 'ensure-services', 'start', 'stop',
  'wizard-done', 'set-app-prefs', 'set-config', 'set-config-bulk',
])
const isDev = !app.isPackaged && process.env.ELECTRON_DEV === '1'

function loadAppIcon() {
  const candidates = process.platform === 'win32'
    ? [ICON_ICO, ICON_TRAY, ICON_PNG]
    : [ICON_PNG, ICON_TRAY]
  for (const p of candidates) {
    if (!fs.existsSync(p)) continue
    const image = nativeImage.createFromPath(p)
    if (!image.isEmpty()) return image
  }
  console.warn('[尊者直播监控] 图标加载失败，请运行 npm run icons')
  return nativeImage.createEmpty()
}

/** Windows 托盘需要 16~32px 小图标，1024px 大图会显示空白 */
function trayIcon() {
  const trayPath = fs.existsSync(ICON_TRAY) ? ICON_TRAY : ICON_PNG
  const image = fs.existsSync(trayPath)
    ? nativeImage.createFromPath(trayPath)
    : loadAppIcon()
  if (image.isEmpty()) return image
  const size = process.platform === 'darwin' ? 22 : 32
  if (image.getSize().width === size) return image
  return image.resize({ width: size, height: size, quality: 'best' })
}

// 避免未捕获异常弹出系统级 "Error" 对话框
process.on('uncaughtException', (err) => console.error('[尊者直播监控]', err))
process.on('unhandledRejection', (reason) => console.error('[尊者直播监控]', reason))

function loadDistPage(win) {
  if (!win || win.isDestroyed()) return Promise.resolve()
  const ses = win.webContents.session
  return ses.clearCache().then(() => win.loadFile(DIST_HTML, {
    query: { t: String(Date.now()) },
  }))
}

function reloadUiWindow() {
  if (!mainWindow || mainWindow.isDestroyed()) return Promise.resolve({ ok: false })
  return loadDistPage(mainWindow).then(() => {
    if (mainWindow.isMinimized()) mainWindow.restore()
    mainWindow.show()
    mainWindow.focus()
    return { ok: true }
  })
}

// 需要联网抓平台数据的命令：慢是正常的，绝不能按本地命令的 30 秒一刀切。
//
// 「列出主播历史」（blogger-videos）要依次试 RSSHub → 官方 wbi/arc 接口 → yt-dlp
// 三套兜底，实测 B站单个主播约 80 秒；批量下载一次入队几十条也要时间。统一 30 秒
// 硬超时会把它们全部杀掉，用户看到的就是「Rust 核心命令超时」——这条命令本身没坏，
// 是被自己的超时误杀的。所以按命令给不同的上限，其余仍保持 30 秒。
const CLI_TIMEOUT_OVERRIDES_MS = {
  // 三兜底走完（RSSHub 40s + 官方接口 + yt-dlp 90s + 并发补全）最坏约 165s，
  // 给到 240s 留足余量；正常情况下 RSSHub 或官方接口几秒内就返回。
  'blogger-videos': 240000,
  'check-videos': 240000,
  'download-videos': 240000,
  'download-video': 240000,
  'monitor-add': 120000,
}

function cliTimeoutMs(cmd) {
  return CLI_TIMEOUT_OVERRIDES_MS[cmd] || 30000
}

function runCli(args) {
  return new Promise((resolve, reject) => {
    const exe = fs.existsSync(rustExe()) ? rustExe() : RUST_DEBUG_EXE
    if (!fs.existsSync(exe)) {
      return reject(new Error(`找不到 Rust 核心，请先构建：cd zunzhe-core && cargo build --release\n${rustExe()}`))
    }
    const timeoutMs = cliTimeoutMs(args[0])
    const proc = spawn(exe, args, {
      cwd: runtimeDir(),
      windowsHide: true,
    })
    const timer = setTimeout(() => {
      proc.kill('SIGTERM')
      // 3 秒后再强制结束，避免进程组残留。
      setTimeout(() => { try { proc.kill('SIGKILL') } catch (e) {} }, 3000)
      reject(new Error(`Rust 核心命令超时（${Math.round(timeoutMs / 1000)} 秒）: ${args[0]}`))
    }, timeoutMs)
    let stdout = ''
    let stderr = ''
    proc.on('error', (err) => reject(new Error(`无法启动后端: ${err.message}`)))
    proc.stdout.on('data', (d) => { stdout += d.toString('utf8') })
    proc.stderr.on('data', (d) => { stderr += d.toString('utf8') })
    proc.on('close', (code) => {
      clearTimeout(timer)
      const text = stdout.trim()
      if (!text) {
        return reject(new Error(stderr || `命令失败 exit=${code}`))
      }
      try {
        resolve(JSON.parse(text.split('\n').pop()))
      } catch (e) {
        reject(new Error(text || stderr || e.message))
      }
    })
  })
}

// 桌面端界面每 30 秒调一次 status，而 status 会递归扫描 downloads /
// fupan_output。CLI 每次都是新进程，Rust 进程内缓存帮不上忙；这里在主进程
// 按 60 秒复用完整快照，界面轮询最多每两次共享一次全量扫描，录制/复盘
// 完成通常远大于 60 秒，最近列表不会长期失真。
async function runStatusCached() {
  const now = Date.now()
  if (statusCache && now - statusCacheAt < STATUS_CACHE_TTL_MS) {
    return statusCache
  }
  const st = await runCli(['status'])
  statusCache = st
  statusCacheAt = now
  return st
}

function invalidateStatusCache() {
  statusCache = null
  statusCacheAt = 0
}

function showLocalNotification(title, body) {
  if (!Notification.isSupported()) return
  const n = new Notification({
    title: title || APP_NAME,
    body: body || '',
    icon: fs.existsSync(ICON_ICO) ? ICON_ICO : ICON_PNG,
  })
  n.on('click', () => createWindow())
  n.show()
}

function pollLocalNotifyQueue() {
  try {
    consumeNotifyQueue(
      notifyQueuePath(),
      notifySeenFilePath(),
      (title, body) => showLocalNotification(title, body),
      (e) => {
        // Rust daemon 可能正在原子替换通知队列文件：短暂读到半截 JSON 时
        // 跳过本轮，下一轮 4 秒后重新读取，不能把桌面主循环打崩。
        console.warn('[local-notify] 队列暂不可读，下一轮重试:', e.message)
      },
    )
  } catch (e) {
    console.warn('[local-notify]', e.message)
  }
}

async function serviceWatchdog() {
  try {
    pollLocalNotifyQueue()
    // 重启职责只归一条链路：launchd 负责进程级拉起，看门狗脚本
    // （scripts/check-rust-daemon.sh，内部会调 ensure-services）负责语义级
    // 判定与拉起。
    //
    // 这里原来在踢脚本之外**又自己调了一次 ensure-services**，于是同一台
    // 机器上三套看门狗各拉各的（launchd KeepAlive + LaunchAgents 的
    // zunzhe-healthcheck.sh + GUI），历史上出现过一天 57 次重启、
    // err.log 累计 220 次 rust-daemon start。现在 GUI 只负责按节拍踢脚本
    // 和刷新自己的看板，不再发起重启，避免竞争。
    const hc = healthcheckInvocation(PROJECT_DIR, process.platform)
    if (hc) {
      const { execFile } = require('child_process')
      execFile(hc.shell, hc.args, { timeout: 10000 }, () => {})
      return
    }
    // 只有拿不到看门狗脚本时才兜底自拉起（便携运行、Windows 未装计划任务等），
    // 否则会变成完全没人管。
    const prefRes = await runCli(['get-app-prefs'])
    const prefs = prefRes.prefs || {}
    if (prefs.watchdog === false) return
    // 健康检查只关心 daemon 是否存活，不加载完整 status 快照（后者会递归
    // 扫描 downloads / fupan_output，录像和复盘产物多了以后每分钟拖慢看板）。
    const st = await runCli(['service-status'])
    const rust = st.rust_daemon || st.rust_daemon_running
    if (!rust) {
      const r = await runCli(['ensure-services'])
      if (r.restarted?.length) {
        console.log('[watchdog] 无外部看门狗脚本，已自行拉起服务:', r.restarted.join(', '))
      }
    }
  } catch (e) {
    console.warn('[watchdog]', e.message)
  }
}

function startBackgroundTasks() {
  if (watchdogTimer) clearInterval(watchdogTimer)
  if (notifyTimer) clearInterval(notifyTimer)
  if (updateReqTimer) clearInterval(updateReqTimer)
  watchdogTimer = setInterval(serviceWatchdog, 60000)
  notifyTimer = setInterval(pollLocalNotifyQueue, 4000)
  updateReqTimer = setInterval(pollUpdateRequest, 10000)
  serviceWatchdog()
  pollUpdateRequest()
}

async function applyStartupPrefs() {
  try {
    const prefRes = await runCli(['get-app-prefs'])
    const prefs = prefRes.prefs || {}
    if (prefs.autoStartServices !== false) {
      await runCli(['ensure-services'])
    }
    if (process.platform === 'win32' || process.platform === 'darwin') {
      app.setLoginItemSettings({
        openAtLogin: !!prefs.openAtLogin,
        openAsHidden: true,
      })
    }
  } catch (e) {
    console.warn('[startup]', e.message)
  }
}

function createWindow() {
  if (mainWindow) {
    if (mainWindow.isMinimized()) mainWindow.restore()
    mainWindow.show()
    mainWindow.focus()
    return
  }
  mainWindow = new BrowserWindow({
    width: 1180,
    height: 860,
    minWidth: 960,
    minHeight: 640,
    title: APP_NAME,
    icon: loadAppIcon(),
    show: false,
    autoHideMenuBar: true,
    webPreferences: {
      preload: path.join(__dirname, 'preload.cjs'),
      contextIsolation: true,
      nodeIntegration: false,
    },
  })
  if (isDev) {
    mainWindow.loadURL('http://127.0.0.1:5173')
  } else if (fs.existsSync(DIST_HTML)) {
    loadDistPage(mainWindow)
  } else {
    mainWindow.loadURL(`data:text/html;charset=utf-8,${encodeURIComponent(
      '<body style="font-family:sans-serif;padding:24px">'
      + '<h2>尊者直播监控</h2>'
      + '<p>界面文件尚未构建，请关闭后重新运行「启动尊者直播监控.bat」。</p>'
      + '</body>'
    )}`)
  }

  // 兜底：页面加载失败也弹窗，避免"双击没反应"
  mainWindow.webContents.on('did-fail-load', (_e, code, desc) => {
    console.warn('[尊者直播监控] 界面加载失败', code, desc)
    if (!mainWindow.isDestroyed() && !mainWindow.isVisible()) {
      mainWindow.show()
      mainWindow.focus()
    }
  })
  mainWindow.once('ready-to-show', () => {
    mainWindow.show()
    mainWindow.focus()
  })
  // 兜底：某些环境下 ready-to-show 可能迟迟不触发，1.5 秒后强制显示窗口
  setTimeout(() => {
    if (mainWindow && !mainWindow.isDestroyed() && !mainWindow.isVisible()) {
      console.warn('[尊者直播监控] ready-to-show 超时，强制显示窗口')
      mainWindow.show()
      mainWindow.focus()
    }
  }, 1500)
  mainWindow.on('close', (e) => {
    if (!app.isQuitting) {
      e.preventDefault()
      mainWindow.hide()
    }
  })
  mainWindow.on('closed', () => { mainWindow = null })
}

/**
 * 让用户把整个数据目录指到别处（托盘菜单「选择数据目录…」）。
 *
 * 写一行到 `<userData>/数据目录.txt` 作为指针文件，然后重启界面进程。
 * 用指针文件而不是写死路径，是为了让打包好的 .app 能跟着数据走。
 *
 * 注意：daemon 由 launchd 按它自己的 WorkingDirectory 启动，不受这里影响。
 * 如果两边指向不同目录，界面会显示另一份数据——所以选择时必须以「daemon 正在
 * 读写的那份」为准，下面的对话框会明确提示这一点。
 */
async function chooseDataDir() {
  const { dialog } = require('electron')
  const current = runtimeDir()
  const res = await dialog.showOpenDialog({
    title: '选择数据目录',
    message: '选 daemon 正在运行的那个项目目录（含 config/config.ini）',
    defaultPath: current,
    properties: ['openDirectory', 'createDirectory'],
    buttonLabel: '使用此目录',
  })
  if (res.canceled || !res.filePaths || !res.filePaths.length) return { ok: false, canceled: true }
  const picked = res.filePaths[0]
  if (!looksLikeProject(picked)) {
    await dialog.showMessageBox({
      type: 'warning',
      message: '这个目录不是数据目录',
      detail: `里面既没有 config/config.ini，也没有 zunzhe-core.db：\n${picked}\n\n`
        + `请选 daemon 正在运行的目录，否则界面会读到另一份（空的）数据。`,
      buttons: ['好'],
    })
    return { ok: false, error: 'not-a-project-dir' }
  }
  try {
    fs.mkdirSync(path.dirname(dataDirPointerFile()), { recursive: true })
    fs.writeFileSync(dataDirPointerFile(), `# 界面读取的数据目录（本项目由「选择数据目录…」写入）\n${picked}\n`, 'utf8')
  } catch (e) {
    console.error('[choose-data-dir] 写入指针文件失败:', e.message)
    return { ok: false, error: e.message }
  }
  dataDirCache = picked
  pathsCache = null
  console.log(`[尊者直播监控] 数据目录已切换到: ${picked}，正在重启界面…`)
  app.relaunch()
  app.exit(0)
  return { ok: true, dataDir: picked }
}

function createTray() {
  const image = trayIcon()
  tray = new Tray(image.isEmpty() ? nativeImage.createEmpty() : image)
  tray.setToolTip(APP_NAME)
  const menu = Menu.buildFromTemplate([
    { label: '显示主窗口', click: createWindow },
    { label: '重新加载界面', click: () => reloadUiWindow() },
    { type: 'separator' },
    // 数据目录不再写死：这里给用户一个真正的入口去换目录。
    { label: '选择数据目录…', click: () => { chooseDataDir().catch((e) => console.warn('[choose-data-dir]', e.message)) } },
    { label: '打开数据目录', click: () => { shell.openPath(runtimeDir()) } },
    { type: 'separator' },
    // 检查更新：只换程序，不碰数据目录。更新源 = <数据目录>/releases/
    { label: '检查更新…', click: () => { checkForUpdateInteractive().catch((e) => console.warn('[check-update]', e.message)) } },
    { label: '打开 releases 文件夹', click: () => { shell.openPath(releasesDir()) } },
    { type: 'separator' },
    { label: '退出', click: () => { app.isQuitting = true; app.quit() } },
  ])
  tray.setContextMenu(menu)
  tray.on('double-click', createWindow)
}

app.setName(APP_NAME)
if (process.platform === 'win32') {
  app.setAppUserModelId('com.zunzhe.livemonitor')
}

const gotLock = app.requestSingleInstanceLock()
if (!gotLock) {
  // 已有实例在跑：退出本进程，由已有实例的 second-instance 事件弹出窗口
  app.quit()
} else {
  app.on('second-instance', () => {
    if (mainWindow) {
      reloadUiWindow()
    } else {
      app.whenReady().then(createWindow)
    }
  })
}

// 打包态下应用包内只读，首次运行把默认配置模板复制到用户数据目录，
// 并创建 daemon 需要的子目录。开发态直接使用项目目录，无需复制。
function ensureRuntime() {
  if (!isPackaged()) return
  if (dataDirMissing) {
    // 绝不在这里 mkdir：目录还没确定就建目录、播种配置，等于亲手造一套空数据出来。
    console.warn('[ensure-runtime] 跳过：数据目录尚未确定')
    return
  }
  const RD = runtimeDir()
  const mk = (p) => { if (!fs.existsSync(p)) fs.mkdirSync(p, { recursive: true }) }
  for (const sub of ['config', 'logs', 'downloads', 'recordings', 'fupan_output', 'fupan_output/run', 'rsshub']) {
    mk(path.join(RD, sub))
  }
  // 复制默认配置（仅复制不存在的，避免覆盖用户已改过的配置）
  const CSD = configSrcDir()
  if (fs.existsSync(CSD)) {
    for (const f of fs.readdirSync(CSD)) {
      const src = path.join(CSD, f)
      const dst = path.join(RD, 'config', f)
      if (fs.statSync(src).isFile() && !fs.existsSync(dst)) {
        try { fs.copyFileSync(src, dst) } catch (e) { console.warn('[ensure-runtime]', e.message) }
      }
    }
  }
  // daemon 的 find_binary 只认 project_dir/ffmpeg/ffmpeg：把内嵌 ffmpeg 复制到 userData。
  const ffDir = path.join(RD, 'ffmpeg')
  mk(ffDir)
  const ffName = process.platform === 'win32' ? 'ffmpeg.exe' : 'ffmpeg'
  const ffSrc = path.join(resDir(), 'ffmpeg', ffName)
  const ffDst = path.join(ffDir, ffName)
  if (fs.existsSync(ffSrc) && !fs.existsSync(ffDst)) {
    try { fs.copyFileSync(ffSrc, ffDst); fs.chmodSync(ffDst, 0o755) }
    catch (e) { console.warn('[ensure-runtime] 复制 ffmpeg 失败:', e.message) }
  }
  // 打包态下从 userData 读配置：复制完再给 daemon 设 cwd 的 RUNTIME_DIR 已就绪。
  console.log(`[尊者直播监控] 打包模式，数据目录: ${RD}`)
}

// ---------- 检查更新（只换程序，不碰数据） ----------
// 更新源 = <数据目录>/releases/，离线可用、不需要服务器。
// 程序目录和数据目录是物理分离的两处，所以「点更新」不可能导致数据消失
// —— 这正是根治「更新一次数据像丢一次」的关键。
/**
 * 当前程序本体路径（要被新版替换的那个）。
 *
 * 只在**打包态**且形态符合 macOS .app 时返回路径，否则返回 null。
 * 原因：开发态 `process.execPath` 指向 node_modules 里的 Electron.app，
 * 上溯三级得到的是 Electron 本体而不是本软件；一旦照常执行替换，
 * 会把开发用的 Electron.app 改名成 `.zunzhe-old-*`（开发机直接跑不起来）。
 * 非 macOS 打包形态同理：本更新器用 ditto/codesign，只认 .app。
 */
function appInstallPath() {
  if (!app.isPackaged) return null
  // /Applications/X.app/Contents/MacOS/X → /Applications/X.app
  const p = path.resolve(process.execPath, '..', '..', '..')
  if (process.platform !== 'darwin') return null
  if (!p.endsWith('.app')) return null
  return p
}

function releasesDir() {
  return path.join(runtimeDir(), updater.RELEASES_DIR_NAME)
}

async function checkForUpdateInteractive(opts = {}) {
  // quiet=true：界面按钮调用。没有更新时不用弹窗打断，交给界面 toast 提示。
  const quiet = !!opts.quiet
  const { dialog } = require('electron')
  const installPath = appInstallPath()
  const current = app.getVersion()
  const rd = releasesDir()
  if (!installPath) {
    // 开发态 / 非 macOS 打包形态：更新器只替换 macOS .app，这里明确拒绝，
    // 不能拿 execPath 上溯出来的路径去改名，否则会搞坏开发环境。
    const why = app.isPackaged
      ? `更新器只支持 macOS 的 .app 形态（当前平台 ${process.platform}）`
      : '开发模式不执行替换更新（打包后的程序才可以自我更新）'
    if (!quiet) await dialog.showMessageBox({ type: 'warning', message: '当前形态不支持就地更新', detail: why, buttons: ['好'] })
    return { ok: false, current, latest: null, hasUpdate: false, releasesDir: rd, error: why }
  }
  const r = updater.checkForUpdate({ releasesDir: rd, currentVersion: current })
  if (!r.ok) {
    if (!quiet) await dialog.showMessageBox({ type: 'warning', message: '检查更新失败', detail: r.error || '', buttons: ['好'] })
    return r
  }
  if (!r.latest || !r.hasUpdate) {
    if (!quiet) {
      await dialog.showMessageBox({
        message: '已是最新版本',
        detail: `当前版本 ${current}\n更新源 ${rd}\n\n${r.note ? `${r.note}\n\n` : ''}`
          + '把新版（.app 或 .zip）放进上面的 releases 文件夹，再点一次即可。',
        buttons: ['好'],
      })
    }
    return r
  }
  const res = await dialog.showMessageBox({
    type: 'question',
    message: `发现新版本 ${r.latest.version}`,
    detail: `当前 ${current} → 新版 ${r.latest.version}\n来源 ${r.latest.name}\n\n`
      + `只替换程序：${installPath}\n数据目录完全不动：${runtimeDir()}`,
    buttons: ['立即更新并重启', '稍后'], defaultId: 0, cancelId: 1, noLink: true,
  })
  if (res.response !== 0) return r
  const a = updater.applyRelease({ release: r.latest, installPath, log: (m) => console.log('[更新]', m) })
  if (!a.ok) {
    await dialog.showMessageBox({ type: 'error', message: '更新失败（已回滚，程序保持原样）', detail: a.error || '', buttons: ['好'] })
    return { ...r, applied: false, error: a.error }
  }
  await dialog.showMessageBox({
    message: '更新完成',
    detail: `已更新到 ${a.version}，程序将自动重启。\n你的数据（${runtimeDir()}）没有被改动。`,
    buttons: ['好'],
  })
  app.relaunch()
  app.exit(0)
  return { ...r, applied: true }
}

// MCP 触发更新通道：轮询 <数据根>/releases/.update-request.json。
// 智能体通过 daemon 的 apply_update 工具写这个信号文件，桌面端认领后
// 走一次交互式检查更新（发现更高版本 → 确认框 → 替换 .app 并重启，不碰数据）。
// 用轮询而不是 fs.watch：更新源在外置盘，kqueue 对外置卷不可靠，10s 轮询开销可忽略。
function pollUpdateRequest() {
  try {
    const rd = releasesDir()
    const reqFile = path.join(rd, '.update-request.json')
    if (!fs.existsSync(reqFile)) return
    let req = null
    try { req = JSON.parse(fs.readFileSync(reqFile, 'utf8')) } catch (e) { req = { parseError: String((e && e.message) || e) } }
    try { fs.unlinkSync(reqFile) } catch (e) { console.warn('[更新] 信号文件删除失败', e.message) }
    console.log('[更新] 收到 MCP 更新请求', req)
    checkForUpdateInteractive().catch((e) => console.warn('[check-update]', e.message))
  } catch (e) { console.warn('[更新] 更新请求处理失败', e.message) }
}

/**
 * 启动前的数据目录守卫。
 *
 * 找不到任何已初始化的数据目录时，**不初始化、不开窗口**，直接明确报错，
 * 让用户在「选目录」和「退出」之间做选择。这样绝不会出现
 * 「悄悄用了空的 userData 库，用户以为数据丢了」。
 */
async function bootDataDirGuard() {
  if (!isPackaged()) return true
  runtimeDir() // 触发一次解析，填充 dataDirMissing
  if (!dataDirMissing) return true
  const { dialog } = require('electron')
  const tried = dataDirTried.map((d) => `  • ${d}`).join('\n')
  const res = await dialog.showMessageBox({
    type: 'error',
    title: '找不到数据目录',
    message: '没有找到已初始化的项目数据目录',
    detail: '以下位置都没有 config/config.ini 或 zunzhe-core.db：\n\n'
      + `${tried}\n\n`
      + '为避免误建空数据目录把你的数据弄丢，程序不会自动初始化任何目录。\n'
      + '请先确认移动硬盘已挂载，或手动指到你的项目目录。',
    buttons: ['选择数据目录…', '退出'],
    defaultId: 0, cancelId: 1, noLink: true,
  })
  if (res.response === 1) { app.isQuitting = true; app.exit(1); return false }
  try { await chooseDataDir() } catch (e) { console.warn('[数据目录]', e.message) }
  return false
}

app.whenReady().then(async () => {
  if (!gotLock) return
  // 先确认数据目录，再谈别的：这一步没过就什么都不做。
  if (!(await bootDataDirGuard())) return
  const installRoot = appInstallPath()
  if (installRoot) {
    // 清掉上一次更新留下的 `.zunzhe-old-*` / `.zunzhe-update-*`（只删自己造的前缀）
    // 只在本体是已打包的 .app 时才做：其余形态下没有"安装目录"可言。
    const removed = updater.cleanupStale(path.dirname(installRoot),
      (m) => console.warn('[更新]', m))
    if (removed.length) console.log('[更新] 已清理上次更新残留:', removed.join(', '))
  }
  ensureRuntime()
  // 先问核心「产物到底在哪个目录」，再注册 IPC：白名单必须在第一个
  // open-path 调用之前就绪，否则界面刚打开那一下点「打开保存目录」仍会被拒。
  try { await refreshAllowedRoots() } catch (e) { console.warn('[paths]', e.message) }
  // 协议处理器依赖 allowedRoots 做校验，放在刷新之后注册。
  registerLocalFileProtocol()
  createTray()

  // ---------- 安全加固：IPC 入参校验（纵深防御） ----------
  // renderer 仅加载本地打包页面，但一旦被 XSS/恶意内容污染，未校验的
  // open-external / read-text / open-path 可被滥用为任意 URL 打开、
  // 任意文件读取等。此处做协议与路径白名单兜底，避免 renderer 越权。
  //
  // 白名单本身来自 `refreshAllowedRoots()`（模块级 isTrustedPath），
  // 所以产物目录被配置到项目之外时，按钮能开到正确位置而不会被静默拒掉。
  const isSafeExternalUrl = (u) => {
    if (typeof u !== 'string' || !u) return false
    try {
      const parsed = new URL(u)
      return parsed.protocol === 'http:' || parsed.protocol === 'https:'
    } catch { return false }
  }

  // 渲染进程能触达的 Rust 子命令白名单。
  //
  // 同文件对 open-path / read-text 都写了「纵深防御」，唯独 cli 这条最宽的
  // 通道原来是任意 `cmd + args` 直透二进制：界面一旦被污染（加载了被篡改的
  // 页面内容、拼进不可信字符串），就能调用任意子命令。
  //
  // 集合取自界面真实用到的命令，范围是 `desktop/src/**/*.{jsx,js}` 全量扫描
  // （不是只看 App.jsx —— 2026-09-17 就是因为漏扫了 LogsPanel.jsx 与 Wizard.jsx，
  // 把「运行日志」和「安装向导」两个按钮直接点了报错）。
  //
  // 仍然刻意排除风险最高的那几个：record-live（直接拉起录制）、doctor（诊断会
  // 改状态）、remove-* 之外的删除类内部调用等。内部调用（paths / status /
  // ensure-services / doctor）平时走 runCli，不经过这条 IPC。
  //
  // 逐条放行理由：
  //   notify-prune  —— 「通知治理」面板的用户显式动作（按钮前有 Popconfirm 二次确认）
  //   tail-logs     —— 只读，读的是本项目 logs/ 下的日志尾部，无副作用
  //   ensure-services —— 安装向导最后一步「初始化完成」按的就是它；它是用户显式
  //                      点击、只能让本应用自己的二进制去拉起它自己那几个服务，
  //                      拦掉等于向导永远走不完
  const ALLOWED_CLI_COMMANDS = new Set([
    // 直播源管理
    'add-live', 'remove-live', 'delete-live', 'check-live',
    // 视频源管理
    'add-video', 'remove-video', 'blogger-videos', 'download-video',
    'download-videos', 'check-videos', 'process',
    // 统一监控主命令（产品手册第 5 章；前端迁移后旧 add-*/remove-* 逐步退役）
    'monitor-add', 'monitor-remove', 'monitor-list', 'monitor-set',
    'check-now', 'anchor-search',
    // 状态与配置
    'status', 'get-config', 'set-config', 'get-app-prefs', 'set-app-prefs',
    'set-notify',
    // 通知与邮箱
    'email-status', 'test-email',
    'notify-list', 'notify-retry', 'notify-prune',
    // 服务生命周期与引导
    'start', 'stop', 'wizard-done', 'ensure-services',
    // 运行日志（只读）
    'tail-logs',
  ])

  ipcMain.handle('cli', (_, cmd, ...args) => {
    if (typeof cmd !== 'string' || !ALLOWED_CLI_COMMANDS.has(cmd)) {
      // 明确报错而不是静默失败：真漏了命令要能立刻看见，也便于加白名单。
      console.warn('[cli] 拒绝未在白名单内的命令:', cmd)
      return Promise.reject(new Error(`不允许的命令: ${cmd}`))
    }
    if (cmd === 'status') return runStatusCached()
    if (STATUS_INVALIDATING_COMMANDS.has(cmd)) {
      invalidateStatusCache()
      // 用户可能刚改了 `[路径设置] 产物根目录`；产物目录跟着变，
      // 白名单也要跟着刷新，否则新目录下的「打开目录」会立刻失效。
      refreshAllowedRoots().catch((e) => console.warn('[paths]', e.message))
    }
    return runCli([cmd, ...args])
  })
  ipcMain.handle('reload-ui', () => reloadUiWindow())
  ipcMain.handle('set-login-item', async (_, enabled) => {
    const open = !!enabled
    if (process.platform === 'win32' || process.platform === 'darwin') {
      app.setLoginItemSettings({ openAtLogin: open, openAsHidden: true })
    }
    await runCli(['set-app-prefs', JSON.stringify({ openAtLogin: open })])
    return { ok: true, openAtLogin: open }
  })
  ipcMain.handle('get-login-item', () => {
    if (process.platform === 'win32' || process.platform === 'darwin') {
      return app.getLoginItemSettings()
    }
    return { openAtLogin: false }
  })
  ipcMain.handle('show-local-notify', (_, title, body) => {
    showLocalNotification(title, body)
    return { ok: true }
  })
  // 打开文件/目录：按「文件类型」挑正确的本地程序打开，而不是交给系统默认关联。
  //
  // 为什么要这么改：以前直接 shell.openPath，走的是 macOS「默认打开方式」。
  // 如果用户曾经把 .mp4/.txt 默认关联到某个代码编辑器（比如 Trae），点开就会在
  // Trae 里打开并弹「格式不对」——这正是被吐槽的点。现在改成：
  //   - 视频 → VLC / IINA / QuickTime Player（按安装顺序挑第一个存在的）
  //   - 音频 → Music / VLC / QuickTime Player
  //   - 图片 → Preview
  //   - 文本(.txt/.srt/.md/.json/.csv…) → TextEdit
  //   - 其它未知类型 / 首选 App 都没装 → 在访达里定位，而不是用错的程序硬开
  // 这样「视频就开视频、文本就开文本」，不会再弹出编程语言编辑器。
  function openWithPreferredApps(target, appNames) {
    for (const appName of appNames) {
      try {
        execFileSync('open', ['-a', appName, target], { timeout: 8000 })
        return true
      } catch {
        // 该 App 未安装或启动失败，尝试下一个候选。
      }
    }
    return false
  }
  const MEDIA_VIDEO_EXTS = ['.mp4', '.mkv', '.mov', '.m4v', '.ts', '.m2ts', '.flv', '.webm', '.avi', '.mpg', '.mpeg', '.wmv', '.rmvb', '.3gp']
  const MEDIA_AUDIO_EXTS = ['.mp3', '.m4a', '.wav', '.aac', '.flac', '.ogg', '.wma', '.opus']
  const IMAGE_EXTS = ['.png', '.jpg', '.jpeg', '.gif', '.webp', '.bmp', '.heic', '.tiff']
  const TEXT_EXTS = ['.txt', '.md', '.markdown', '.srt', '.ass', '.ssa', '.vtt', '.lrc', '.json', '.csv', '.log', '.xml', '.yaml', '.yml', '.sub']
  ipcMain.handle('open-path', async (_, p) => {
    const target = resolveOpenablePath(p)
    if (!target) {
      // 不能再静默返回：以前返回一个字符串、渲染层又不看返回值，用户看到的就是
      // 「点了没反应」。这里至少留下日志和一条系统通知，让失败可被看见。
      const msg = '无法打开：该路径不在已知的数据/产物目录内'
      console.warn('[open-path]', msg, '=>', p)
      showLocalNotification('无法打开目录', `${p || '(空路径)'}\n${msg}`)
      return msg
    }
    // 目录永远用访达打开（Finder，不可能是 Trae），保持原来的「打开文件夹」体验。
    let st = null
    try { st = fs.statSync(target) } catch { /* 不存在就走下面的兜底 */ }
    if (st && st.isDirectory()) {
      const err = await shell.openPath(target)
      if (err) {
        console.warn('[open-path] 打开目录失败', target, err)
        showLocalNotification('无法打开目录', `${target}\n${err}`)
        return err
      }
      return ''
    }
    const ext = path.extname(target).toLowerCase()
    let opened = false
    if (MEDIA_VIDEO_EXTS.includes(ext)) {
      opened = openWithPreferredApps(target, ['VLC', 'IINA', 'QuickTime Player'])
    } else if (MEDIA_AUDIO_EXTS.includes(ext)) {
      opened = openWithPreferredApps(target, ['Music', 'VLC', 'QuickTime Player'])
    } else if (IMAGE_EXTS.includes(ext)) {
      opened = openWithPreferredApps(target, ['Preview'])
    } else if (TEXT_EXTS.includes(ext)) {
      opened = openWithPreferredApps(target, ['TextEdit'])
    }
    if (!opened) {
      // 未知类型或首选 App 都没装上：在访达里定位，而不是用错的程序硬开。
      shell.showItemInFolder(target)
    }
    return ''
  })
  // 在访达里定位文件：这才是用户说的「直接跑到那个地址」。
  //
  // 与 open-path 的分工要分清：
  //   open-path  = 打开（目录交给访达，文件交给系统默认程序）
  //   show-item  = 定位（打开父目录并高亮选中那个文件，用户一眼看到它躺在哪）
  // 产物路径可能还没生成（刚改完存放位置还没跑过一轮），所以同样走 resolveOpenablePath，
  // 失败时给系统通知而不是静默——「点了没反应」是这个界面历史上被骂最多的毛病。
  ipcMain.handle('show-item', (_, p) => {
    const target = resolveOpenablePath(p)
    if (!target) {
      const msg = '无法定位：该路径不在已知的数据/产物目录内'
      console.warn('[show-item]', msg, '=>', p)
      showLocalNotification('无法定位文件', `${p || '(空路径)'}\n${msg}`)
      return msg
    }
    shell.showItemInFolder(target)
    return ''
  })
  // 列一层目录：主播详情页要显示「这个主播到底存了哪些文件」。
  //
  // 只读、只列一层、只列白名单内的目录——不做递归（递归一个大目录会把主进程卡住）。
  // 目录不存在时返回空列表而不是抛错：刚添加还没跑过一轮的主播，本来就该是空的。
  ipcMain.handle('list-dir', (_, p) => {
    const target = resolveOpenablePath(p)
    if (!target) return { ok: false, error: '该路径不在已知的数据/产物目录内', items: [] }
    let st
    try { st = fs.statSync(target) } catch (e) { return { ok: false, error: '路径不存在', items: [] } }
    if (!st.isDirectory()) return { ok: false, error: '不是文件夹', items: [] }
    try {
      const items = []
      for (const ent of fs.readdirSync(target, { withFileTypes: true })) {
        if (ent.name.startsWith('.')) continue
        if (items.length >= 400) break
        let size = 0
        let mtime = 0
        try {
          const s = fs.statSync(path.join(target, ent.name))
          size = s.size
          mtime = s.mtimeMs
        } catch { /* 断链/无权限：留 0，界面显示为未知 */ }
        items.push({ name: ent.name, dir: ent.isDirectory(), size, mtime, path: path.join(target, ent.name) })
      }
      // 文件夹在前，同类按时间新到旧——用户找的是「最近录的那个」。
      items.sort((a, b) => (a.dir === b.dir ? b.mtime - a.mtime : (a.dir ? -1 : 1)))
      return { ok: true, path: target, items }
    } catch (e) {
      return { ok: false, error: String(e.message || e), items: [] }
    }
  })
  ipcMain.handle('open-external', (_, url) => {
    if (!isSafeExternalUrl(url)) return '拒绝：仅允许打开 http/https 链接'
    return shell.openExternal(url)
  })
  ipcMain.handle('read-text', (_, p) => {
    if (!isTrustedPath(p)) return ''
    try { return fs.readFileSync(p, 'utf8') } catch { return '' }
  })
  // 封面等项目内本地文件转成安全协议 URL；解析不到可信根内的图片时返回空串，
  // 前端会退化为「无缓存」链接，而不是暴露任意本地文件。
  ipcMain.handle('cover-url', (_, p) => {
    const file = resolveCoverFile(p)
    return file ? localFileUrl(file, LOCAL_FILE_SCHEME) : ''
  })
  // 数据目录查询/切换：让「界面里能换目录」这件事真正成立。
  ipcMain.handle('get-paths', async () => {
    await refreshAllowedRoots().catch(() => {})
    return {
      dataDir: runtimeDir(),
      pointerFile: dataDirPointerFile(),
      allowedRoots,
      resolved: pathsCache,
      packaged: isPackaged(),
    }
  })
  ipcMain.handle('choose-data-dir', () => chooseDataDir())

  // 通用「挑一个文件夹」：给「产物根目录」这类设置项用。
  //
  // 与 choose-data-dir 的区别：那个专门挑**项目目录**，并且强制 looksLikeProject 校验
  // （选错地方界面就读到另一份空库）。这个只是让用户挑一个存放位置，选完原样返回，
  // 目录不存在也允许——用户很可能就是想新建一个。
  ipcMain.handle('choose-dir', async (_e, opts) => {
    const { dialog } = require('electron')
    const o = opts || {}
    const res = await dialog.showOpenDialog({
      title: o.title || '选择文件夹',
      defaultPath: typeof o.defaultPath === 'string' && o.defaultPath ? o.defaultPath : undefined,
      properties: ['openDirectory', 'createDirectory'],
      buttonLabel: '用这个文件夹',
    })
    if (res.canceled || !res.filePaths || !res.filePaths.length) {
      return { ok: false, canceled: true }
    }
    // realish：macOS 上 /tmp 这类符号链接会被 dialog 返回成 /private/tmp，
    // 存原样路径会让「可信根」比对出现同目录两副面孔。
    return { ok: true, path: pathUtils.realish(res.filePaths[0]) }
  })

  // 选择文件（目前用于自定义 SQLite 数据库）。用 save 对话框而不是 open：
  // 用户既要能选已有库，也要能把库指到一个尚未创建的新文件。
  ipcMain.handle('choose-file', async (_e, opts) => {
    const { dialog } = require('electron')
    const o = opts || {}
    const defaultPath = typeof o.defaultPath === 'string' && o.defaultPath ? o.defaultPath : undefined
    const filters = Array.isArray(o.filters) && o.filters.length
      ? o.filters
      : [{ name: '所有文件', extensions: ['*'] }]
    const res = await dialog.showSaveDialog({
      title: o.title || '选择文件',
      defaultPath,
      filters,
      buttonLabel: '使用这个文件',
    })
    if (res.canceled || !res.filePath) return { ok: false, canceled: true }
    return { ok: true, path: pathUtils.realish(res.filePath) }
  })

  // 检查更新（只换程序，不碰数据）。界面按钮和托盘菜单走同一条实现。
  ipcMain.handle('check-update', async (_e, opts) => {
    const r = await checkForUpdateInteractive({ quiet: !!(opts && opts.quiet) })
    return {
      ok: r.ok !== false,
      current: r.current,
      latest: r.latest ? r.latest.version : '',
      releasesDir: r.releasesDir || releasesDir(),
      hasUpdate: !!r.hasUpdate,
      applied: !!r.applied,
      error: r.error || '',
    }
  })
  // 授权类 Cookie 必须落到 daemon 真正读取的运行目录：打包态下脚本的
  // `__dirname` 在 app.asar 内，若由脚本自己推导会写进应用包，既污染签名
  // 又让 daemon 读不到（用户点了授权却依然匿名）。这里由主进程把核心解析出的
  // run 目录显式传下去，并通过 ZUNZHE_RUN_DIR 交给任何子进程。
  function authCookiePath(name) {
    const runDir = runDirResolved()
    process.env.ZUNZHE_RUN_DIR = runDir
    return path.join(runDir, name)
  }
  // B站授权：弹出真实浏览器窗口，用户可选登录；关闭后导出会话 Cookie 供下载会员/高清视频
  ipcMain.handle('bili-login', async () => {
    const { harvestBilibiliCookies } = require('./bili-cookie.cjs')
    return harvestBilibiliCookies(app, authCookiePath('bilibili_browser_cookies.txt'), true)
  })
  // 抖音授权：弹出抖音登录页，用户用手机抖音扫码；登录成功后导出 Cookie 供短视频监控/下载
  ipcMain.handle('douyin-login', async () => {
    const { harvestDouyinCookies } = require('./douyin-cookie.cjs')
    return harvestDouyinCookies(app, authCookiePath('douyin_browser_cookies.txt'), '')
  })
  // 抖音号 → 主播主页 URL 解析：跑 douyin_resolve_douyinid.js（优先注入已保存 Cookie），
  // 前端「添加主播」填抖音号+名字即可，不需要手工复制直播间链接。
  ipcMain.handle('resolve-douyin-anchor', async (_ev, douyinId) => {
    const { execFile } = require('child_process')
    const path = require('path')
    const id = String(douyinId || '').trim()
    if (!id) return { ok: false, error: '抖音号不能为空' }
    const script = path.join(PROJECT_DIR, 'scripts', 'douyin_resolve_douyinid.js')
    const cookiePath = authCookiePath('douyin_browser_cookies.txt')
    const out = await new Promise((resolve) => {
      // 浏览器采样正常只要几秒；挂住必是 Chrome 启动失败/风控/网络卡死。
      // 旧值 150 秒会让「添加」按钮转圈 2 分半、用户以为整个软件卡死——
      // 30 秒失败即报，提示改粘主播主页链接（免浏览器通道）。
      execFile('node', [script, id, '2', cookiePath], { timeout: 30000, maxBuffer: 8 * 1024 * 1024 },
        (err, stdout, stderr) => resolve({ err, stdout: String(stdout || ''), stderr: String(stderr || '') }))
    })
    // 脚本成功输出「=== 推荐使用的 sec_uid ===」+ 下一行 sec_uid
    const m = out.stdout.match(/=== 推荐使用的 sec_uid ===\n\s*([A-Za-z0-9_-]+)/)
    const nick = out.stdout.match(/对应账号: ([^（(]+)/)
    if (m) {
      const url = `https://www.douyin.com/user/${m[1]}`
      return { ok: true, url, sec_uid: m[1], nickname: nick ? nick[1].trim() : id, source: 'resolve' }
    }
    return { ok: false, error: '解析失败：未能确认该抖音号。可能原因：① 抖音号拼写有误；② 尚未完成抖音授权（全局设置→抖音授权→扫码登录）后重试；③ 直接粘贴主播主页链接更稳妥', detail: out.stdout.slice(0, 300) }
  })

  // 窗口必须最后创建。渲染进程一加载就会发第一个 `cli('status')`，而
  // 所有 ipcMain.handle 都在上面：以前 createWindow() 排在前面、handler 排在
  // 后面，冷启动那一发会打到还没注册的通道上，主进程日志报
  // "No handler registered for 'cli'"，界面首屏空白要等下一次轮询才恢复。
  // 顺序反过来以后，渲染进程能发出的第一个请求就一定有人接。
  createWindow()
  await applyStartupPrefs()
  startBackgroundTasks()
})

app.on('will-quit', () => {
  if (watchdogTimer) clearInterval(watchdogTimer)
  if (notifyTimer) clearInterval(notifyTimer)
  if (updateReqTimer) clearInterval(updateReqTimer)
})
app.on('window-all-closed', () => {
  // 主窗口只是隐藏而不是关闭；这里保持托盘进程常驻，不退出应用
})
