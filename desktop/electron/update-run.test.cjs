// update-run.cjs 的单测。
//
// 为什么这个模块必须有测试：它是「更新」这条路上唯一会**动文件系统**的代码，
// 而且动的是 /Applications 里的应用包。这里出一次错就是「点一下更新，程序没了」，
// 比数据丢失更难恢复。所以版本比较、候选挑选、替换边界都要能直接测。
//
// 全程只在 mkdtemp 造的临时目录里操作，不碰真实的 /Applications 和数据目录。

const fs = require('fs')
const os = require('os')
const path = require('path')

const u = require('./update-run.cjs')

// 收集所有失败再统一汇报：这类测试一挂就 exit 的话，后面的断言永远跑不到，
// 只能一个问题一个问题修，很费时间。
let checks = 0
const failures = []
function ok(cond, label) {
  checks++
  if (cond) {
    console.log(`  ✓ ${label}`)
  } else {
    failures.push(label)
    console.log(`  ✗ ${label}`)
  }
}

const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'zunzhe-update-test-'))

/** 造一个假的 .app：只需要 Contents/Info.plist 有版本号。 */
function makeFakeApp(dir, version, marker = '') {
  const app = path.join(dir, '尊者直播监控.app')
  const contents = path.join(app, 'Contents')
  fs.mkdirSync(path.join(contents, 'MacOS'), { recursive: true })
  fs.writeFileSync(path.join(contents, 'Info.plist'), `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>尊者直播监控</string>
  <key>CFBundleIdentifier</key><string>com.zunzhe.livemonitor</string>
  <key>CFBundleShortVersionString</key><string>${version}</string>
</dict></plist>
`, 'utf8')
  if (marker) fs.writeFileSync(path.join(contents, 'MacOS', marker), marker, 'utf8')
  return app
}

// ---------- 版本号比较 ----------
ok(u.parseVersion('1.2.3').join('.') === '1.2.3', "parseVersion('1.2.3')")
ok(u.parseVersion('v1.2.3').join('.') === '1.2.3', "parseVersion 吃掉 v 前缀")
ok(u.compareVersion('1.2.0', '1.1.9') === 1, '1.2.0 > 1.1.9')
ok(u.compareVersion('1.1.0', '1.1') === 0, '1.1.0 == 1.1（缺位按 0 补）')
ok(u.compareVersion('1.0.0', '1.0.1') === -1, '1.0.0 < 1.0.1')
ok(u.compareVersion('2.0.0', '10.0.0') === -1, '2.0.0 < 10.0.0（按数值比，不是字典序）')
// 预发布语义：正式版必须比同名 beta 新。旧实现把 'beta.1' 的 1 当成第 4 个数字，
// 判成 beta 更新 —— 更新源混入 beta 就会把正式版用户降版（2026-09-26 实测）。
ok(u.compareVersion('1.7.3-beta.1', '1.7.3') === -1, '1.7.3-beta.1 < 1.7.3（beta 更旧）')
ok(u.compareVersion('1.7.3', '1.7.3-beta.1') === 1, '1.7.3 > 1.7.3-beta.1（正式版更新）')
ok(u.compareVersion('1.7.3-beta.2', '1.7.3-beta.1') === 1, 'beta.2 > beta.1')
ok(u.compareVersion('1.7.3-beta.1', '1.7.3-beta.1') === 0, '同名 beta 相等')

// ---------- 在解压目录里找 .app（含嵌套） ----------
const nested = fs.mkdtempSync(path.join(tmp, 'nested-'))
const deepDir = path.join(nested, '外层')
fs.mkdirSync(deepDir, { recursive: true })
const nestedApp = makeFakeApp(deepDir, '9.9.9')
ok(u.findAppIn(nested) === nestedApp, 'findAppIn 能在嵌套一层里找到 .app')
ok(u.findAppIn(fs.mkdtempSync(path.join(tmp, 'empty-'))) === null, '空目录返回 null')

// ---------- 读版本号 ----------
ok(u.readBundleVersion(nestedApp) === '9.9.9', 'readBundleVersion 读 Info.plist')
ok(u.readBundleVersion(path.join(tmp, '不存在的.app')) === '', '不存在的包返回空串')

// ---------- checkForUpdate：挑最高版本 ----------
const rel = path.join(tmp, 'releases')
fs.mkdirSync(rel, { recursive: true })
makeFakeApp(path.join(fs.mkdirSync(path.join(rel, 'a'), { recursive: true }) || path.join(rel, 'a')), '1.0.0')
makeFakeApp(path.join(fs.mkdirSync(path.join(rel, 'b'), { recursive: true }) || path.join(rel, 'b')), '1.3.0')
makeFakeApp(path.join(fs.mkdirSync(path.join(rel, 'c'), { recursive: true }) || path.join(rel, 'c')), '1.2.0')
// 杂物：必须被忽略，不能因为看到个文件夹就当版本
fs.writeFileSync(path.join(rel, 'README.txt'), '说明', 'utf8')
fs.mkdirSync(path.join(rel, '.DS_Store-dir'), { recursive: true })

const r1 = u.checkForUpdate({ releasesDir: rel, currentVersion: '1.1.0', tmpDir: tmp })
ok(r1.ok === true, 'checkForUpdate 正常返回')
ok(r1.latest && r1.latest.version === '1.3.0', '挑出最高版本 1.3.0（不是 readdir 的第一个）')
ok(r1.hasUpdate === true, '1.3.0 > 1.1.0 → hasUpdate')

const r2 = u.checkForUpdate({ releasesDir: rel, currentVersion: '1.3.0', tmpDir: tmp })
ok(r2.hasUpdate === false, '同版本不算有更新')

const r3 = u.checkForUpdate({ releasesDir: rel, currentVersion: '2.0.0', tmpDir: tmp })
ok(r3.hasUpdate === false, '本地比 releases 新时不下沉（不降级）')

const r4 = u.checkForUpdate({ releasesDir: path.join(tmp, '不存在'), currentVersion: '1.0.0', tmpDir: tmp })
ok(r4.ok === true && r4.hasUpdate === false && !r4.latest, 'releases 目录不存在时安静地返回「无更新」')

// ---------- applyRelease：替换边界 ----------
const installDir = fs.mkdtempSync(path.join(tmp, 'Applications-'))
const installPath = makeFakeApp(installDir, '1.1.0', '旧版本标记.txt') // 假装是已安装的旧版
const srcRel = path.join(tmp, 'src-rel')
fs.mkdirSync(srcRel, { recursive: true })
const newApp = makeFakeApp(srcRel, '1.4.0', '新版本标记.txt')

const applied = u.applyRelease({
  release: { path: newApp, kind: 'app', version: '1.4.0', name: '尊者直播监控.app' },
  installPath,
  log: () => {},
})
ok(applied.ok === true, `applyRelease 成功（${applied.error || ''}）`)
ok(fs.existsSync(installPath), '替换后安装位仍然存在')
ok(u.readBundleVersion(installPath) === '1.4.0', '安装位已是新版 1.4.0')
ok(fs.existsSync(path.join(installPath, 'Contents', 'MacOS', '新版本标记.txt')), '安装位内容确实来自新版')
ok(fs.existsSync(applied.oldDir), '旧包被改名保留（不是直接删）')
ok(u.readBundleVersion(applied.oldDir) === '1.1.0', '保留下来的确实是旧版 1.1.0')

// ---------- cleanupStale：只删自己造的前缀 ----------
const bystander = path.join(installDir, '别动我.app')
fs.mkdirSync(bystander, { recursive: true })
const removed = u.cleanupStale(installDir)
ok(!fs.existsSync(applied.oldDir), 'cleanupStale 清掉了 .zunzhe-old-*')
ok(fs.existsSync(installPath), 'cleanupStale 不动正常的安装位')
ok(fs.existsSync(bystander), 'cleanupStale 绝不动无关目录')

// 解压目录的临时残留也必须被清（applyRelease 的 finally）
ok(removed.every((p) => path.basename(p).startsWith('.zunzhe-')), 'cleanupStale 只返回自己造的前缀')

// ---------- 数据目录绝不被触碰 ----------
const dataDir = fs.mkdtempSync(path.join(tmp, 'data-'))
fs.writeFileSync(path.join(dataDir, 'zunzhe-core.db'), 'not-empty', 'utf8')
const before = fs.readdirSync(dataDir).join(',')
u.applyRelease({
  release: { path: newApp, kind: 'app', version: '1.4.0', name: 'x' },
  installPath: path.join(fs.mkdtempSync(path.join(tmp, 'Applications2-')), '尊者直播监控.app'),
  log: () => {},
})
ok(fs.readdirSync(dataDir).join(',') === before, 'applyRelease 全程不碰数据目录')

// ---------- rmrf：必须能删掉「含 app.asar 的目录树」 ----------
//
// 回归测试。踩过的坑：Electron 会改写主进程的 fs，把含 `.asar` 的路径当成
// 压缩包虚拟目录，`fs.rmSync` 递归到 `Resources/app.asar` 时会去 rmdir 一个
// 文件，内核回 ENOTDIR —— 结果是应用包永远删不掉，每次更新在 /Applications
// 里留一份 288MB 残骸。所以删包一律走 rmrf（内部用系统 /bin/rm）。
const bundleLike = path.join(tmp, 'bundleLike', '尊者直播监控.app')
fs.mkdirSync(path.join(bundleLike, 'Contents', 'Resources'), { recursive: true })
fs.writeFileSync(path.join(bundleLike, 'Contents', 'Resources', 'app.asar'), 'fake-asar-bytes', 'utf8')
fs.mkdirSync(path.join(bundleLike, 'Contents', 'Frameworks', 'X.framework', 'Versions', 'A'), { recursive: true })
fs.symlinkSync('A', path.join(bundleLike, 'Contents', 'Frameworks', 'X.framework', 'Versions', 'Current'))
const del = u.rmrf(path.join(tmp, 'bundleLike'))
ok(del.ok, 'rmrf 能删掉含 app.asar + framework 软链的包目录')
ok(!fs.existsSync(path.join(tmp, 'bundleLike')), 'rmrf 删完确实不存在了')

// cleanupStale 走的就是 rmrf，所以对「包形态」的残留同样有效
const pkgLeftover = path.join(installDir, '.zunzhe-old-999')
fs.mkdirSync(path.join(pkgLeftover, 'Contents', 'Resources'), { recursive: true })
fs.writeFileSync(path.join(pkgLeftover, 'Contents', 'Resources', 'app.asar'), 'x', 'utf8')
const removed2 = u.cleanupStale(installDir)
ok(!fs.existsSync(pkgLeftover), 'cleanupStale 删得掉带 app.asar 的旧包残留')
ok(removed2.some((p) => path.basename(p) === '.zunzhe-old-999'), 'cleanupStale 把该包记为已清理')

fs.rmSync(tmp, { recursive: true, force: true })
console.log(`\nupdate-run: ${checks - failures.length} 通过 / ${failures.length} 失败 / 共 ${checks}`)
if (failures.length) {
  for (const f of failures) console.log(`  - ${f}`)
  process.exit(1)
}
