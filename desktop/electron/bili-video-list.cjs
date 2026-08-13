// 用 Electron 隐藏窗口加载 B站 UP 主空间页，从页面脚本/接口中提取投稿 BV 号。
// 通过 FUPAN_SCRIPT_FILE 读取要执行的 JavaScript，FUPAN_STATUS_FILE 写 JSON 结果，
// 由 Rust 核心用统一临时文件协议驱动，避免依赖 Electron stdout。
const path = require('path')
const fs = require('fs')

const payloadFile = process.env.FUPAN_SCRIPT_FILE
const statusFile = process.env.FUPAN_STATUS_FILE

if (!payloadFile || !statusFile) {
  console.error('缺少 FUPAN_SCRIPT_FILE 或 FUPAN_STATUS_FILE')
  process.exit(2)
}

const { app, BrowserWindow } = require('electron')

app.disableHardwareAcceleration()
app.setPath('userData', path.join(app.getPath('temp'), 'fupan-bili-list'))

const finish = (obj, code) => {
  try {
    fs.mkdirSync(path.dirname(statusFile), { recursive: true })
    fs.writeFileSync(statusFile, JSON.stringify(obj), 'utf-8')
  } catch (e) {
    // 结果文件写失败时不能继续；让 Rust 看到空结果并记录超时/失败。
  }
  app.exit(code)
}

const watchdog = setTimeout(() => finish({ ok: false, error: '超时（90秒）' }, 1), 90000)

async function run() {
  const script = fs.readFileSync(payloadFile, 'utf8')
  const url = process.env.FUPAN_TARGET_URL
  if (!url) {
    return finish({ ok: false, error: '缺少 FUPAN_TARGET_URL' }, 1)
  }

  const win = new BrowserWindow({
    width: 1100,
    height: 750,
    show: false,
    webPreferences: { partition: 'persist:bilibili', nodeIntegration: false, contextIsolation: true },
  })

  try {
    await Promise.race([
      win.loadURL(url).catch((e) => {
        // 页面加载可能因重定向/长连接不 resolve；继续尝试执行脚本。
      }),
      new Promise((resolve) => setTimeout(resolve, 18000)),
    ])
    await new Promise((resolve) => setTimeout(resolve, 1500))
    const raw = await win.webContents.executeJavaScript(script, true)
    let value
    try {
      value = typeof raw === 'string' ? JSON.parse(raw) : raw
    } catch (e) {
      value = { ok: false, error: '页面脚本返回了非 JSON 字符串' }
    }
    finish(value || { ok: false, error: '页面脚本返回空结果' }, 0)
  } catch (e) {
    finish({ ok: false, error: String(e && e.message ? e.message : e) }, 1)
  } finally {
    if (!win.isDestroyed()) win.destroy()
  }
}

clearTimeout(watchdog)
app.whenReady().then(run).catch((e) => finish({ ok: false, error: 'whenReady: ' + e.message }, 1))
