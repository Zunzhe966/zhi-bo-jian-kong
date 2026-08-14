// 桌面端健康检查脚本调度：按平台选择可执行的检查器。
// Unix 用 /bin/bash 跑 scripts/check-rust-daemon.sh；Windows 用 powershell 跑
// scripts/check-rust-daemon.ps1，避免 Windows 上 execFile('/bin/bash') 失败导致
// 24×30 心跳日志缺失。
const path = require('path')

/**
 * 返回 { shell, args } 或 null（脚本不存在时）。
 * projectDir 是项目根目录，platform 用 process.platform 语义。
 */
function healthcheckInvocation(projectDir, platform) {
  const root = projectDir || process.cwd()
  const isWin = platform === 'win32'
  const checker = path.join(root, 'scripts', isWin ? 'check-rust-daemon.ps1' : 'check-rust-daemon.sh')
  if (!require('fs').existsSync(checker)) return null
  if (isWin) {
    return {
      shell: 'powershell',
      args: ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', checker],
    }
  }
  return { shell: '/bin/bash', args: [checker] }
}

module.exports = { healthcheckInvocation }
