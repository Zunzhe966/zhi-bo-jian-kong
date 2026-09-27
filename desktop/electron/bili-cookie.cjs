// B站 Cookie 采集：用 Electron 自带的 Chromium 在后台访问 B站，
// 通过其 JS 风控验证后，把会话 Cookie 导出成 Netscape 格式给 yt-dlp 用。
// 原理与"浏览器下载插件"相同：请求来自真实浏览器内核，B站不会 412。
// 可独立运行：electron bili-cookie.cjs [输出路径]
const path = require('path')
const fs = require('fs')

// Cookie 文件落点绝不能从 __dirname 推导：打包态下 __dirname 在 app.asar 内，
// 写出来会落到 `Contents/Resources/fupan_output/run/`，既污染应用包（导致
// codesign 报 "a sealed resource is missing or invalid"），又让 daemon 读不到
// —— 用户点了「B站授权」但录制链路依然匿名，等于白做。
//
// 解析优先级：
//   1. ZUNZHE_RUN_DIR      —— 核心/主进程解析出的真实运行目录（推荐）
//   2. ZUNZHE_PROJECT_ROOT —— 项目根，回落 <root>/fupan_output/run
//   3. __dirname/../..     —— 仅开发态源码目录可用
function defaultOut() {
  const name = 'bilibili_browser_cookies.txt'
  const runDir = String(process.env.ZUNZHE_RUN_DIR || '').trim()
  if (runDir) return path.join(runDir, name)
  const root = String(process.env.ZUNZHE_PROJECT_ROOT || '').trim()
  if (root) return path.join(root, 'fupan_output', 'run', name)
  return path.join(__dirname, '..', '..', 'fupan_output', 'run', name)
}

/**
 * 在隐藏窗口里加载 B站首页，等风控 JS 跑完生成验证 Cookie，再导出。
 * @param {Electron.App} app
 * @param {string} outFile Netscape cookie 文件输出路径
 * @param {boolean} visible 是否显示窗口（用户想登录账号时传 true）
 */
async function harvestBilibiliCookies(app, outFile = defaultOut(), visible = false) {
  const { BrowserWindow, session } = require('electron')
  const ses = session.fromPartition('persist:bilibili')
  const win = new BrowserWindow({
    width: 1000,
    height: 700,
    show: visible,
    title: 'B站授权（可登录后关闭窗口）',
    webPreferences: { partition: 'persist:bilibili', nodeIntegration: false, contextIsolation: true },
  })
  const dbg = (m) => { try { fs.appendFileSync(path.join(path.dirname(outFile), 'harvest_status.json'), `[${Date.now()}] fn:${m}\n`) } catch { /* ignore */ } }
  dbg('window created')
  try {
    // loadURL 在部分站点重定向/长连接时可能迟迟不 resolve，用超时兜底
    await Promise.race([
      win.loadURL('https://www.bilibili.com/').then(() => dbg('loadURL done')).catch((e) => dbg('loadURL err ' + e.message)),
      new Promise((r) => setTimeout(() => { dbg('loadURL timeout'); r() }, 20000)),
    ])
    // 风控 JS（ExClimbWuzhi）需要时间上报环境并激活 buvid
    await new Promise((r) => setTimeout(r, visible ? 500 : 6000))
    dbg('reading cookies')
    if (visible) {
      // 可见模式：等用户自己操作（登录/浏览），窗口关闭时再导出
      await new Promise((resolve) => win.once('closed', resolve))
    }
    const cookies = await ses.cookies.get({ domain: '.bilibili.com' })
    if (!cookies.length) throw new Error('未取到任何 B站 Cookie')
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
    return { ok: true, file: outFile, count: cookies.length }
  } finally {
    if (!win.isDestroyed()) win.destroy()
  }
}

/**
 * 只做「搬运」：把 persist:bilibili 分区里**已经存在**的 Cookie 直接导出成
 * Netscape 文件——不打开网页、不发任何网络请求。
 *
 * 为什么必须有这一步（2026-09-27 实测事故）：
 *   用户 09-24 在授权窗口里登录成功，登录态存进了 Electron 分区
 *   （Partitions/bilibili/Cookies 里 SESSDATA 有效期到 2027-03-23），
 *   但「列出历史」走的是后台程序直接请求 api.bilibili.com，只认这个
 *   Netscape 文件——它当时压根没被写出来（唯一一份还是 9-18 留下的
 *   28 字节空壳，而且落在旧产物根 ~/Documents/尊者产物 下）。
 *   结果就是用户反复质问「我明明登录过」，而程序一直以匿名身份取数，
 *   官方 46 条只能列出 45 条。
 *
 * 每次启动都静默重导一次，让「浏览器分区」成为唯一事实来源：产物根改了、
 * 文件被别的工具覆写了、用户只是重新登录了，都能自动自愈，不需要再点授权。
 * @param {Electron.App} app
 * @param {string} outFile Netscape cookie 输出路径
 */
async function exportSessionCookies(app, outFile = defaultOut()) {
  const { session } = require('electron')
  const ses = session.fromPartition('persist:bilibili')
  const all = await ses.cookies.get({})
  // 只留 B站 域；并按 cookie 名去重——`.bilibili.cn` 与 `.bilibili.com`
  // 会各存一份同名的 SESSDATA，重复写进去会让后台程序拼出
  // "SESSDATA=a; ...; SESSDATA=a" 这种头（多数服务端能容忍，但没必要）。
  const seen = new Set()
  const cookies = []
  for (const c of all) {
    const host = String(c.domain || '')
    if (!host.endsWith('bilibili.com')) continue
    if (!c.name || seen.has(c.name)) continue
    seen.add(c.name)
    cookies.push(c)
  }
  if (!cookies.length) return { ok: false, count: 0, reason: 'session-empty' }
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
  return { ok: true, count: cookies.length, file: outFile }
}

/**
 * 独立进程入口：由 harvest-run.cjs 调用（electron 主脚本里 require.main 不可靠，
 * 所以单独用一个无条件执行的启动脚本，而不是在这里用 require.main 判断）。
 * @param {string} out cookie 输出路径
 * @param {string} statusFile 状态/结果 JSON 路径（供父进程轮询）
 */
function runStandalone(out, statusFile) {
  const { app } = require('electron')
  app.disableHardwareAcceleration()
  app.setPath('userData', path.join(app.getPath('temp'), 'fupan-bili-cookie'))
  const finish = (obj, code) => {
    try {
      fs.mkdirSync(path.dirname(statusFile), { recursive: true })
      fs.writeFileSync(statusFile, JSON.stringify(obj), 'utf-8')
    } catch { /* ignore */ }
    app.exit(code)
  }
  setTimeout(() => finish({ ok: false, error: '超时（60秒）' }, 1), 60000)
  app.whenReady().then(async () => {
    try {
      finish(await harvestBilibiliCookies(app, out, false), 0)
    } catch (e) {
      finish({ ok: false, error: String(e.message || e) }, 1)
    }
  }).catch((e) => finish({ ok: false, error: 'whenReady: ' + e.message }, 1))
}

/**
 * 用 Electron 内核加载 B站视频页，直接读取网页内嵌的 window.__playinfo__，
 * 拿到 DASH 视频/音频直链——和"浏览器下载插件"完全一样的原理，天然绕过
 * yt-dlp 打 playurl API 时遇到的 412 风控。
 * @param {Electron.App} app
 * @param {string} videoUrl B站视频页地址
 * @returns {Promise<{ok:boolean,title?:string,video?:string,audio?:string,error?:string}>}
 */
async function extractBilibiliStreams(app, videoUrl) {
  const { BrowserWindow } = require('electron')
  const win = new BrowserWindow({
    width: 1200, height: 800, show: false,
    webPreferences: { partition: 'persist:bilibili', nodeIntegration: false, contextIsolation: true },
  })
  try {
    await Promise.race([
      win.loadURL(videoUrl).catch(() => {}),
      new Promise((r) => setTimeout(r, 25000)),
    ])
    // 轮询等待 __playinfo__ 注入（播放器脚本异步写入）
    const script = `(function(){
      try{
        var p = window.__playinfo__; if(!p) return null;
        var d = p.data || p.result || {};
        var title = (window.__INITIAL_STATE__ && window.__INITIAL_STATE__.videoData && window.__INITIAL_STATE__.videoData.title) || document.title;
        if(d.dash){
          var vs=(d.dash.video||[]).slice().sort(function(a,b){return (b.bandwidth||0)-(a.bandwidth||0)});
          var as=(d.dash.audio||[]).slice().sort(function(a,b){return (b.bandwidth||0)-(a.bandwidth||0)});
          return JSON.stringify({title:title, video:(vs[0]||{}).baseUrl||(vs[0]||{}).base_url, audio:(as[0]||{}).baseUrl||(as[0]||{}).base_url});
        }
        if(d.durl && d.durl.length){ return JSON.stringify({title:title, video:d.durl[0].url, audio:null}); }
        return null;
      }catch(e){ return 'ERR:'+e.message; }
    })()`
    let info = null
    for (let i = 0; i < 20; i++) {
      const r = await win.webContents.executeJavaScript(script, true)
      if (r && !String(r).startsWith('ERR:')) { info = JSON.parse(r); break }
      await new Promise((res) => setTimeout(res, 1000))
    }
    if (!info || !info.video) return { ok: false, error: '页面未找到可下载的视频流（可能需要登录/大会员）' }
    return { ok: true, title: info.title, video: info.video, audio: info.audio || '' }
  } finally {
    if (!win.isDestroyed()) win.destroy()
  }
}

module.exports = {
  harvestBilibiliCookies,
  exportSessionCookies,
  extractBilibiliStreams,
  runStandalone,
  defaultOut,
  // 兼容旧调用方：取用时才求值，保证 env 已就绪。
  get DEFAULT_OUT() { return defaultOut() },
}
