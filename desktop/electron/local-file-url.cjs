const path = require('path')

const DEFAULT_SCHEME = 'local-file'

// 主进程里用的是 LOCAL_FILE_SCHEME 这个名字，测试里用的是 DEFAULT_SCHEME。
// 两个名字指向同一个值，并**同时导出**——2026-09-17 的打包事故就是因为
// main.cjs 用了 LOCAL_FILE_SCHEME 却没地方定义它，模块顶层直接
// ReferenceError 炸掉主进程，窗口永远出不来。这里保证名字有出处。
const LOCAL_FILE_SCHEME = DEFAULT_SCHEME

/**
 * 把绝对路径编码成 `local-file://local/<encodeURIComponent(绝对路径)>`。
 *
 * 为什么用绝对路径而不是「相对项目根」：产物根目录现在是可配置的
 * （见 `zunzhe-core/src/paths.rs` 的 `[路径设置] 产物根目录`），
 * 封面可能落在项目根之外，一个相对路径无法表示它属于哪个根。
 * 可信根校验因此放在真正读文件的地方——主进程的协议处理器里，
 * 那里会拿 allowedRoots 再判一次。非字符串或空值返回空串。
 */
function localFileUrl(filePath, scheme = DEFAULT_SCHEME) {
  if (!filePath || typeof filePath !== 'string') return ''
  return `${scheme}://local/${encodeURIComponent(path.resolve(filePath))}`
}

/**
 * 从 `local-file://` URL 还原绝对路径。
 *
 * scheme 不匹配、host 不对、解码失败、解码结果不是绝对路径，一律返回 null。
 * 刻意不做「看起来像路径就放行」的兜底——调用方拿到 null 就该拒绝这次请求，
 * 而不是拿着半截字符串去拼路径。
 */
function parseLocalFileUrl(url, scheme = DEFAULT_SCHEME) {
  if (typeof url !== 'string' || !url.startsWith(`${scheme}://`)) return null
  let parsed
  try {
    parsed = new URL(url)
  } catch {
    return null
  }
  if (parsed.hostname !== 'local') return null
  let decoded
  try {
    decoded = decodeURIComponent(parsed.pathname.replace(/^\/+/, ''))
  } catch {
    return null
  }
  if (!decoded || !path.isAbsolute(decoded)) return null
  return path.normalize(decoded)
}

module.exports = { DEFAULT_SCHEME, LOCAL_FILE_SCHEME, localFileUrl, parseLocalFileUrl }
