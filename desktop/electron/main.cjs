const { app, BrowserWindow, Tray, Menu, globalShortcut, ipcMain, shell, nativeImage, Notification } = require('electron')
const path = require('path')
const { spawn } = require('child_process')
const fs = require('fs')

// electron/ 在 desktop/ 下，Rust 核心在项目根目录（再上一级）
const PROJECT_DIR = path.resolve(__dirname, '..', '..')
const RUST_EXE = path.join(PROJECT_DIR, 'zunzhe-core', 'target', 'release', 'zunzhe-core')
const RUST_DEBUG_EXE = path.join(PROJECT_DIR, 'zunzhe-core', 'target', 'debug', 'zunzhe-core')
const DIST_HTML = path.join(__dirname, '..', 'dist', 'index.html')
const ASSETS = path.join(__dirname, '..', 'assets')
const ICON_ICO = path.join(ASSETS, 'icon.ico')
const ICON_PNG = path.join(ASSETS, 'icon.png')
const ICON_TRAY = path.join(ASSETS, 'icon-32.png')
const APP_NAME = '尊者直播监控'
const HOTKEY = 'Control+Shift+L'
const NOTIFY_QUEUE = path.join(PROJECT_DIR, 'fupan_output', 'run', 'pending_notify.json')

let mainWindow = null
let tray = null
let watchdogTimer = null
let notifyTimer = null
const seenNotifyIds = new Set()
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
    const exe = fs.existsSync(RUST_EXE) ? RUST_EXE : RUST_DEBUG_EXE
    if (!fs.existsSync(exe)) {
      return reject(new Error(`找不到 Rust 核心，请先构建：cd zunzhe-core && cargo build --release\n${RUST_EXE}`))
    }
    const proc = spawn(exe, args, {
      cwd: PROJECT_DIR,
      windowsHide: true,
    })
    let stdout = ''
    let stderr = ''
    proc.on('error', (err) => reject(new Error(`无法启动后端: ${err.message}`)))
    proc.stdout.on('data', (d) => { stdout += d.toString('utf8') })
    proc.stderr.on('data', (d) => { stderr += d.toString('utf8') })
    proc.on('close', (code) => {
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
    if (!fs.existsSync(NOTIFY_QUEUE)) return
    const raw = fs.readFileSync(NOTIFY_QUEUE, 'utf8')
    const items = JSON.parse(raw)
    if (!Array.isArray(items) || !items.length) return
    const pending = []
    for (const item of items) {
      const id = String(item.id || '')
      if (!id || seenNotifyIds.has(id)) continue
      seenNotifyIds.add(id)
      showLocalNotification(item.title, item.body)
      pending.push(item)
    }
    if (seenNotifyIds.size > 200) {
      seenNotifyIds.clear()
      for (const item of items.slice(-50)) {
        if (item.id) seenNotifyIds.add(String(item.id))
      }
    }
  } catch (e) {
    console.warn('[local-notify]', e.message)
  }
}

async function serviceWatchdog() {
  try {
    pollLocalNotifyQueue()
    const prefRes = await runCli(['get-app-prefs'])
    const prefs = prefRes.prefs || {}
    if (prefs.watchdog === false) return
    const st = await runCli(['status'])
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
    { label: `快捷键 ${HOTKEY}`, enabled: false },
    { type: 'separator' },
    { label: '退出', click: () => { app.isQuitting = true; app.quit() } },
  ])
  tray.setContextMenu(menu)
  tray.on('double-click', createWindow)
}

function registerShortcut() {
  globalShortcut.unregisterAll()
  const ok = globalShortcut.register(HOTKEY, createWindow)
  if (!ok) console.warn('快捷键注册失败:', HOTKEY)
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

app.whenReady().then(async () => {
  if (!gotLock) return
  createTray()
  registerShortcut()
  createWindow()
  await applyStartupPrefs()
  startBackgroundTasks()
  ipcMain.handle('cli', (_, cmd, ...args) => runCli([cmd, ...args]))
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
  globalShortcut.unregisterAll()
  if (watchdogTimer) clearInterval(watchdogTimer)
  if (notifyTimer) clearInterval(notifyTimer)
})
app.on('window-all-closed', () => {
  // 主窗口只是隐藏而不是关闭；这里保持托盘进程常驻，不退出应用
})
