// 把 Rust 侧注入的 B站 Cookie 文件（Netscape 格式）写进 Electron 会话。
// 这样独立启动的 Electron 进程也能复用桌面端「B站授权」导出的登录态，
// 而不是每次用全新的临时 userData 匿名访问 B站。
const fs = require('fs')
const path = require('path')

function parseNetscape(file) {
  const text = fs.readFileSync(file, 'utf8')
  const cookies = []
  for (const line of text.split(/\r?\n/)) {
    const trimmed = line.trim()
    if (!trimmed || trimmed.startsWith('#')) continue
    const fields = trimmed.split('\t')
    if (fields.length < 7) continue
    const [domain, , cookiePath, secure, expires, name, value] = fields
    if (!domain || !name || value === undefined) continue
    cookies.push({
      url: (domain.startsWith('.') ? 'https://www' : 'https://') + domain + (cookiePath || '/'),
      name,
      value,
      domain: domain.startsWith('.') ? domain.slice(1) : domain,
      path: cookiePath || '/',
      secure: String(secure).toUpperCase() === 'TRUE',
      expirationDate: Number(expires) || Math.floor(Date.now() / 1000) + 86400 * 180,
    })
  }
  return cookies
}

async function injectCookies(ses) {
  const files = []
  if (process.env.FUPAN_COOKIE_FILE && fs.existsSync(process.env.FUPAN_COOKIE_FILE)) {
    files.push(process.env.FUPAN_COOKIE_FILE)
  }
  if (process.env.FUPAN_USER_COOKIE_FILE && fs.existsSync(process.env.FUPAN_USER_COOKIE_FILE)) {
    files.push(process.env.FUPAN_USER_COOKIE_FILE)
  }
  for (const file of files) {
    for (const cookie of parseNetscape(file)) {
      try {
        await ses.cookies.set(cookie)
      } catch (e) {
        // 单条 Cookie 失败不阻塞整体；保留后续 Cookie 继续注入。
        console.warn('[bilibili-cookie] set failed:', cookie.name, e.message)
      }
    }
  }
}

module.exports = { parseNetscape, injectCookies }
