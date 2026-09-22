#!/usr/bin/env node
'use strict'

// Read-only frontend/control-plane audit.
//
// It intentionally never starts Electron and never calls a mutating MCP tool.
// The source of truth for the MCP surface is the running binary's real
// initialize + tools/list exchange, because a checked-in JSON snapshot is
// exactly what drifted before.

const { spawnSync } = require('child_process')
const fs = require('fs')
const path = require('path')

const ROOT = path.resolve(__dirname, '..')

function read(file) {
  return fs.readFileSync(file, 'utf8')
}

function walkFiles(dir, out = []) {
  if (!fs.existsSync(dir)) return out
  for (const name of fs.readdirSync(dir).sort()) {
    const full = path.join(dir, name)
    const st = fs.statSync(full)
    if (st.isDirectory()) {
      if (name === 'node_modules' || name === 'dist' || name === 'release') continue
      walkFiles(full, out)
    } else if (/\.(?:jsx?|mjs|cjs)$/.test(name)) {
      out.push(full)
    }
  }
  return out
}

function lineAt(text, index) {
  let line = 1
  for (let i = 0; i < index; i++) if (text.charCodeAt(i) === 10) line++
  return line
}

function occurrences(text, regex, rel) {
  const out = []
  regex.lastIndex = 0
  for (const match of text.matchAll(regex)) {
    const value = match[1] || ''
    out.push({ value, file: rel, line: lineAt(text, match.index || 0) })
  }
  return out
}

// A few UI actions are necessarily dynamic, e.g.
//   cli(start ? 'start' : 'stop')
// The direct-call regex above cannot see either literal. Scan those
// expressions separately so the audit does not report a real button as
// "unwired" merely because both branches live inside one call.
function dynamicCliOccurrences(text, rel) {
  const out = []
  const regex = /\bcli\(\s*([A-Za-z_$][\w$]*)\s*\?\s*['"]([^'"]+)['"]\s*:\s*['"]([^'"]+)['"]/g
  for (const match of text.matchAll(regex)) {
    const line = lineAt(text, match.index || 0)
    out.push({ value: match[2], file: rel, line, expression: match[0] })
    out.push({ value: match[3], file: rel, line, expression: match[0] })
  }
  return out
}

function parseToolsList(binary) {
  const requests = [
    { jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '2024-11-05', capabilities: {}, clientInfo: { name: 'frontend-action-audit', version: '1' } } },
    { jsonrpc: '2.0', method: 'notifications/initialized', params: {} },
    { jsonrpc: '2.0', id: 2, method: 'tools/list', params: {} },
  ]
  const run = spawnSync(binary, ['extension'], {
    cwd: ROOT,
    input: requests.map((item) => JSON.stringify(item)).join('\n') + '\n',
    encoding: 'utf8',
    maxBuffer: 16 * 1024 * 1024,
    timeout: 60000,
  })
  if (run.error) throw new Error(`启动 ${binary} 失败: ${run.error.message}`)
  if (run.status !== 0) throw new Error(`${binary} extension 退出码 ${run.status}: ${(run.stderr || '').trim()}`)
  const replies = (run.stdout || '').split(/\r?\n/).filter(Boolean).map((line) => JSON.parse(line))
  const listed = replies.find((reply) => reply.id === 2)
  if (!listed || !listed.result || !Array.isArray(listed.result.tools)) {
    throw new Error('真实 tools/list 没有返回工具数组')
  }
  return listed.result.tools
}

function parseRiskClasses(controlListText) {
  const risks = new Map()
  for (const line of controlListText.split(/\r?\n/)) {
    const match = line.match(/\b([a-z][a-z0-9_]+)\b.*\b(read_only|config_write|queue_write|network|service_lifecycle|record_delete)\b/)
    if (match) risks.set(match[1], match[2])
  }
  return risks
}

function buildAudit() {
  const binary = path.resolve(ROOT, 'zunzhe-core/target/debug/zunzhe-core')
  if (!fs.existsSync(binary)) throw new Error(`找不到真实 extension 二进制: ${binary}`)
  const tools = parseToolsList(binary)
  const names = tools.map((tool) => tool.name)
  let risks = new Map()
  const controlRequest = {
    jsonrpc: '2.0',
    id: 3,
    method: 'tools/call',
    params: { name: 'control_list', arguments: {} },
  }
  const controlRun = spawnSync(binary, ['extension'], {
    cwd: ROOT,
    input: JSON.stringify(controlRequest) + '\n',
    encoding: 'utf8',
    maxBuffer: 16 * 1024 * 1024,
    timeout: 60000,
  })
  if (controlRun.status === 0) {
    try {
      const reply = JSON.parse((controlRun.stdout || '').trim().split(/\r?\n/).filter(Boolean).pop())
      const text = reply.result.content[0].text
      const body = JSON.parse(text)
      for (const control of body.controls || []) {
        if (control.name && control.risk) risks.set(control.name, control.risk)
      }
    } catch (error) {
      // 风险级别只是审计增强；真实工具清单失败才是硬错误。
      risks = parseRiskClasses(controlRun.stdout || '')
    }
  }

  const mainPath = path.join(ROOT, 'desktop/electron/main.cjs')
  const preloadPath = path.join(ROOT, 'desktop/electron/preload.cjs')
  const main = read(mainPath)
  const preload = read(preloadPath)
  const allowMatch = main.match(/const ALLOWED_CLI_COMMANDS = new Set\(\[([\s\S]*?)\]\)/)
  if (!allowMatch) throw new Error('未能解析 Electron CLI 白名单')
  const allowedCli = [...allowMatch[1].matchAll(/'([^']+)'/g)].map((m) => m[1])
  const ipcHandlers = [...main.matchAll(/ipcMain\.handle\(\s*'([^']+)'/g)].map((m) => m[1])
  const ipcInvokes = [...preload.matchAll(/ipcRenderer\.invoke\(\s*'([^']+)'/g)].map((m) => m[1])
  const desktopApis = [...preload.matchAll(/^\s*([A-Za-z][A-Za-z0-9_]*)\s*:/gm)].map((m) => m[1])
  const internalCliCalls = occurrences(main, /runCli\(\s*\[\s*['"]([^'"]+)['"]/g, 'desktop/electron/main.cjs')
  const hostTriggers = [
    { value: 'tray-menu:选择数据目录', file: 'desktop/electron/main.cjs', line: lineAt(main, main.indexOf("label: '选择数据目录…'")) },
    { value: 'tray-menu:打开数据目录', file: 'desktop/electron/main.cjs', line: lineAt(main, main.indexOf("label: '打开数据目录'")) },
    { value: 'tray-menu:检查更新', file: 'desktop/electron/main.cjs', line: lineAt(main, main.indexOf("label: '检查更新…'")) },
    { value: 'tray-menu:重新加载界面', file: 'desktop/electron/main.cjs', line: lineAt(main, main.indexOf("label: '重新加载界面'")) },
  ]

  const files = walkFiles(path.join(ROOT, 'desktop/src'))
  const cliCalls = []
  const desktopCalls = []
  for (const file of files) {
    const rel = path.relative(ROOT, file)
    const text = read(file)
    cliCalls.push(...occurrences(text, /\bcli\(\s*['"]([^'"]+)['"]/g, rel))
    cliCalls.push(...dynamicCliOccurrences(text, rel))
    desktopCalls.push(...occurrences(text, /window\.desktop\.([A-Za-z][A-Za-z0-9_]*)/g, rel))
  }

  const toolMap = new Map(names.map((name) => [name, {
    name,
    risk: risks.get(name) || null,
    description: tools.find((tool) => tool.name === name).description || '',
    cli: [],
    desktopIpc: [],
    frontend: [],
    internal: [],
    hostTriggers: [],
  }]))

  // The CLI/extension naming is not one-to-one. Keep the translation explicit
  // and reviewable; do not guess from string similarity.
  const cliToTool = {
    'live_status': 'live_status',
    'live_list': 'live_list',
    'live_add': 'live_add',
    'live_remove': 'live_remove',
    'live_delete': 'live_delete',
    'live_check': 'live_check',
    'live_set_notify': 'live_set_notify',
    'record_live': 'record_live',
    'service_status': 'service_status',
    'tail_logs': 'tail_logs',
    'video_add': 'video_add',
    'video_remove': 'video_remove',
    'video_list': 'video_list',
    'video_check_now': 'video_check_now',
    'blogger_videos': 'blogger_videos',
    'download_video': 'download_video',
    'video_download': 'video_download',
    'download_videos': 'download_videos',
    'video_library': 'video_library',
    'status': 'status',
    'export_training_data': 'export_training_data',
    'email_status': 'email_status',
    'email_setup': 'email_setup',
    'app_prefs_get': 'app_prefs_get',
    'app_prefs_set': 'app_prefs_set',
    'config_get': 'config_get',
    'config_set': 'config_set',
    'config_set_bulk': 'config_set_bulk',
    'config_schema_tool': 'config_schema_tool',
    'notes_list': 'notes_list',
    'process_media': 'process_media',
    'service_start': 'service_start',
    'service_stop': 'service_stop',
    'ensure_services': 'ensure_services',
    'doctor': 'doctor',
    'wizard_done': 'wizard_done',
    'test_email': 'test_email',
    'test_notify': 'test_notify',
    'notify_list': 'notify_list',
    'notify_retry': 'notify_retry',
    'notify_prune': 'notify_prune',
    'audit_list': 'audit_list',
    'control_list': 'control_list',
  }
  const cliAliases = {
    'add-live': 'live_add',
    'remove-live': 'live_remove',
    'delete-live': 'live_delete',
    'check-live': 'live_check',
    'set-notify': 'live_set_notify',
    'add-video': 'video_add',
    'remove-video': 'video_remove',
    'check-videos': 'video_check_now',
    'service-status': 'service_status',
    'blogger-videos': 'blogger_videos',
    'download-video': 'download_video',
    'video-download': 'video_download',
    'download-videos': 'download_videos',
    'process': 'process_media',
    'start': 'service_start',
    'stop': 'service_stop',
    'ensure-services': 'ensure_services',
    'wizard-done': 'wizard_done',
    'get-config': 'config_get',
    'set-config': 'config_set',
    'get-app-prefs': 'app_prefs_get',
    'set-app-prefs': 'app_prefs_set',
    'email-status': 'email_status',
    'test-email': 'test_email',
    'notify-list': 'notify_list',
    'notify-retry': 'notify_retry',
    'notify-prune': 'notify_prune',
    'tail-logs': 'tail_logs',
  }
  for (const call of cliCalls) {
    const tool = cliAliases[call.value] || cliToTool[call.value]
    if (tool && toolMap.has(tool)) toolMap.get(tool).cli.push(call)
  }
  for (const call of internalCliCalls) {
    const tool = cliAliases[call.value] || cliToTool[call.value]
    if (tool && toolMap.has(tool)) toolMap.get(tool).internal.push(call)
  }

  const ipcToTool = {
    'open-path': ['read_only', '本地打开会话/产物路径'],
    'show-item': ['read_only', '在访达定位文件'],
    'list-dir': ['read_only', '列出一层可信目录'],
    'open-external': ['network', '用系统浏览器打开 http/https'],
    'read-text': ['read_only', '只读预览文字稿'],
    'cover-url': ['read_only', '把可信封面路径转成 local-file URL'],
    'get-paths': ['read_only', '读取路径解析结果'],
    'choose-data-dir': ['config_write', '选择并固化数据目录'],
    'choose-dir': ['read_only', '选择目录，返回路径'],
    'choose-file': ['read_only', '选择文件，返回路径'],
    'check-update': ['file_write', '检查并可能应用程序更新'],
    'bili-login': ['credentials_write', '打开登录窗口并写 B站 Cookie'],
    'douyin-login': ['credentials_write', '打开登录窗口并写抖音 Cookie'],
    'reload-ui': ['local_ui', '重新加载界面'],
    'set-login-item': ['config_write', '设置开机启动'],
    'get-login-item': ['read_only', '读取开机启动状态'],
    'show-local-notify': ['local_ui', '弹出系统通知'],
  }
  for (const channel of ipcHandlers) {
    const meta = ipcToTool[channel]
    if (!meta) continue
    const key = `__ipc__:${channel}`
    if (!toolMap.has(key)) {
      toolMap.set(key, { name: key, risk: meta[0], description: meta[1], cli: [], desktopIpc: [], frontend: [], internal: [], hostTriggers: [] })
    }
  }
  for (const call of desktopCalls) {
    const meta = ipcToTool[call.value]
    if (meta) {
      const key = `__ipc__:${call.value}`
      if (!toolMap.has(key)) {
        toolMap.set(key, { name: key, risk: meta[0], description: meta[1], cli: [], desktopIpc: [], frontend: [], internal: [], hostTriggers: [] })
      }
      toolMap.get(key).frontend.push(call)
    }
  }
  const chooseDirEntry = toolMap.get('__ipc__:choose-data-dir')
  if (chooseDirEntry) chooseDirEntry.hostTriggers.push(hostTriggers[0])
  const getPathsEntry = toolMap.get('__ipc__:get-paths')
  if (getPathsEntry) getPathsEntry.hostTriggers.push(hostTriggers[1])
  const updateEntry = toolMap.get('__ipc__:check-update')
  if (updateEntry) updateEntry.hostTriggers.push(hostTriggers[2])
  const reloadEntry = toolMap.get('__ipc__:reload-ui')
  if (reloadEntry) reloadEntry.hostTriggers.push(hostTriggers[3])

  const result = [...toolMap.values()].map((item) => ({
    ...item,
    frontendReachable: item.frontend.length > 0 || item.cli.length > 0 || item.internal.length > 0 || item.hostTriggers.length > 0,
  }))
  return {
    schema_version: 1,
    generated_at: new Date().toISOString(),
    source: {
      binary,
      tool_count: names.length,
      tool_names: names,
      frontend_files_scanned: files.map((file) => path.relative(ROOT, file)).sort(),
    },
    summary: {
      tools: names.length,
      toolsWithFrontendCli: result.filter((item) => !item.name.startsWith('__ipc__:') && item.cli.length).length,
      toolsWithAnySourceEntry: result.filter((item) => !item.name.startsWith('__ipc__:') && item.frontendReachable).map((item) => item.name),
      toolsWithoutAnySourceEntry: result.filter((item) => !item.name.startsWith('__ipc__:') && !item.frontendReachable).map((item) => item.name),
      allowed_cli: allowedCli,
      ipc_handlers: ipcHandlers,
      ipc_invokes: ipcInvokes,
      desktop_apis: desktopApis,
      blocked_frontend_cli: cliCalls.map((item) => item.value).filter((value) => !allowedCli.includes(value)),
      frontend_cli: [...new Set(cliCalls.map((item) => item.value))].sort(),
      frontend_desktop_apis: [...new Set(desktopCalls.map((item) => item.value))].sort(),
    },
    entries: result,
  }
}

if (require.main === module) {
  try {
    process.stdout.write(JSON.stringify(buildAudit(), null, 2) + '\n')
  } catch (error) {
    console.error(error.stack || error.message)
    process.exit(1)
  }
}

module.exports = { buildAudit }
