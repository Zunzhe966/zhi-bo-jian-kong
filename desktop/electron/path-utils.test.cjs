// path-utils.cjs 的单测。纯 Node 运行，不依赖 electron / 测试框架：
//   node desktop/electron/path-utils.test.cjs
//
// 重点是回归原 bug：旧白名单写死成 [应用包目录, userData]，而界面要打开的是
// `~/Projects/尊者监控直播短视频`，导致「打开目录」按钮静默失效、连报错都没有。

const fs = require('fs')
const os = require('os')
const path = require('path')

const u = require('./path-utils.cjs')

let passed = 0
const failures = []

function check(name, cond, detail) {
  if (cond) {
    passed += 1
    console.log(`  ok   ${name}`)
  } else {
    failures.push(`${name}${detail ? ` — ${detail}` : ''}`)
    console.log(`  FAIL ${name}${detail ? ` — ${detail}` : ''}`)
  }
}

function mkTemp(tag) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), `zunzhe-pathutils-${tag}-`))
  return dir
}

// ---------- looksLikeProject ----------
console.log('\nlooksLikeProject')
{
  const dir = mkTemp('looks')
  check('空目录不算项目', u.looksLikeProject(dir) === false, dir)
  fs.mkdirSync(path.join(dir, 'config'), { recursive: true })
  fs.writeFileSync(path.join(dir, 'config', 'config.ini'), '[录制设置]\n')
  check('有 config/config.ini 就算项目', u.looksLikeProject(dir) === true)

  const dir2 = mkTemp('looks-db')
  fs.writeFileSync(path.join(dir2, 'zunzhe-core.db'), '')
  check('有 zunzhe-core.db 也算项目', u.looksLikeProject(dir2) === true)
  check('空值/null 不算项目', u.looksLikeProject('') === false && u.looksLikeProject(null) === false)
  fs.rmSync(dir, { recursive: true, force: true })
  fs.rmSync(dir2, { recursive: true, force: true })
}

// ---------- candidateDataDirs ----------
console.log('\ncandidateDataDirs 优先级')
{
  const home = mkTemp('home')
  const pointerDir = mkTemp('pointer-target')
  const pointerFile = path.join(mkTemp('pointer'), '数据目录.txt')
  fs.writeFileSync(pointerFile, '# 注释行\n\n' + pointerDir + '\n')

  const list = u.candidateDataDirs({
    envDir: '/tmp/zz-env-dir',
    pointerFile,
    home,
    projectName: '尊者监控直播短视频',
    volumesRoot: path.join(home, 'no-such-volumes'),
  })
  check('环境变量排第一', list[0] === '/tmp/zz-env-dir', list[0])
  check('指针文件排第二', list[1] === pointerDir, list[1])
  check('Projects 候选项存在', list.includes(path.join(home, 'Projects', '尊者监控直播短视频')))
  check('Desktop 候选项存在', list.includes(path.join(home, 'Desktop', '尊者监控直播短视频')))
  check('注释与空行被跳过', !list.some((d) => d.startsWith('#')))

  const noEnv = u.candidateDataDirs({
    envDir: undefined,
    pointerFile: '',
    home,
    projectName: '尊者监控直播短视频',
    volumesRoot: path.join(home, 'no-such-volumes'),
  })
  check('没有环境变量/指针文件时不留空项', noEnv.every((d) => d && d.trim()) && noEnv.length >= 2)
  fs.rmSync(home, { recursive: true, force: true })
  fs.rmSync(pointerDir, { recursive: true, force: true })
}

// ---------- pickDataDir ----------
console.log('\npickDataDir')
{
  const real = mkTemp('pick-real')
  fs.writeFileSync(path.join(real, 'zunzhe-core.db'), '')
  const fallback = '/tmp/zz-fallback'
  check(
    '跳过不存在的候选，挑第一个像样的',
    u.pickDataDir(['/tmp/zz-does-not-exist-xyz', real, '/tmp/zz-later'], fallback) === real,
  )
  check('都不像时用 fallback',
    u.pickDataDir(['/tmp/zz-does-not-exist-xyz'], fallback) === fallback)
  fs.rmSync(real, { recursive: true, force: true })
}

// ---------- isTrustedPath ----------
console.log('\nisTrustedPath')
{
  const root = mkTemp('trust-root')
  const projectRoot = mkTemp('trust-project')
  const roots = [projectRoot, root]

  check('根目录自身可信', u.isTrustedPath(root, roots) === true)
  check('根目录下的路径可信', u.isTrustedPath(path.join(root, 'a', 'b.mp4'), roots) === true)
  check('界外路径被拒', u.isTrustedPath('/etc/passwd', roots) === false)
  check('空值被拒', u.isTrustedPath('', roots) === false && u.isTrustedPath(null, roots) === false)
  check('空白名单一律拒绝', u.isTrustedPath(root, []) === false)
  // 前缀相似但不是子目录（/tmp/xx 与 /tmp/xx-evil）不能误判
  const sibling = `${root}-evil`
  check('同前缀的兄弟目录不当成子目录', u.isTrustedPath(path.join(sibling, 'x'), roots) === false)
  fs.rmSync(root, { recursive: true, force: true })
  fs.rmSync(projectRoot, { recursive: true, force: true })
}

// ---------- 回归：旧白名单会拒掉的路径，新白名单必须放行 ----------
console.log('\n回归：原「打开目录」静默失效 bug')
{
  // 模拟打包态：应用包目录 + userData 是旧白名单的两个根。
  const appBundle = mkTemp('app-bundle')
  const userData = mkTemp('userdata')
  const oldRoots = [appBundle, userData]

  // 真实数据目录（旧实现里是硬编码的 ~/Projects/尊者监控直播短视频）。
  const dataDir = mkTemp('real-data')
  const logsDir = path.join(dataDir, 'logs')
  fs.mkdirSync(logsDir, { recursive: true })

  check('旧白名单确实会拒掉真实日志目录（这就是那个 bug）',
    u.isTrustedPath(logsDir, oldRoots) === false)

  // 新实现：白名单由核心上报，数据目录在里面。
  const productRoot = mkTemp('product-root')
  const newRoots = [...oldRoots, dataDir, productRoot]
  check('新白名单放行真实日志目录', u.isTrustedPath(logsDir, newRoots) === true)
  check('新白名单仍拒绝对外越权', u.isTrustedPath('/etc/passwd', newRoots) === false)

  // 产物被搬到项目之外的专属目录（[路径设置] 产物根目录）后也必须可信。
  const recordings = path.join(productRoot, 'downloads', 'B站直播', '主播')
  fs.mkdirSync(recordings, { recursive: true })
  check('自定义产物根下的录像目录可信', u.isTrustedPath(recordings, newRoots) === true)

  fs.rmSync(appBundle, { recursive: true, force: true })
  fs.rmSync(userData, { recursive: true, force: true })
  fs.rmSync(dataDir, { recursive: true, force: true })
  fs.rmSync(productRoot, { recursive: true, force: true })
}

// ---------- resolveOpenablePath ----------
console.log('\nresolveOpenablePath')
{
  const root = mkTemp('openable')
  const roots = [root]
  const existing = path.join(root, 'fupan_output')
  fs.mkdirSync(existing, { recursive: true })

  check('已存在的目录原样返回',
    u.resolveOpenablePath(existing, roots) === existing)
  check('尚未创建的目录退到最近的已存在祖先',
    u.resolveOpenablePath(path.join(root, 'downloads', '短视频', '主播A'), roots) === root)
  check('界外路径返回 null',
    u.resolveOpenablePath('/etc/passwd', roots) === null)
  check('空路径返回 null',
    u.resolveOpenablePath('', roots) === null && u.resolveOpenablePath(undefined, roots) === null)
  fs.rmSync(root, { recursive: true, force: true })
}

// ---------- macOS 符号链接别名 ----------
console.log('\n符号链接别名（macOS /tmp -> /private/tmp）')
{
  if (process.platform === 'darwin') {
    const tag = `zunzhe-alias-${process.pid}`
    const real = path.join('/private/tmp', tag)
    fs.mkdirSync(real, { recursive: true })
    const alias = path.join('/tmp', tag)
    const viaAlias = path.join(alias, 'downloads')
    fs.mkdirSync(viaAlias, { recursive: true })

    check('真实路径与别名互相认可（否则白名单会误拒）',
      u.isTrustedPath(path.join(real, 'downloads'), [alias]) === true)
    check('反向也成立',
      u.isTrustedPath(viaAlias, [real]) === true)
    check('不相关路径仍被拒',
      u.isTrustedPath('/private/tmp/zunzhe-alias-other/x', [alias]) === false)
    fs.rmSync(real, { recursive: true, force: true })
  } else {
    console.log('  跳过（非 darwin）')
  }
}

// ---------- firstProjectDir ----------
console.log('\nfirstProjectDir（找不到必须返回 null，不能悄悄兜底到空库）')
{
  const real = mkTemp('fpd')
  fs.mkdirSync(path.join(real, 'config'), { recursive: true })
  fs.writeFileSync(path.join(real, 'config', 'config.ini'), 'x', 'utf8')
  const empty = mkTemp('fpd-empty')
  const empty2 = mkTemp('fpd-empty2')

  check('命中第一个像样的目录',
    u.firstProjectDir(['/tmp/zz-nope-xyz', empty, real, empty2]) === real)
  check('一个都不像时返回 null（而不是 fallback）',
    u.firstProjectDir([empty, empty2, '/tmp/zz-nope-xyz']) === null)
  check('空数组返回 null', u.firstProjectDir([]) === null)
  check('只有 zunzhe-core.db 也算数据目录', (() => {
    const d = mkTemp('fpd-db')
    fs.writeFileSync(path.join(d, 'zunzhe-core.db'), '', 'utf8')
    return u.firstProjectDir([d]) === d
  })())
  check('pickDataDir 的老行为不变（仍然兜底）',
    u.pickDataDir([empty], '/tmp/zz-fallback') === '/tmp/zz-fallback')

  for (const d of [real, empty, empty2]) fs.rmSync(d, { recursive: true, force: true })
}

// ---------- volumesCandidates 顺序必须稳定 ----------
console.log('\nvolumesCandidates 顺序稳定（否则会「今天读这个库、明天读那个库」）')
{
  const vol = mkTemp('vols')
  for (const n of ['zeta', 'Alpha', 'beta']) fs.mkdirSync(path.join(vol, n), { recursive: true })
  fs.mkdirSync(path.join(vol, '.hidden'), { recursive: true })
  const a = u.volumesCandidates('项目X', vol)
  const b = u.volumesCandidates('项目X', vol)
  const bases = a.map((p) => path.basename(path.dirname(p)))
  check('两次调用结果完全一致', JSON.stringify(a) === JSON.stringify(b))
  check('忽略点号开头的卷', !a.some((p) => p.includes('.hidden')))
  check('数量正确', a.length === 3, `实际 ${a.length}`)
  check('按 localeCompare 排序', JSON.stringify(bases) === JSON.stringify(['Alpha', 'beta', 'zeta']),
    JSON.stringify(bases))
  fs.rmSync(vol, { recursive: true, force: true })
}

console.log(`\n通过 ${passed} 项，失败 ${failures.length} 项`)
if (failures.length) {
  for (const f of failures) console.log(`  - ${f}`)
  process.exit(1)
}
