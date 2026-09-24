const { app, BrowserWindow, Tray, Menu, ipcMain, shell, nativeImage, Notification } = require('electron')
const path = require('path')
const { spawn } = require('child_process')
const fs = require('fs')
const { consumeNotifyQueue } = require('./local-notify.cjs')
const { healthcheckInvocation } = require('./healthcheck.cjs')

// electron/ 在 desktop/ 下，Rust 核心在项目根目录（再上一级）。
// 打包态（app.isPackaged=true）下应用包内只读，所有可写数据（config/logs/
// downloads/fupan_output）都落到用户数据目录，Rust 二进制从 resourcesPath 读取。
// 注意：所有依赖 electronApp.getPath() 的值必须在 app ready 之后才可靠，
// 因此这里只用惰性 getter，避免顶层求值导致主进程启动即崩溃。
const PROJECT_DIR = path.resolve(__dirname, '..', '..')
function isPackaged() { return app.isPackaged }
// 数据根永远跟随项目根（外置盘/~/Projects），不要用 userData：
// userData 里曾残留一套旧 config/DB/目录，导致打包态 CLI 与 daemon 数据根分裂、
// 「启动服务」找不到二进制、打开位置指到旧目录、配置改完不生效。
function runtimeDir() { return PROJECT_DIR }
function resDir() { return isPackaged() ? process.resourcesPath : PROJECT_DIR }
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
function notifyQueuePath() { return path.join(runtimeDir(), 'fupan_output', 'run', 'pending_notify.json') }
function notifySeenFilePath() { return path.join(runtimeDir(), 'fupan_output', 'run', 'desktop_seen_notify_ids.json') }
// 打包态下默认配置模板从 resourcesPath/config 复制；开发态直接用项目 config。
function configSrcDir() { return isPackaged() ? path.join(resDir(), 'config') : path.join(PROJECT_DIR, 'config') }

let mainWindow = null
let tray = null
let watchdogTimer = null
let notifyTimer = null
let statusCache = null
let statusCacheAt = 0
const STATUS_CACHE_TTL_MS = 60000
// 这些命令会改变监控状态/文件列表，成功后必须让下次 status 重新全量扫描，
// 否则界面点“添加/下载/复盘”后可能继续显示旧快照。
const STATUS_INVALIDATING_COMMANDS = new Set([
  'add-live', 'remove-live', 'delete-live', 'record-live',
  'add-video', 'remove-video', 'download-video', 'video-download',
  'process', 'check-videos', 'ensure-services', 'start', 'stop',
  'wizard-done', 'set-app-prefs', 'set-config', 'set-config-bulk',
  'monitor-add', 'monitor-remove', 'monitor-set', 'check-now',
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

function runCli(args) {
  return new Promise((resolve, reject) => {
    const exe = fs.existsSync(rustExe()) ? rustExe() : RUST_DEBUG_EXE
    if (!fs.existsSync(exe)) {
      return reject(new Error(`找不到 Rust 核心，请先构建：cd zunzhe-core && cargo build --release\n${rustExe()}`))
    }
    const timeoutMs = 30000
    const proc = spawn(exe, args, {
      cwd: runtimeDir(),
      windowsHide: true,
    })
    const timer = setTimeout(() => {
      proc.kill('SIGTERM')
      // 30 秒后再强制结束，避免进程组残留。
      setTimeout(() => { try { proc.kill('SIGKILL') } catch (e) {} }, 3000)
      reject(new Error(`Rust 核心命令超时（${timeoutMs}ms）: ${args[0]}`))
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
    // 每次健康检查都把结果追加到 logs/zunzhe-core.uptime.log，
    // 与 launchd 看门狗共用同一份 24×30 验收记录。
    const hc = healthcheckInvocation(PROJECT_DIR, process.platform)
    if (hc) {
      const { execFile } = require('child_process')
      execFile(hc.shell, hc.args, { timeout: 10000 }, () => {})
    }
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
        console.log('[watchdog] 已重启服务:', r.restarted.join(', '))
      }
    }
  } catch (e) {
    console.warn('[watchdog]', e.message)
  }
}

function startBackgroundTasks() {
  if (watchdogTimer) clearInterval(watchdogTimer)
  if (notifyTimer) clearInterval(notifyTimer)
  watchdogTimer = setInterval(serviceWatchdog, 60000)
  notifyTimer = setInterval(pollLocalNotifyQueue, 4000)
  serviceWatchdog()
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
  mainWindow.once('ready-to-show', () => {
    mainWindow.show()
    mainWindow.focus()
  })
  mainWindow.on('close', (e) => {
    if (!app.isQuitting) {
      e.preventDefault()
      mainWindow.hide()
    }
  })
  mainWindow.on('closed', () => { mainWindow = null })
}

function createTray() {
  const image = trayIcon()
  tray = new Tray(image.isEmpty() ? nativeImage.createEmpty() : image)
  tray.setToolTip(APP_NAME)
  const menu = Menu.buildFromTemplate([
    { label: '显示主窗口', click: createWindow },
    { label: '重新加载界面', click: () => reloadUiWindow() },
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

app.whenReady().then(async () => {
  if (!gotLock) return
  ensureRuntime()
  createTray()
  createWindow()
  await applyStartupPrefs()
  startBackgroundTasks()
  ipcMain.handle('cli', (_, cmd, ...args) => {
    if (cmd === 'status') return runStatusCached()
    if (STATUS_INVALIDATING_COMMANDS.has(cmd)) invalidateStatusCache()
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
  ipcMain.handle('open-path', (_, p) => shell.openPath(p))
  ipcMain.handle('open-external', (_, url) => shell.openExternal(url))
  // 外置盘访问权限自检：macOS 对 /Volumes/* 有 TCC 授权要求，App 未被授权时
  // 读不到产物根（watchdog 拉不起 daemon、打开位置失败）。返回状态供前端显示引导。
  ipcMain.handle('check-external-volume', () => {
    const candidates = [
      PROJECT_DIR,
      (() => { try { return fs.readFileSync(path.join(runtimeDir(), 'config', 'config.ini'), 'utf8') } catch (e) { return '' } })(),
    ]
    let mediaRoot = ''
    try {
      const ini = candidates[1]
      const m = ini && ini.match(/^\s*产物根目录\s*=\s*(.+)$/m)
      if (m) mediaRoot = m[1].trim()
    } catch (e) {}
    if (!mediaRoot) mediaRoot = '/Volumes/A区/尊者产物'
    try {
      fs.accessSync(mediaRoot, fs.constants.R_OK)
      return { ok: true, mediaRoot }
    } catch (e) {
      try { fs.accessSync(PROJECT_DIR, fs.constants.R_OK); return { ok: false, mediaRoot, hint: `App 读不到产物根（${mediaRoot}）` } } catch (e2) {}
      return { ok: false, mediaRoot, hint: `App 读不到项目根（${PROJECT_DIR}）` }
    }
  })
  ipcMain.handle('read-text', (_, p) => {
    try { return fs.readFileSync(p, 'utf8') } catch { return '' }
  })
  // B站授权：弹出真实浏览器窗口，用户可选登录；关闭后导出会话 Cookie 供下载会员/高清视频
  ipcMain.handle('bili-login', async () => {
    const { harvestBilibiliCookies, DEFAULT_OUT } = require('./bili-cookie.cjs')
    return harvestBilibiliCookies(app, DEFAULT_OUT, true)
  })
})

app.on('will-quit', () => {
  if (watchdogTimer) clearInterval(watchdogTimer)
  if (notifyTimer) clearInterval(notifyTimer)
})
app.on('window-all-closed', () => {
  // 主窗口只是隐藏而不是关闭；这里保持托盘进程常驻，不退出应用
})
