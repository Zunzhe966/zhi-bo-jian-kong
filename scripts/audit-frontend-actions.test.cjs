#!/usr/bin/env node
'use strict'

// Contract test for scripts/audit-frontend-actions.js.
//
// The audit must be conservative and evidence-based: it reads the real
// extension tools/list surface and source code, but it must not execute any
// mutating tool. These assertions lock the two failure modes we have already
// seen: stale tool counts and missing dynamic CLI calls.

const assert = require('assert')
const { buildAudit } = require('./audit-frontend-actions.js')

const audit = buildAudit()

assert.strictEqual(audit.source.tool_count, 43, '真实 tools/list 必须是 43 个工具')
assert.strictEqual(audit.summary.tools, 43, '审计摘要必须保留真实工具数')
assert.deepStrictEqual(audit.summary.blocked_frontend_cli, [], '前端不得调用 Electron 白名单外的 CLI')

const serviceStop = audit.entries.find((item) => item.name === 'service_stop')
assert.ok(serviceStop, '控制面必须包含 service_stop')
assert.ok(
  serviceStop.cli.some((call) => call.value === 'stop'),
  '动态调用 cli(start ? "start" : "stop") 必须被识别为 service_stop 前端入口',
)

const serviceStart = audit.entries.find((item) => item.name === 'service_start')
assert.ok(serviceStart, '控制面必须包含 service_start')
assert.ok(
  serviceStart.cli.some((call) => call.value === 'start'),
  '动态调用 cli(start ? "start" : "stop") 必须同时识别出 start',
)

const status = audit.entries.find((item) => item.name === 'status')
assert.ok(status, '控制面必须包含 status')
assert.ok(
  status.internal.some((call) => call.value === 'status'),
  'Electron runStatusCached 的内部 status 调用必须算作可达入口',
)

const serviceStatus = audit.entries.find((item) => item.name === 'service_status')
assert.ok(serviceStatus, '控制面必须包含 service_status')
assert.ok(
  serviceStatus.internal.some((call) => call.value === 'service-status'),
  '后台看门狗调用的 service-status 必须算作可达入口',
)

const trayChooseDataDir = audit.entries.find((item) => item.name === '__ipc__:choose-data-dir')
assert.ok(trayChooseDataDir, '审计必须记录 choose-data-dir IPC')
assert.ok(
  trayChooseDataDir.hostTriggers.some((call) => call.value === 'tray-menu:选择数据目录'),
  '托盘菜单「选择数据目录」必须算作 choose-data-dir 的真实入口',
)

for (const api of ['openPath', 'showItem', 'checkUpdate', 'biliLogin', 'douyinLogin']) {
  assert.ok(
    audit.summary.frontend_desktop_apis.includes(api),
    `前端动作扫描不得漏掉 window.desktop.${api}`,
  )
}

console.log('frontend action audit contract: ok')
