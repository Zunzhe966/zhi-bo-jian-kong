// Electron 层测试总入口：`npm test`。
//
// 之前 desktop/electron 下的 5 个 *.test.cjs 只能一个个手敲 node 去跑，
// 没有聚合入口，结果就是**没人跑**——2026-09-17 main.cjs 顶层引用未定义常量、
// 打包出来的 .app 一启动就弹 JavaScript error 的事故，本来只要跑一次测试
// 就能拦住。这里把「跑一遍」变成一条命令。
//
// 不引第三方测试框架：这些测试都是自包含的 assert 脚本，退出码即结论。

const { spawnSync } = require('child_process')
const fs = require('fs')
const path = require('path')

const electronDir = path.join(__dirname, '..', 'electron')
const files = fs.readdirSync(electronDir)
  .filter((f) => f.endsWith('.test.cjs'))
  .sort()

if (files.length === 0) {
  console.error('没找到任何 electron/*.test.cjs —— 测试入口配错了？')
  process.exit(1)
}

const failed = []
const passed = []
const srcTests = [
  path.join(__dirname, '..', 'src', 'stash.test.mjs'),
]

for (const f of files) {
  const full = path.join(electronDir, f)
  const r = spawnSync(process.execPath, [full], { encoding: 'utf8' })
  const ok = r.status === 0
  const label = f.padEnd(38)
  if (ok) {
    passed.push(f)
    console.log(`  PASS  ${label} ${(r.stdout || '').trim().split('\n').pop() || ''}`)
  } else {
    failed.push(f)
    console.log(`  FAIL  ${label}`)
    const out = `${r.stdout || ''}${r.stderr || ''}`.trim()
    for (const line of out.split('\n').slice(-14)) console.log(`        ${line}`)
  }
}

for (const full of srcTests) {
  const r = spawnSync(process.execPath, [full], { encoding: 'utf8' })
  const ok = r.status === 0
  const label = path.basename(full).padEnd(38)
  if (ok) {
    passed.push(path.basename(full))
    console.log(`  PASS  ${label} ${(r.stdout || '').trim().split('\n').pop() || ''}`)
  } else {
    failed.push(path.basename(full))
    console.log(`  FAIL  ${label}`)
    const out = `${r.stdout || ''}${r.stderr || ''}`.trim()
    for (const line of out.split('\n').slice(-14)) console.log(`        ${line}`)
  }
}

console.log()
console.log(`electron 层测试：${passed.length} 通过 / ${failed.length} 失败 / 共 ${files.length + srcTests.length}`)
if (failed.length) {
  console.log(`失败文件：${failed.join(', ')}`)
  process.exit(1)
}
