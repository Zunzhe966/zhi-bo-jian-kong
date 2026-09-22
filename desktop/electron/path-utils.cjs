// 路径解析工具：Electron 主进程里最容易出错、也最值得单测的那部分。
//
// 抽出来的原因很直接：这里以前出过一个「点了没反应」的静默 bug——
// 「打开目录」按钮的白名单写死成 [应用包目录, userData]，而界面要打开的路径是
// `~/Projects/尊者监控直播短视频`，不在白名单里；handler 返回一个字符串又没人看，
// 于是连报错都没有。这类逻辑必须能被直接测，不能只靠肉眼看代码。
//
// 依赖全部以参数传入，模块本身不 require('electron')，因此可以在纯 Node 下跑测试。

const fs = require('fs')
const path = require('path')

/** 尽量解析成真实路径；路径不存在时退到「最近的存在祖先 + 剩余段」。 */
function realish(p) {
  const abs = path.resolve(p)
  try { return fs.realpathSync.native(abs) } catch (e) { /* 继续往上找 */ }
  const tail = []
  let cur = abs
  while (true) {
    const parent = path.dirname(cur)
    if (parent === cur) return abs
    tail.unshift(path.basename(cur))
    try { return path.join(fs.realpathSync.native(parent), ...tail) } catch (e) { /* 再往上 */ }
    cur = parent
  }
}

/**
 * 路径是否落在某个根之内。
 *
 * 原样和真实路径各比一遍：macOS 上 `/tmp` 是指向 `/private/tmp` 的符号链接，
 * 只比字符串会把同一个目录判成两个，进而把正常路径也拒掉。
 */
function withinRoot(p, root) {
  for (const a of [realish(p), path.resolve(p)]) {
    for (const b of [realish(root), path.resolve(root)]) {
      if (a === b || a.startsWith(b + path.sep)) return true
    }
  }
  return false
}

/** 路径是否落在任意一个可信根内。 */
function isTrustedPath(p, roots) {
  if (typeof p !== 'string' || !p.trim()) return false
  if (!Array.isArray(roots) || !roots.length) return false
  return roots.some((root) => withinRoot(p, root))
}

/** 一个目录算不算「已初始化的项目数据目录」：有 config/config.ini 或已有主库。 */
function looksLikeProject(dir) {
  if (!dir || typeof dir !== 'string') return false
  try {
    return fs.existsSync(path.join(dir, 'config', 'config.ini'))
      || fs.existsSync(path.join(dir, 'zunzhe-core.db'))
  } catch (e) {
    return false
  }
}

/** 读指针文件的第一行非空非注释内容；读不到返回空串。 */
function readPointerFile(pointerFile) {
  try {
    const raw = fs.readFileSync(pointerFile, 'utf8')
    return raw.split(/\r?\n/).map((s) => s.trim()).find((s) => s && !s.startsWith('#')) || ''
  } catch (e) {
    return ''
  }
}

/**
 * 在 /Volumes 下找项目副本（数据放在外置盘上的情况）。
 *
 * 必须**排序**：`readdirSync` 返回的是目录项原始顺序，同一台机器上不保证稳定。
 * 不排序时「挂了两块盘、各有一份项目」的情况下，谁被选中是随机的——
 * 表现就是「今天打开是这个库，明天打开是那个库」。
 */
function volumesCandidates(projectName, volumesRoot = '/Volumes') {
  const out = []
  try {
    const names = fs.readdirSync(volumesRoot).filter((n) => !n.startsWith('.'))
    names.sort((a, b) => a.localeCompare(b, 'zh-Hans-CN'))
    for (const name of names) {
      out.push(path.join(volumesRoot, name, projectName))
    }
  } catch (e) { /* 非 macOS 或目录不可读，忽略 */ }
  return out
}

/**
 * 候选数据目录，按优先级排列（先命中先用）。
 *
 * 顺序就是「越靠前越算用户显式指定」：环境变量 > 指针文件 > 常规位置 > 挂载卷。
 * 不再有任何一个候选是唯一硬编码答案，所以打包好的 .app 能跟着数据走。
 */
function candidateDataDirs({ envDir, pointerFile, home, projectName, volumesRoot }) {
  return [
    envDir,
    pointerFile ? readPointerFile(pointerFile) : '',
    home ? path.join(home, 'Projects', projectName) : '',
    home ? path.join(home, 'Desktop', projectName) : '',
    ...volumesCandidates(projectName, volumesRoot),
  ].filter((d) => typeof d === 'string' && d.trim())
}

/**
 * 按候选顺序挑第一个像样的数据目录；**都不像则返回 null**。
 *
 * 与 pickDataDir 的区别很重要：pickDataDir 会用 fallback 悄悄兜底，
 * 调用方无法区分「真的找到了」和「一个都没找到、只好用兜底」。
 * 而 fallback 往往是 userData —— 那里通常是一套**空的**库，
 * 悄悄用它会直接表现为「界面全空、数据像丢了」。
 * 所以需要「找不到就明确报错」的地方一律用这个函数。
 */
function firstProjectDir(candidates) {
  for (const dir of candidates) {
    if (looksLikeProject(dir)) return dir
  }
  return null
}

/** 按候选顺序挑第一个像样的数据目录；都不像则返回 fallback。 */
function pickDataDir(candidates, fallback) {
  return firstProjectDir(candidates) || fallback
}

/**
 * 把界面给的路径转成真正能打开的目录。
 *
 * 目录可能还没建（产物根刚改完、还没跑过一轮），这时退到「仍在白名单内、
 * 且真实存在的最近祖先」——总比点了没反应强。
 */
function resolveOpenablePath(p, roots) {
  if (typeof p !== 'string' || !p.trim()) return null
  const abs = path.resolve(p)
  if (!isTrustedPath(abs, roots)) return null
  if (fs.existsSync(abs)) return abs
  let cur = abs
  while (true) {
    const parent = path.dirname(cur)
    if (parent === cur) return null
    if (fs.existsSync(parent) && isTrustedPath(parent, roots)) return parent
    cur = parent
  }
}

module.exports = {
  realish,
  withinRoot,
  isTrustedPath,
  looksLikeProject,
  readPointerFile,
  volumesCandidates,
  candidateDataDirs,
  firstProjectDir,
  pickDataDir,
  resolveOpenablePath,
}
