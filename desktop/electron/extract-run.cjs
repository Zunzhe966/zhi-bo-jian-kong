// 独立提取 B站视频直链的 electron 入口（无条件执行）。
// 用法: electron extract-run.cjs <视频页地址> <结果JSON路径>
const path = require('path')
const fs = require('fs')

// 通过环境变量传参：electron 会把 https:// 位置参数当成待打开的 URL 吞掉，故不能用 argv
const videoUrl = process.env.FUPAN_VIDEO_URL || process.argv[2]
const outFile = process.env.FUPAN_OUT_FILE || process.argv[3] || path.join(require('os').tmpdir(), 'bili_streams.json')

const { app } = require('electron')
const { extractBilibiliStreams } = require('./bili-cookie.cjs')
const cookieParser = require('./bilibili-cookie-loader.cjs')
const crypto = require('crypto')

app.disableHardwareAcceleration()
// 独立 userData，避免多个 Electron 实例并发时 Chromium profile 锁冲突。
app.setPath('userData', path.join(app.getPath('temp'), 'fupan-bili-extract-' + crypto.randomBytes(4).toString('hex')))
const finish = (obj, code) => {
  try { fs.mkdirSync(path.dirname(outFile), { recursive: true }); fs.writeFileSync(outFile, JSON.stringify(obj), 'utf-8') } catch { /* ignore */ }
  app.exit(code)
}
setTimeout(() => finish({ ok: false, error: '超时（60秒）' }, 1), 60000)
app.whenReady().then(async () => {
  try {
    const { session } = require('electron')
    await cookieParser.injectCookies(session.fromPartition('persist:bilibili'))
    finish(await extractBilibiliStreams(app, videoUrl), 0)
  }
  catch (e) { finish({ ok: false, error: String(e.message || e) }, 1) }
}).catch((e) => finish({ ok: false, error: 'whenReady: ' + e.message }, 1))
