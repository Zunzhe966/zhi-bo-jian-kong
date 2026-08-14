const assert = require('assert')
const path = require('path')
const fs = require('fs')
const os = require('os')
const { healthcheckInvocation } = require('./healthcheck.cjs')

const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'zunzhe-healthcheck-'))
try {
  fs.mkdirSync(path.join(dir, 'scripts'), { recursive: true })
  fs.writeFileSync(path.join(dir, 'scripts', 'check-rust-daemon.sh'), '#!/bin/bash\n')
  fs.writeFileSync(path.join(dir, 'scripts', 'check-rust-daemon.ps1'), '# ps1\n')

  const unix = healthcheckInvocation(dir, 'darwin')
  assert.ok(unix, 'unix 应返回调用')
  assert.strictEqual(unix.shell, '/bin/bash')
  assert.deepStrictEqual(unix.args, [path.join(dir, 'scripts', 'check-rust-daemon.sh')])

  const win = healthcheckInvocation(dir, 'win32')
  assert.ok(win, 'windows 应返回调用')
  assert.strictEqual(win.shell, 'powershell')
  assert.deepStrictEqual(win.args, [
    '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File',
    path.join(dir, 'scripts', 'check-rust-daemon.ps1'),
  ])

  const none = healthcheckInvocation(path.join(dir, 'empty'), 'win32')
  assert.strictEqual(none, null, '脚本缺失时应返回 null')
  console.log('healthcheck test ok')
} finally {
  fs.rmSync(dir, { recursive: true, force: true })
}
