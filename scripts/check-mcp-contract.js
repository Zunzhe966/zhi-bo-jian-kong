#!/usr/bin/env node
// 真实 MCP stdio 契约检查：入口名、initialize 和 tools/list 都必须能用。
//
// 这个脚本不依赖 extension.rs 的源码可读性，直接运行当前 release 二进制。
// 它专门防止两类漂移：
//   1. 文档写着 extension，但 release 实际只接受 mcp；
//   2. 二进制能启动，但 stdio 握手或工具清单坏了。
//
// 用法：node scripts/check-mcp-contract.js [二进制路径]

const { spawnSync } = require('child_process')
const fs = require('fs')
const path = require('path')

const root = path.resolve(__dirname, '..')
const binary = path.resolve(
  root,
  process.argv[2] || 'zunzhe-core/target/release/zunzhe-core',
)

if (!fs.existsSync(binary)) {
  console.error(`找不到 release 二进制：${binary}`)
  process.exit(2)
}

const requests = [
  { jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '2024-11-05', capabilities: {}, clientInfo: { name: 'contract-check', version: '1' } } },
  { jsonrpc: '2.0', method: 'notifications/initialized', params: {} },
  { jsonrpc: '2.0', id: 2, method: 'tools/list', params: {} },
]

const run = spawnSync(binary, ['mcp'], {
  cwd: root,
  input: requests.map((r) => JSON.stringify(r)).join('\n') + '\n',
  encoding: 'utf8',
  maxBuffer: 8 * 1024 * 1024,
})

if (run.error) {
  console.error(`启动 MCP 失败：${run.error.message}`)
  process.exit(2)
}
if (run.status !== 0) {
  console.error(`MCP 退出码 ${run.status}\n${run.stderr || ''}`)
  process.exit(2)
}

const replies = (run.stdout || '')
  .split(/\r?\n/)
  .map((line) => line.trim())
  .filter(Boolean)
  .map((line) => {
    try { return JSON.parse(line) } catch { return null }
  })
  .filter(Boolean)

const init = replies.find((r) => r.id === 1)
const listed = replies.find((r) => r.id === 2)
if (!init || init.error || !init.result) {
  console.error(`initialize 失败：${JSON.stringify(init || null)}`)
  process.exit(1)
}
if (!listed || listed.error || !listed.result || !Array.isArray(listed.result.tools)) {
  console.error(`tools/list 失败：${JSON.stringify(listed || null)}`)
  process.exit(1)
}

const names = listed.result.tools.map((t) => t.name).filter(Boolean)
const required = ['live_status', 'tail_logs', 'process_media', 'doctor']
const missing = required.filter((name) => !names.includes(name))
if (missing.length) {
  console.error(`tools/list 缺少核心工具：${missing.join(', ')}`)
  process.exit(1)
}

const legacy = spawnSync(binary, ['extension', '--help'], { encoding: 'utf8' })
const legacyAccepted = legacy.status === 0

console.log(JSON.stringify({
  ok: true,
  binary,
  entry: 'mcp',
  tool_count: names.length,
  required_present: required,
  legacy_extension_accepted: legacyAccepted,
  hint: legacyAccepted
    ? 'mcp 与 extension 都可用；文档应选定唯一主入口。'
    : '当前只接受 mcp；文档和 extension-info 不能继续把 extension 写成可用入口。',
}, null, 2))
