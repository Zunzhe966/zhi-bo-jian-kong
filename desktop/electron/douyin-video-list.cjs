// 抖音视频列表采集：用 Electron 隐藏窗口加载抖音用户主页，
// 在页面加载前注入脚本 hook XHR/fetch，拦截 /web/aweme/post 的响应 JSON，
// 提取 aweme_list（含 aweme_id、标题、发布时间、封面）。
// 原理与 RSSHub 的 douyin/user 路由一致；注入登录 Cookie 绕过未登录拦截。
// FUPAN_SCRIPT_FILE 存目标 URL，FUPAN_STATUS_FILE 写结果 JSON。
const path = require('path')
const fs = require('fs')

const payloadFile = process.env.FUPAN_SCRIPT_FILE
const statusFile = process.env.FUPAN_STATUS_FILE

if (!payloadFile || !statusFile) {
  console.error('缺少 FUPAN_SCRIPT_FILE 或 FUPAN_STATUS_FILE')
  process.exit(2)
}

const { app, BrowserWindow, session } = require('electron')
const crypto = require('crypto')

app.disableHardwareAcceleration()
app.setPath('userData', path.join(app.getPath('temp'), 'fupan-douyin-list-' + crypto.randomBytes(4).toString('hex')))

const finish = (obj, code) => {
  try {
    fs.mkdirSync(path.dirname(statusFile), { recursive: true })
    fs.writeFileSync(statusFile, JSON.stringify(obj), 'utf-8')
  } catch (e) { /* ignore */ }
  app.exit(code)
}

let watchdog = setTimeout(() => finish({ ok: false, error: '超时（90秒）' }, 1), 90000)

// 注入到页面的 hook 脚本：拦截 XHR 响应体，把 /web/aweme/post 的 JSON 存到 window.__awemePostData
const HOOK_SCRIPT = `(function(){
  if (window.__zunzheHookInstalled) return;
  window.__zunzheHookInstalled = true;
  window.__awemePostData = null;

  var origOpen = XMLHttpRequest.prototype.open;
  var origSend = XMLHttpRequest.prototype.send;
  XMLHttpRequest.prototype.open = function(method, url){
    this.__zurl = url;
    return origOpen.apply(this, arguments);
  };
  XMLHttpRequest.prototype.send = function(){
    var self = this;
    this.addEventListener('load', function(){
      try {
        if (self.__zurl && self.__zurl.indexOf('/web/aweme/post') >= 0) {
          var data = JSON.parse(self.responseText);
          if (data && data.aweme_list) {
            window.__awemePostData = data;
          }
        }
      } catch(e) {}
    });
    return origSend.apply(this, arguments);
  };

  var origFetch = window.fetch;
  window.fetch = function(){
    var url = arguments[0] && arguments[0].url ? arguments[0].url : String(arguments[0]);
    return origFetch.apply(this, arguments).then(function(resp){
      try {
        if (url.indexOf('/web/aweme/post') >= 0) {
          resp.clone().json().then(function(data){
            if (data && data.aweme_list) window.__awemePostData = data;
          }).catch(function(){});
        }
      } catch(e) {}
      return resp;
    });
  };
})()`

async function run() {
  const url = fs.readFileSync(payloadFile, 'utf8').trim()
  if (!url) return finish({ ok: false, error: '缺少目标 URL' }, 1)

  // Cookie 落点必须与 daemon 读取位置一致；打包态 __dirname 在 app.asar 内，
  // 用它推导会把 cookie 当成「应用包里的文件」来读，永远读不到用户授权的登录态。
  // 优先用核心注入的 ZUNZHE_RUN_DIR。
  const runDir = String(process.env.ZUNZHE_RUN_DIR || '').trim()
  const projectRoot = process.env.ZUNZHE_PROJECT_ROOT || path.join(__dirname, '..', '..')
  const cookieFile = path.join(
    runDir || path.join(projectRoot, 'fupan_output', 'run'),
    'douyin_browser_cookies.txt')

  const win = new BrowserWindow({
    width: 1100,
    height: 780,
    show: false,
    webPreferences: { partition: 'persist:douyin', nodeIntegration: false, contextIsolation: true },
  })

  try {
    const ses = session.fromPartition('persist:douyin')
    if (fs.existsSync(cookieFile)) {
      const text = fs.readFileSync(cookieFile, 'utf8')
      const cookies = []
      for (const line of text.split('\n')) {
        if (!line || line.startsWith('#')) continue
        const parts = line.split('\t')
        if (parts.length < 7) continue
        cookies.push({
          url: 'https://www.douyin.com/',
          name: parts[5],
          value: parts[6],
          domain: parts[0],
          path: parts[2] || '/',
          secure: parts[3] === 'TRUE',
          expirationDate: parseInt(parts[4], 10) || (Date.now() / 1000 + 86400 * 180),
        })
      }
      if (cookies.length) await ses.cookies.set(cookies)
    }

    await Promise.race([
      win.loadURL(url).catch(() => {}),
      new Promise((r) => setTimeout(r, 20000)),
    ])

    // 注入 hook 并重新触发页面数据加载
    await win.webContents.executeJavaScript(HOOK_SCRIPT, true).catch(() => {})
    // 滚动触发懒加载，确保 aweme/post 接口被调用
    for (let i = 0; i < 6; i++) {
      await win.webContents.executeJavaScript('window.scrollTo(0, document.body.scrollHeight)', true).catch(() => {})
      await new Promise((r) => setTimeout(r, 1500))
    }

    // 轮询读取拦截到的数据
    let data = null
    for (let i = 0; i < 20; i++) {
      const r = await win.webContents.executeJavaScript(
        'window.__awemePostData ? JSON.stringify(window.__awemePostData) : null', true)
      if (r) {
        try { data = JSON.parse(r); if (data.aweme_list) break } catch (e) { /* ignore */ }
      }
      await new Promise((res) => setTimeout(res, 1500))
    }

    if (!data || !data.aweme_list) {
      return finish({ ok: false, error: '未捕获到抖音视频列表（可能需要登录，或触发风控验证）' }, 1)
    }

    const items = data.aweme_list.map((post) => ({
      id: String(post.aweme_id || ''),
      title: (post.desc || '').split('\n')[0].trim(),
      url: 'https://www.douyin.com/video/' + post.aweme_id,
      cover: (post.video && post.video.cover && post.video.cover.url_list && post.video.cover.url_list[0]) || '',
      created_at: post.create_time ? post.create_time * 1000 : 0, // 毫秒时间戳
      duration: post.video && post.video.duration ? Math.round(post.video.duration / 1000) : 0,
    })).filter((it) => it.id)

    const author = data.aweme_list[0] && data.aweme_list[0].author ? data.aweme_list[0].author : null
    return finish({
      ok: true,
      count: items.length,
      author: author ? { sec_uid: author.sec_uid || '', nickname: author.nickname || '' } : null,
      items,
    }, 0)
  } catch (e) {
    return finish({ ok: false, error: String(e && e.message ? e.message : e) }, 1)
  } finally {
    if (!win.isDestroyed()) win.destroy()
  }
}

app.whenReady().then(run).catch((e) => finish({ ok: false, error: 'whenReady: ' + e.message }, 1))
