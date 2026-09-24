// 抖音 Cookie 采集：用 Electron 弹出抖音网页登录页，用户用手机抖音扫码登录，
// 检测到登录成功（出现 sessionid 系列 Cookie）后，把会话 Cookie 导出成
// Netscape 格式给 yt-dlp 和抖音官方接口用，并尽量捕获登录用户的 sec_uid。
// 原理与 B站 bili-cookie.cjs 完全一致：请求来自真实 Chromium 内核，天然绕过风控。
const path = require('path')
const fs = require('fs')

// 落点解析与 bili-cookie.cjs 保持一致：绝不用 __dirname（打包态在 asar 内，
// 会把 cookie 写进应用包，既污染签名又让 daemon 读不到）。
//   1. ZUNZHE_RUN_DIR      —— 真实运行目录（推荐）
//   2. ZUNZHE_PROJECT_ROOT —— 项目根，回落 <root>/fupan_output/run
//   3. __dirname/../..     —— 仅开发态可用
function defaultOut() {
  const name = 'douyin_browser_cookies.txt'
  const runDir = String(process.env.ZUNZHE_RUN_DIR || '').trim()
  if (runDir) return path.join(runDir, name)
  const root = String(process.env.ZUNZHE_PROJECT_ROOT || '').trim()
  if (root) return path.join(root, 'fupan_output', 'run', name)
  return path.join(__dirname, '..', '..', 'fupan_output', 'run', name)
}

// 抖音登录成功的标志 Cookie：网页登录后 Chromium 会写入这些会话 Cookie。
const LOGIN_COOKIE_KEYS = ['sessionid', 'sessionid_ss', 'sid_tt', 'uid_tt']

function logLine(statusFile, msg) {
  try {
    fs.appendFileSync(statusFile + '.log', `[${Date.now()}] ${msg}\n`)
  } catch { /* ignore */ }
}

const LOAD_URL = 'https://www.douyin.com/'
// 抖音首页很重（SSR + 大量 JS），单次加载给足 40 秒；失败自动 reload 重试。
const LOAD_TIMEOUT_MS = 40000
const MAX_LOAD_ATTEMPTS = 3
const RETRY_DELAY_MS = 5000
// 页面加载完成后，SPA 是渐进渲染的，登录按钮可能晚出现：每 3 秒找一次，最多 36 秒。
const CLICK_RETRIES = 12
const CLICK_RETRY_DELAY_MS = 3000

/**
 * 可靠加载抖音首页：事件驱动判定（did-fail-load / loadURL 完成 / 超时），
 * 失败自动 reload 重试（最多 MAX_LOAD_ATTEMPTS 次），并把进度写进窗口标题，
 * 用户能实时看到"加载中/重试中/请扫码"而不是面对空白窗口干等。
 */
async function loadLoginPage(win, debug) {
  const loadOnce = () => new Promise((resolve) => {
    let settled = false
    const finish = (ok, msg) => {
      if (!settled) { settled = true; debug(msg); resolve(ok) }
    }
    // did-fail-load 会带错误码（如 ERR_NAME_NOT_RESOLVED），命中即快速失败，不用干等超时
    const onFail = (event, code, desc, url) => finish(false, `did-fail-load ${code} ${desc} ${url || ''}`)
    win.webContents.once('did-fail-load', onFail)
    win.loadURL(LOAD_URL)
      .then(() => finish(true, 'loadURL done'))
      .catch((e) => finish(false, 'loadURL err ' + e.message))
    setTimeout(() => finish(false, 'loadURL timeout'), LOAD_TIMEOUT_MS)
  })

  for (let attempt = 1; attempt <= MAX_LOAD_ATTEMPTS; attempt++) {
    win.setTitle(attempt === 1
      ? '抖音授权登录 · 正在加载登录页（请稍候）…'
      : `抖音授权登录 · 加载失败，正在重试（${attempt}/${MAX_LOAD_ATTEMPTS}）…`)
    const ok = await loadOnce()
    if (ok) {
      // 给 SPA 一点时间渲染首屏
      await new Promise((r) => setTimeout(r, 3000))
      try {
        const hasContent = await win.webContents.executeJavaScript(
          `document.body && document.body.innerText.length > 0 ? true : false`
        )
        if (hasContent) {
          win.setTitle('抖音授权登录 · 请用手机抖音扫码')
          debug('page ready')
          return true
        }
        debug('page empty body')
      } catch (e) {
        debug('page check err ' + e.message)
      }
    }
    if (attempt < MAX_LOAD_ATTEMPTS) {
      await new Promise((r) => setTimeout(r, RETRY_DELAY_MS))
    }
  }
  win.setTitle('抖音授权登录 · 页面加载困难，请在弹出的窗口里手动刷新后扫码')
  return false
}

/**
 * 自动点击「登录」按钮弹出扫码框。抖音 SPA 渐进渲染，按钮可能延迟出现，
 * 所以按 CLICK_RETRY_DELAY_MS 间隔重试 CLICK_RETRIES 次；找到并点击成功即停。
 */
async function clickLogin(win, debug) {
  for (let i = 0; i < CLICK_RETRIES; i++) {
    try {
      const r = await win.webContents.executeJavaScript(`(function(){
        var btns = Array.from(document.querySelectorAll('button, [class*="login" i], [class*="Login"]'));
        var candidates = ['登录', '登陆', 'Log in', 'Sign in'];
        for (var i = 0; i < btns.length; i++) {
          var el = btns[i];
          var t = (el.textContent || '').trim();
          if (candidates.indexOf(t) >= 0 && el.offsetParent !== null) {
            el.click();
            return 'clicked:' + t;
          }
        }
        return 'no-login-btn';
      })()`, true)
      debug('click login -> ' + r)
      if (String(r).startsWith('clicked:')) return true
    } catch (e) {
      debug('click err ' + e.message)
    }
    await new Promise((r) => setTimeout(r, CLICK_RETRY_DELAY_MS))
  }
  debug('no login button after retries')
  return false
}

/**
 * 可见窗口登录流程：
 *  1. 打开抖音首页（无痕分区），失败自动重试，标题实时提示状态
 *  2. 注入脚本自动点击「登录」按钮（多次重试，适配 SPA 渐进渲染）
 *  3. 轮询 Cookie，出现登录态 Cookie 即判定登录成功
 *  4. 从页面抓取当前登录用户的 sec_uid（尽量）
 *  5. 导出 Netscape Cookie
 */
async function harvestDouyinCookies(app, outFile = defaultOut(), statusFile = '') {
  const { BrowserWindow, session } = require('electron')
  const ses = session.fromPartition('persist:douyin')
  const win = new BrowserWindow({
    width: 1100,
    height: 780,
    show: true,
    title: '抖音授权登录（请用手机抖音扫码）',
    webPreferences: { partition: 'persist:douyin', nodeIntegration: false, contextIsolation: true },
  })

  const debug = (m) => { try { fs.appendFileSync(path.join(path.dirname(outFile), 'douyin_harvest_status.json'), `[${Date.now()}] fn:${m}\n`) } catch { /* ignore */ } }
  debug('window created')

  try {
    // 1. 可靠加载（失败自动重试 3 次）
    await loadLoginPage(win, debug)

    // 2. 自动尝试点击登录按钮，弹出扫码框（抖音可能弹「验证」滑块，用户自行处理）
    await clickLogin(win, debug)

    // 3. 轮询等待登录态 Cookie 出现（最多 300 秒，足够用户扫码）
    let loggedIn = false
    for (let i = 0; i < 300; i++) {
      const cookies = await ses.cookies.get({ domain: '.douyin.com' })
      const names = new Set(cookies.map((c) => c.name))
      if (LOGIN_COOKIE_KEYS.some((k) => names.has(k))) {
        loggedIn = true
        debug('login detected at iteration ' + i)
        break
      }
      await new Promise((r) => setTimeout(r, 1000))
      if (win.isDestroyed()) break
    }

    if (!loggedIn) {
      // 用户可能手动关闭了窗口
      return { ok: false, error: '未检测到登录（超时 300 秒或窗口已关闭），请确认已用手机抖音扫码登录' }
    }

    // 4. 登录成功后，尝试从页面拿到当前用户信息（sec_uid / 抖音号 / 昵称）
    let userInfo = {}
    try {
      const info = await win.webContents.executeJavaScript(`(function(){
        var out = {};
        // 抖音 SPA 里当前用户信息常挂在 window 或 localStorage
        try {
          var ls = localStorage.getItem('douyin_user_info');
          if (ls) { var u = JSON.parse(ls); out.sec_uid = u.sec_uid || ''; out.short_id = u.short_id || ''; out.nickname = u.nickname || ''; }
        } catch(e) {}
        if (!out.sec_uid) {
          // 从页面脚本里找 __INITIAL_STATE__ 或 userInfo
          var w = window;
          if (w.__INITIAL_STATE__ && w.__INITIAL_STATE__.userInfo) {
            var u = w.__INITIAL_STATE__.userInfo;
            out.sec_uid = u.sec_uid || ''; out.nickname = u.nickname || '';
          }
        }
        return JSON.stringify(out);
      })()`, true)
      try { userInfo = JSON.parse(info) } catch { userInfo = {} }
      debug('userInfo: ' + JSON.stringify(userInfo))
    } catch (e) { debug('userInfo err ' + e.message) }

    // 5. 导出 Cookie
    const cookies = await ses.cookies.get({ domain: '.douyin.com' })
    if (!cookies.length) return { ok: false, error: '未取到任何抖音 Cookie' }
    const lines = ['# Netscape HTTP Cookie File']
    for (const c of cookies) {
      const expires = c.expirationDate ? Math.floor(c.expirationDate) : Math.floor(Date.now() / 1000) + 86400 * 180
      lines.push([
        c.domain.startsWith('.') ? c.domain : `.${c.domain}`,
        'TRUE', c.path || '/', c.secure ? 'TRUE' : 'FALSE', expires, c.name, c.value,
      ].join('\t'))
    }
    fs.mkdirSync(path.dirname(outFile), { recursive: true })
    fs.writeFileSync(outFile, lines.join('\n') + '\n', 'utf-8')
    debug('cookies exported: ' + cookies.length)
    return { ok: true, file: outFile, count: cookies.length, user: userInfo }
  } finally {
    if (!win.isDestroyed()) win.destroy()
  }
}

function runStandalone(out, statusFile) {
  const { app } = require('electron')
  app.disableHardwareAcceleration()
  app.setPath('userData', path.join(app.getPath('temp'), 'fupan-douyin-cookie'))
  const finish = (obj, code) => {
    try {
      fs.mkdirSync(path.dirname(statusFile), { recursive: true })
      fs.writeFileSync(statusFile, JSON.stringify(obj), 'utf-8')
    } catch { /* ignore */ }
    app.exit(code)
  }
  // 登录流程给足 6 分钟
  setTimeout(() => finish({ ok: false, error: '超时（360秒）' }, 1), 360000)
  app.whenReady().then(async () => {
    try {
      finish(await harvestDouyinCookies(app, out, statusFile), 0)
    } catch (e) {
      finish({ ok: false, error: String(e.message || e) }, 1)
    }
  }).catch((e) => finish({ ok: false, error: 'whenReady: ' + e.message }, 1))
}

// 独立进程入口：electron douyin-cookie.cjs <输出路径> <状态文件>
if (process.env.ZUNZHE_DOUYIN_COOKIE === '1') {
  const out = process.argv[2] || defaultOut()
  const statusFile = process.argv[3] || path.join(path.dirname(out), 'douyin_cookie_status.json')
  runStandalone(out, statusFile)
}

module.exports = {
  harvestDouyinCookies,
  runStandalone,
  defaultOut,
  get DEFAULT_OUT() { return defaultOut() },
}
