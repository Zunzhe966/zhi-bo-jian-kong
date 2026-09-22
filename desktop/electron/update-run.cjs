// 检查更新 / 应用更新。
//
// 这个项目最痛的一条教训就是「更新一次，数据像丢了一次」。根因是程序和数据
// 曾经搅在一起，更新时顺手动了数据。本模块的设计前提只有一条：
//
//   **更新只替换程序，永远不碰数据目录。**
//
// 程序在 /Applications/尊者直播监控.app，数据在「数据目录」（由 path-utils 解析，
// 见 main.cjs 的 runtimeDir）。两者物理分离，所以「点更新」不可能导致数据消失。
//
// 更新源 = <数据目录>/releases/。离线可用，不需要服务器；把新版本丢进这个文件夹，
// 界面上点「检查更新」即可。
//
// 认可两种投放形式：
//   · 尊者直播监控.app              整个应用包
//   · 尊者直播监控-<版本>.zip        ditto 打的压缩包（解开后自动找 .app）
//
// 版本号是唯一权威，取包内 Contents/Info.plist 的 CFBundleShortVersionString。
//
// 依赖只有 node 内建模块 + macOS 自带命令行（ditto / codesign / PlistBuddy / unzip），
// 因此可以在纯 Node 下跑单测，见 update-run.test.cjs。

const fs = require('fs')
const path = require('path')
const { spawnSync } = require('child_process')

const RELEASES_DIR_NAME = 'releases'
const APP_BUNDLE_NAME = '尊者直播监控.app'
// 暂存目录用点号开头：Finder 默认不显示，且严格限制在安装目录内，
// 保证 rename 是**同卷操作**（跨卷 rename 会退化成拷贝，不再原子）。
const STAGE_PREFIX = '.zunzhe-update-'
const OLD_PREFIX = '.zunzhe-old-'

/** '1.2.3' / 'v1.2.3' / '1.2.3-beta.1' → [1,2,3,0,...]（非数字段忽略） */
function parseVersion(v) {
  if (typeof v !== 'string') return [0]
  const nums = v.trim().replace(/^v/i, '')
    .split(/[.\-+_]/)
    .map((s) => parseInt(s, 10))
    .filter((n) => Number.isFinite(n))
  return nums.length ? nums : [0]
}

/** a > b → 1；a < b → -1；相等 → 0。缺位按 0 补齐。 */
function compareVersion(a, b) {
  const x = parseVersion(a)
  const y = parseVersion(b)
  const n = Math.max(x.length, y.length)
  for (let i = 0; i < n; i++) {
    const xi = x[i] || 0
    const yi = y[i] || 0
    if (xi !== yi) return xi > yi ? 1 : -1
  }
  return 0
}

function plistVersion(plistPath) {
  if (!fs.existsSync(plistPath)) return ''
  const r = spawnSync('/usr/libexec/PlistBuddy',
    ['-c', 'Print :CFBundleShortVersionString', plistPath], { encoding: 'utf8' })
  if (r.status !== 0) return ''
  return String(r.stdout || '').trim()
}

/** 从 .app 包读版本号；读不到返回空串。 */
function readBundleVersion(appPath) {
  return plistVersion(path.join(appPath, 'Contents', 'Info.plist'))
}

/** 在解压出来的目录里找第一个 .app（只认带 Info.plist 的）。 */
function findAppIn(dir, depth = 0) {
  if (depth > 3) return null
  let names = []
  try { names = fs.readdirSync(dir) } catch (e) { return null }
  names.sort()
  for (const n of names) {
    if (n.startsWith('.')) continue
    const full = path.join(dir, n)
    if (n.endsWith('.app')) {
      try { if (fs.statSync(full).isDirectory() && fs.existsSync(path.join(full, 'Contents', 'Info.plist'))) return full } catch (e) { /* 跳过 */ }
    }
  }
  for (const n of names) {
    if (n.startsWith('.')) continue
    const full = path.join(dir, n)
    try { if (fs.statSync(full).isDirectory()) { const hit = findAppIn(full, depth + 1); if (hit) return hit } } catch (e) { /* 跳过 */ }
  }
  return null
}

/**
 * 读 zip 里那个 .app 的版本号。
 *
 * 只解出 Info.plist 一个条目（unzip -o <entry>），不解整个包——
 * 检查更新是很轻的操作，不能为了知道版本号把 300MB 全解开。
 */
function zipBundleVersion(zipPath, tmpDir) {
  const l = spawnSync('/usr/bin/unzip', ['-Z1', zipPath],
    { encoding: 'utf8', maxBuffer: 16 * 1024 * 1024 })
  if (l.status !== 0) return ''
  const entry = String(l.stdout || '').split('\n')
    .map((s) => s.trim().replace(/\/+$/, ''))
    .find((s) => s.endsWith('.app/Contents/Info.plist'))
  if (!entry) return ''
  const e = spawnSync('/usr/bin/unzip', ['-o', zipPath, entry, '-d', tmpDir],
    { encoding: 'utf8', maxBuffer: 16 * 1024 * 1024 })
  if (e.status !== 0) return ''
  return plistVersion(path.join(tmpDir, entry))
}

/**
 * 列出 releases 目录里所有可识别的版本。
 *
 * 三种投放形式都认，因为用户/脚本自然会这么丢：
 *   releases/尊者直播监控.app                整个应用包
 *   releases/尊者直播监控-1.4.0.zip           ditto 打的压缩包
 *   releases/1.4.0/尊者直播监控.app            带版本号的文件夹（自动下钻一层找 .app）
 * 其余杂物（说明文件、.DS_Store）忽略。
 */
function listReleases(releasesDir, tmpDir) {
  const out = []
  let names = []
  try { names = fs.readdirSync(releasesDir) } catch (e) { return out }
  names.sort((a, b) => a.localeCompare(b, 'zh-Hans-CN'))
  for (const name of names) {
    if (name.startsWith('.')) continue
    const full = path.join(releasesDir, name)
    let isDir = false
    try { isDir = fs.statSync(full).isDirectory() } catch (e) { continue }
    if (isDir && name.endsWith('.app')) {
      const version = readBundleVersion(full)
      if (version) out.push({ path: full, kind: 'app', version, name })
    } else if (!isDir && name.toLowerCase().endsWith('.zip')) {
      let version = ''
      try { version = zipBundleVersion(full, tmpDir) } catch (e) { version = '' }
      // 兜底：zip 解不出 Info.plist 就退回文件名里的版本号（尊者直播监控-1.2.0.zip）
      if (!version) {
        const m = name.match(/-(\d+\.\d+(?:\.\d+)*)\.zip$/i)
        if (m) version = m[1]
      }
      out.push({ path: full, kind: 'zip', version, name })
    } else if (isDir) {
      // 版本文件夹：下钻找 .app
      const inner = findAppIn(full)
      if (inner) {
        const version = readBundleVersion(inner)
        if (version) out.push({ path: inner, kind: 'app', version, name: `${name}/${path.basename(inner)}` })
      }
    }
  }
  return out
}

/** 检查更新：返回 { ok, current, latest, hasUpdate, error }。 */
function checkForUpdate({ releasesDir, currentVersion, tmpDir }) {
  const tmp = tmpDir || fs.mkdtempSync(path.join(require('os').tmpdir(), 'zunzhe-check-'))
  const all = listReleases(releasesDir, tmp)
  const usable = all.filter((r) => r.version)
  if (!usable.length) {
    const why = all.length
      ? `releases 里有 ${all.length} 个条目，但都读不出版本号（zip 里要能找到 .app）`
      : 'releases 文件夹是空的'
    return { ok: true, current: currentVersion, latest: null, hasUpdate: false, releasesDir, note: why }
  }
  usable.sort((a, b) => compareVersion(b.version, a.version))
  const latest = usable[0]
  return {
    ok: true,
    current: currentVersion,
    latest,
    hasUpdate: compareVersion(latest.version, currentVersion) > 0,
    releasesDir,
    note: '',
  }
}

/**
 * 删除一个目录树（里面可能含 .app / app.asar）。
 *
 * **不要用 fs.rmSync 删应用包。** Electron 会改写主进程的 fs，把任何含
 * `.asar` 的路径当成压缩包的虚拟目录，于是 rmSync 递归到
 * `Contents/Resources/app.asar` 时认定它是目录、去调 rmdir，内核直接回
 * ENOTDIR。实测后果：旧包永远删不掉，每更新一次就在 /Applications 里
 * 留一份 288MB 的残骸吃内盘空间。
 *
 * 交给系统 /bin/rm 走原始文件系统最稳；/bin/rm 不可用时再退回 Node 实现
 * （对不含 asar 的路径有效）。
 */
function rmrf(target) {
  const r = spawnSync('/bin/rm', ['-rf', target], { encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 })
  if (r.status === 0) return { ok: true }
  try {
    fs.rmSync(target, { recursive: true, force: true })
    return { ok: true }
  } catch (e) {
    return { ok: false, error: `${(r.stderr || '').trim() || e.message}` }
  }
}

/** adhoc 签名。无 Apple 开发者证书时这是唯一正确做法（自签名证书会让 Team ID 不一致）。 */
function signApp(appPath) {  const r = spawnSync('/usr/bin/codesign', ['--deep', '--force', '--sign', '-', appPath],
    { encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 })
  return { ok: r.status === 0, out: `${r.stdout || ''}${r.stderr || ''}`.trim() }
}

/** 校验签名健康态；返回 { ok, out }。 */
function verifyApp(appPath) {
  const r = spawnSync('/usr/bin/codesign', ['--verify', '--deep', '--strict', appPath],
    { encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 })
  return { ok: r.status === 0, out: `${r.stdout || ''}${r.stderr || ''}`.trim() }
}

/**
 * 应用一个 release 到 installPath。
 *
 * 步骤：取 .app → 暂存到同卷 → adhoc 签名 → 原子改名替换。
 * 旧包改名到 `.zunzhe-old-<ts>` 而不是删除：正在运行的进程还 mmap 着它，
 * 立刻删可能出问题；留给下次启动时清理（cleanupStale）。
 *
 * 全程**不触碰数据目录**。
 */
function applyRelease({ release, installPath, log = () => {} }) {
  const installDir = path.dirname(installPath)
  let unzipTmp = null
  try {
    let srcApp = release.path
    if (release.kind === 'zip') {
      unzipTmp = fs.mkdtempSync(path.join(installDir, `${STAGE_PREFIX}unzip-`))
      log(`解压 ${release.path}`)
      const r = spawnSync('/usr/bin/ditto', ['-x', '-k', release.path, unzipTmp],
        { encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 })
      if (r.status !== 0) return { ok: false, error: `解压失败: ${(r.stderr || '').trim()}` }
      srcApp = findAppIn(unzipTmp)
      if (!srcApp) return { ok: false, error: '压缩包里没找到可用的 .app' }
    }

    const version = readBundleVersion(srcApp)
    if (!version) return { ok: false, error: '新版包里读不到版本号（Info.plist 缺 CFBundleShortVersionString）' }

    const stage = fs.mkdtempSync(path.join(installDir, STAGE_PREFIX))
    const stagedApp = path.join(stage, APP_BUNDLE_NAME)
    log(`暂存到 ${stagedApp}`)
    let r = spawnSync('/usr/bin/ditto', [srcApp, stagedApp],
      { encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 })
    if (r.status !== 0) return { ok: false, error: `拷贝失败: ${(r.stderr || '').trim()}` }

    log('adhoc 签名')
    const sg = signApp(stagedApp)
    if (!sg.ok) return { ok: false, error: `签名失败: ${sg.out}` }
    const vf = verifyApp(stagedApp)
    if (!vf.ok) return { ok: false, error: `签名校验未通过: ${vf.out}` }

    const oldDir = path.join(installDir, `${OLD_PREFIX}${Date.now()}`)
    log(`替换 ${installPath}`)
    fs.renameSync(installPath, oldDir)
    try {
      fs.renameSync(stagedApp, installPath)
    } catch (e) {
      try { fs.renameSync(oldDir, installPath) } catch (e2) { /* 回滚也失败，如实往上报 */ }
      return { ok: false, error: `替换失败（已尝试回滚）: ${e.message}` }
    }
    return { ok: true, version, oldDir, installDir }
  } catch (e) {
    return { ok: false, error: e.message }
  } finally {
    if (unzipTmp) {
      const del = rmrf(unzipTmp)
      if (!del.ok) log(`解压临时目录清理失败 ${unzipTmp}: ${del.error}`)
    }
  }
}

/**
 * 清掉上一次更新留下的暂存/旧包目录。只在启动时调用，只删自己造的前缀。
 *
 * 失败**不再静默**：macOS 13+ 的「App 管理」TCC 有可能拦住非公证应用删除
 * /Applications 下的 .app 包，那样旧包会一直留在内盘上吃空间。这种情况必须
 * 在日志里说清楚，否则用户只会看到磁盘莫名变小。
 */
function cleanupStale(installDir, log = () => {}) {
  const removed = []
  let names = []
  try { names = fs.readdirSync(installDir) } catch (e) {
    log(`读取 ${installDir} 失败: ${e.message}`)
    return removed
  }
  for (const n of names) {
    if (!n.startsWith(STAGE_PREFIX) && !n.startsWith(OLD_PREFIX)) continue
    const full = path.join(installDir, n)
    const del = rmrf(full)
    if (del.ok) {
      removed.push(full)
    } else {
      log(`清理失败 ${n}: ${del.error}（留在原处，可手动删）`)
    }
  }
  return removed
}

module.exports = {
  RELEASES_DIR_NAME,
  APP_BUNDLE_NAME,
  parseVersion,
  compareVersion,
  readBundleVersion,
  findAppIn,
  zipBundleVersion,
  listReleases,
  checkForUpdate,
  signApp,
  verifyApp,
  rmrf,
  applyRelease,
  cleanupStale,
}
