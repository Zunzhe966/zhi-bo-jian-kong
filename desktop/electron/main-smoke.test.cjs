// 主进程「能不能加载」的冒烟测试。
//
// 2026-09-17 打包事故复盘：main.cjs 顶层用了 LOCAL_FILE_SCHEME，但那个符号
// 从来没有被定义或引入，模块一加载就 ReferenceError，Electron 弹出
// "A JavaScript error occurred in the main process"，whenReady 里的
// createWindow() 永远不执行，窗口永远不出现。打包、签名、装到 /Applications
// 全都成功了，唯独这个没人测。
//
// 更早一次是同一条线的另一种形状：require 里漏了 protocol，顶层调用
// protocol.registerSchemesAsPrivileged 直接 TypeError。
//
// 这个测试不启动 Electron，只用桩把 electron 模块换掉，然后 require
// main.cjs，断言：
//   1. 顶层加载不抛异常（no-undef 类错误的最后一道网）
//   2. 特权 scheme 真的注册了，且 scheme 是合法非空字符串
//      （scheme 为 undefined 时 Electron 会抛 "Argument must be an array
//       of custom schemes."，正是这次崩溃的现场）
//   3. IPC 与 preload 的通道表对得上（少一个通道界面就有一个按钮点不动）

const assert = require('assert')
const Module = require('module')
const fs = require('fs')
const path = require('path')

const schemeRegistrations = []
const ipcHandlers = new Set()
const appListeners = new Set()
const trace = []

function rec(name) {
  return (...args) => { trace.push([name, args]) }
}

const electronStub = {
  app: {
    isPackaged: false,
    getName: () => 'zunzhe-smoke-test',
    setName: rec('app.setName'),
    disableHardwareAcceleration: rec('app.disableHardwareAcceleration'),
    commandLine: { appendSwitch: rec('app.commandLine.appendSwitch') },
    requestSingleInstanceLock: () => true,
    on: (ev) => { appListeners.add(ev); return electronStub.app },
    // 刻意永不 resolve：本测试只验证「顶层加载」这一步，
    // whenReady 回调里的窗口/托盘/后台任务属于运行时，交给真机验证。
    whenReady: () => new Promise(() => {}),
    getPath: (k) => path.join('/tmp', 'zunzhe-smoke', k),
    getLoginItemSettings: () => ({ openAtLogin: false }),
    setLoginItemSettings: rec('app.setLoginItemSettings'),
    quit: rec('app.quit'),
  },
  BrowserWindow: function BrowserWindow() {
    return {
      loadFile: rec('win.loadFile'),
      show: rec('win.show'),
      hide: rec('win.hide'),
      on: rec('win.on'),
      once: rec('win.once'),
      isDestroyed: () => false,
      webContents: {
        on: rec('wc.on'),
        setWindowOpenHandler: rec('wc.setWindowOpenHandler'),
        send: rec('wc.send'),
      },
    }
  },
  Tray: function Tray() {
    return {
      setToolTip: rec('tray.setToolTip'),
      setContextMenu: rec('tray.setContextMenu'),
      on: rec('tray.on'),
    }
  },
  Menu: {
    buildFromTemplate: (tpl) => tpl,
    setApplicationMenu: rec('menu.setApplicationMenu'),
  },
  globalShortcut: {
    register: () => true,
    unregisterAll: rec('gs.unregisterAll'),
  },
  ipcMain: {
    handle: (ch) => { ipcHandlers.add(ch) },
    on: (ch) => { ipcHandlers.add(ch) },
  },
  shell: {
    openPath: async () => '',
    openExternal: async () => {},
  },
  nativeImage: {
    createFromPath: () => ({ isEmpty: () => false, resize: () => ({}) }),
  },
  Notification: function Notification() {
    return { show: rec('notification.show') }
  },
  protocol: {
    registerSchemesAsPrivileged: (schemes) => {
      assert.ok(Array.isArray(schemes), 'registerSchemesAsPrivileged 必须收到数组')
      assert.ok(schemes.length > 0, '至少要注册一个特权 scheme')
      for (const s of schemes) {
        assert.ok(s && typeof s === 'object', `scheme 条目必须是对象，实际是 ${typeof s}`)
        // 这一条正是 09-17 崩溃的直接判据：scheme 为 undefined 时 Electron
        // 会抛 "Argument must be an array of custom schemes."
        assert.strictEqual(
          typeof s.scheme, 'string',
          `scheme 必须是字符串，实际是 ${typeof s.scheme}（${String(s.scheme)}）—— 说明引用了没定义的常量`,
        )
        assert.ok(s.scheme.length > 0, 'scheme 不能是空串')
        const p = s.privileges
        assert.ok(p && typeof p === 'object', 'privileges 必须是对象')
      }
      schemeRegistrations.push(...schemes)
    },
    handle: rec('protocol.handle'),
  },
  net: {
    fetch: async () => new Response(''),
  },
}

// 用桩接管 require('electron')，其余模块照常真实加载。
const originalLoad = Module._load
Module._load = function patchedLoad(request, parent, isMain) {
  if (request === 'electron') return electronStub
  return originalLoad.call(this, request, parent, isMain)
}

let loaded = false
let loadError = null
try {
  require('./main.cjs')
  loaded = true
} catch (e) {
  loadError = e
} finally {
  Module._load = originalLoad
}

assert.ok(
  loaded,
  `main.cjs 顶层加载抛异常，打包出来的应用会一启动就白屏：${loadError && (loadError.stack || loadError.message)}`,
)

// 1. 特权 scheme 注册正确
assert.strictEqual(schemeRegistrations.length, 1, `应恰好注册 1 个特权 scheme，实际 ${schemeRegistrations.length}`)
assert.strictEqual(schemeRegistrations[0].scheme, 'local-file', '封面协议 scheme 应为 local-file')
assert.strictEqual(schemeRegistrations[0].privileges.secure, true, '封面协议必须声明 secure')
assert.strictEqual(schemeRegistrations[0].privileges.supportFetchAPI, true, '封面协议必须声明 supportFetchAPI')

// 2. 顶层确实执行到了（防「测试因为什么都没跑而通过」）
assert.ok(
  trace.some(([n]) => n === 'app.disableHardwareAcceleration'),
  '顶层应调用 app.disableHardwareAcceleration',
)
assert.ok(
  trace.some(([n]) => n === 'app.commandLine.appendSwitch'),
  '顶层应追加命令行开关',
)

// 3. preload 发出的每个通道，主进程都能接（静态契约；
//    ipcMain.handle 在 whenReady 里注册，桩不会跑到那一步，所以这里直接
//    从源码里抽 handler，与 preload 的 invoke 列表做交叉核对）。
const mainSrc = fs.readFileSync(path.join(__dirname, 'main.cjs'), 'utf8')
const preSrc = fs.readFileSync(path.join(__dirname, 'preload.cjs'), 'utf8')
const declared = new Set([...mainSrc.matchAll(/ipcMain\.handle\(\s*'([^']+)'/g)].map((m) => m[1]))
const invoked = new Set([...preSrc.matchAll(/ipcRenderer\.invoke\(\s*'([^']+)'/g)].map((m) => m[1]))
const missing = [...invoked].filter((c) => !declared.has(c))
assert.deepStrictEqual(
  missing, [],
  `preload 调用了主进程没有的通道（界面按钮会点不动）：${missing.join(', ')}`,
)
assert.ok(declared.size >= 10, `主进程 handler 数偏少（${declared.size}），疑似注册段被改坏`)

// 4. 界面会点到的每个 Rust 子命令，都必须在白名单里。
//    2026-09-17 的实例：LogsPanel.jsx 的 tail-logs、Wizard.jsx 的
//    ensure-services 不在白名单，两个按钮点了直接报「不允许的命令」。
const allowMatch = mainSrc.match(/const ALLOWED_CLI_COMMANDS = new Set\(\[([\s\S]*?)\]\)/)
assert.ok(allowMatch, '没能从 main.cjs 里解析出 ALLOWED_CLI_COMMANDS')
const allowed = new Set([...allowMatch[1].matchAll(/'([^']+)'/g)].map((m) => m[1]))

const srcDir = path.join(__dirname, '..', 'src')
const usedCmds = new Set()
for (const name of fs.readdirSync(srcDir)) {
  if (!name.endsWith('.jsx') && !name.endsWith('.js')) continue
  const text = fs.readFileSync(path.join(srcDir, name), 'utf8')
  for (const m of text.matchAll(/cli\(\s*['"]([^'"]+)['"]/g)) usedCmds.add(m[1])
}
const blocked = [...usedCmds].filter((c) => !allowed.has(c))
assert.deepStrictEqual(
  blocked, [],
  `界面会调用但被白名单拒绝的命令（点了必报错）：${blocked.join(', ')}`,
)

// 5. 「运行日志」必须默认打开真正在写入的 Rust 核心日志。
//    launchd/开发态都把 zunzhe-core.log 作为主日志；旧 Python 服务日志
//    只保留为可切换历史。默认值若退回 live_monitor，用户会看到空日志，
//    从而误判“服务没跑”。
const logsPanelSrc = fs.readFileSync(path.join(srcDir, 'LogsPanel.jsx'), 'utf8')
assert.match(
  logsPanelSrc,
  /value:\s*'zunzhe-core'/,
  '运行日志必须提供 zunzhe-core 选项',
)
assert.match(
  logsPanelSrc,
  /useState\('zunzhe-core'\)/,
  '运行日志必须默认打开 zunzhe-core，而不是旧服务名',
)

// 6. 视频详情里的「转写这个视频」必须连接真实 process 队列。
//    只出现按钮文字、只切配置开关、或只弹成功提示都不算；调用参数里
//    必须带当前文件的真实路径，并把 task_id/error 反馈给用户。
const appSrc = fs.readFileSync(path.join(srcDir, 'App.jsx'), 'utf8')
assert.match(
  appSrc,
  /cli\('process',\s*p,/,
  '转写按钮必须把真实文件路径交给 process 队列',
)
assert.match(
  appSrc,
  /processEnqueueFeedback\(r\)/,
  '转写按钮必须通过真实反馈函数展示核心返回的 task_id/错误，不能只做占位提示',
)
assert.match(
  appSrc,
  /notes-transcribe-panel/,
  '复盘笔记页必须有可直接转写的视频面板，不能把入口藏在多层详情弹窗里',
)
assert.match(
  appSrc,
  /queueTranscription\(\{\s*path:\s*item\.path/,
  '复盘页转写按钮必须把视频的真实路径交给 queueTranscription',
)

// 7. 存放位置选择不能全部退回产物根：日志/下载/复盘/数据库各有自己的有效位置；
//    数据库项还必须走文件选择器，否则会把目录写进“数据库文件”。
assert.match(
  appSrc,
  /stashFieldDefault\(field,\s*emailForm\.getFieldValue\(field\),\s*status\?\.paths\)/,
  '路径选择器必须按字段当前值/有效值给默认目录',
)
assert.match(
  appSrc,
  /chooseFile\(\{[\s\S]{0,260}数据库文件/,
  '数据库文件必须使用文件选择器，不能复用目录选择器',
)
assert.match(
  appSrc,
  /数据库路径将在后台服务重启后生效/,
  '修改数据库路径后必须明确提示重启后生效',
)
assert.match(
  appSrc,
  /downloadQueueFeedback\(r,\s*\{\s*name,\s*paths\s*\}\)/,
  '批量下载反馈必须使用当前真实下载目录，不能写死 downloads/短视频/',
)

// 8. 外部产物白名单必须来自真实存在的 CLI 子命令。历史上这里调用了
//    `zunzhe-core paths`，但 Rust 没有 paths 子命令，导致错误被 catch 后
//    静默退回项目目录，用户点“打开产物目录”没有任何反应。源码层必须同时
//    锁住调用命令和字段读取，Rust 侧由 cli.rs 的解析测试保证命令存在。
assert.match(
  mainSrc,
  /runCli\(\[['"]paths['"]\]\)/,
  'refreshAllowedRoots 必须调用真实存在的 paths CLI 子命令',
)
for (const key of ['media_root', 'downloads', 'fupan_output', 'recordings', 'covers', 'run', 'database']) {
  assert.ok(
    mainSrc.includes(`'${key}'`),
    `refreshAllowedRoots 必须读取 paths 返回的 ${key}`,
  )
}
assert.match(
  mainSrc,
  /path\.dirname\(path\.resolve\(db\)\)/,
  'database 是文件路径，白名单还必须包含它的父目录，否则数据库同级文件打不开',
)

console.log('main 主进程 smoke test ok')
