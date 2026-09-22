#!/usr/bin/env node
/**
 * 界面渲染验收（发版前必跑）。
 *
 * 为什么要有这个脚本：
 *   2026-09-17 出过两次事故：
 *   1) 发布后用户打开是**黑屏**。原因是详情弹窗引用了一个没声明的变量，
 *      界面一渲染就抛 ReferenceError，整棵 React 树没挂载，只剩深色底。
 *      `vite build` 对此毫无反应（它只打包，不执行代码）。
 *   2) 黑屏修好之后，「打开存放位置 / 查看内容」点了没反应。
 *      原因是 App.jsx 渲染 LiveCard 时漏传了 onOpenStash / onDetail 两个属性，
 *      而「按钮存在」「能点」这些表面检查全都通得过 —— 必须点下去看**真结果**。
 *
 * 所以这个脚本只认「点了之后发生了什么」：
 *   - 点「查看内容」→ 必须弹出详情弹窗，且里面有地址、存放位置、文件清单；
 *   - 点「打开存放位置」→ 必须真的调到了 openPath，并且传的是个目录路径；
 *   - 点「主播设置」→ 必须看到「存到这里」和真实落盘路径。
 * 两个标签页（直播 / 视频）都要验，因为这两条渲染分支是分开写的。
 *
 * 两个环境坑（踩过，不要再踩）：
 *   A. 探针状态**不能**写在 window 上再从 page.evaluate 读 —— 这个环境里读不到
 *      （执行上下文隔离），会静默拿到 null，把「有调用」误判成「没调用」。
 *      改用 DOM 属性当信道：写 document.documentElement.dataset，evaluate 能读。
 *   B. 不要用 `element.click()` 派发合成点击 —— 在这个环境下它对部分 antd 按钮
 *      不触发 React 的处理函数，会误报「点了没反应」。用 page.mouse.click 走真实坐标。
 *
 * 用法：
 *   node scripts/check-ui-render.cjs                    # 检查 desktop/dist
 *   node scripts/check-ui-render.cjs <dist目录> <截图输出目录>
 *
 * 依赖 patchright（本机在 rsshub/node_modules 下）。
 */
const fs = require('fs')
const path = require('path')
const http = require('http')
const os = require('os')

const ROOT = path.resolve(__dirname, '..')
const DIST = process.argv[2] || path.join(ROOT, 'desktop', 'dist')
const OUT = process.argv[3] || path.join(os.tmpdir(), 'zunzhe-ui-check')
const PORT = Number(process.env.PROBE_PORT || 18930)

// 假数据：形状必须和真实 status 一致，否则界面会走另一条分支，验不出真问题。
const FAKE_STATUS = {
  rust_daemon: true,
  services: { live_monitor: { running: true }, video_monitor: { running: true } },
  source_health: { ok: true },
  notify_health: { ok: true },
  live_watchlist: [
    { url: 'https://live.bilibili.com/24931109', remark: '主播: 侯翠翠', enabled: true, quality: '原画', notify: '微信' },
    { url: 'https://live.douyin.com/Glx6768', remark: '主播: 关立新', enabled: true, quality: '蓝光', notify: '跟随全局' },
    { url: 'https://live.kuaishou.com/u/Bd20210915', remark: '主播: 快手测试', enabled: false, quality: '高清', notify: '不通知' },
  ],
  video_watchlist: [
    { url: 'https://space.bilibili.com/1458143131', name: '影视飓风', download: true, notify: '微信', platform: 'Bilibili' },
  ],
  notes: [
    { anchor: '关立新', session: '[B站] “向死而生”的秘密——直播连麦', notes_file: '/x/fupan/关立新/场次1/笔记.复盘笔记.md', transcript_file: '/x/fupan/关立新/场次1/转写.txt', points_file: '/x/fupan/关立新/场次1/知识点.json', clips: ['/x/fupan/关立新/场次1/精华切片/c1.mp4'] },
    { anchor: '关立新', session: '[B站] 人生的选择_来自于你的底气', transcript_file: '/x/fupan/关立新/场次2/转写.txt' },
    { anchor: '侯翠翠', session: '直播 2026-09-16', transcript_file: '/x/fupan/侯翠翠/直播/转写.txt' },
  ],
  recent_recordings: [],
  recording_sessions: [],
  paths: {
    media_root: '/Volumes/A区/尊者产物',
    downloads: '/Volumes/A区/尊者产物/downloads',
    fupan_output: '/Volumes/A区/尊者产物/fupan_output',
    project_dir: ROOT,
  },
}

const FAKE_FILES = {
  ok: true,
  items: [
    { name: '侯翠翠_直播_2026-09-17_20-10-03.mp4', dir: false, size: 528734208, mtime: Date.now() - 60000, path: '/x/a.mp4' },
    { name: '侯翠翠_直播_2026-09-16_20-01-11.mp4', dir: false, size: 934281216, mtime: Date.now() - 86400000, path: '/x/b.mp4' },
    { name: '转写文本', dir: true, size: 0, mtime: Date.now() - 3600000, path: '/x/c' },
  ],
}

const MIME = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.css': 'text/css; charset=utf-8', '.png': 'image/png', '.svg': 'image/svg+xml' }

function requirePatchright() {
  const tried = []
  for (const p of ['patchright', path.join(ROOT, 'rsshub/node_modules/patchright')]) {
    try { return require(p) } catch (e) { tried.push(`${p}: ${e.message}`) }
  }
  throw new Error('找不到 patchright，试过：\n' + tried.join('\n'))
}

/** 把 dist 复制一份，在 index.html 里插桩：假 window.desktop + 调用记录（写进 DOM 属性）。 */
function stage(distDir, outDir) {
  fs.rmSync(outDir, { recursive: true, force: true })
  fs.mkdirSync(outDir, { recursive: true })
  fs.cpSync(distDir, outDir, { recursive: true })

  const htmlPath = path.join(outDir, 'index.html')
  let html = fs.readFileSync(htmlPath, 'utf8')
  const head = `<script>
(function () {
  var STATUS = ${JSON.stringify(FAKE_STATUS)};
  var FILES = ${JSON.stringify(FAKE_FILES)};
  // 调用记录写进 DOM 属性：window 上的东西在这个环境里 evaluate 读不到，DOM 能读到。
  var root = document.documentElement;
  var calls = [];
  function record(name, arg) {
    calls.push(name + (arg === undefined ? '' : ':' + String(arg).slice(0, 120)));
    root.setAttribute('data-probe-calls', calls.join(' || '));
  }
  var errors = [];
  function recordErr(msg) {
    errors.push(String(msg).slice(0, 200));
    root.setAttribute('data-probe-errors', errors.join(' || '));
  }
  window.addEventListener('error', function (e) { recordErr('ERR: ' + e.message) }, true);
  window.addEventListener('unhandledrejection', function (e) {
    recordErr('REJECT: ' + (e.reason && e.reason.message ? e.reason.message : String(e.reason)));
  });
  var rec = function (name) {
    return function (arg) { record(name, arg); return Promise.resolve({ ok: true }); };
  };
  var noop = function () { return Promise.resolve({}); };
  window.desktop = {
    cli: function (cmd) {
      record('cli', cmd);
      if (cmd === 'status') return Promise.resolve(STATUS);
      if (cmd === 'get-config') return Promise.resolve({});
      if (cmd === 'email-status') return Promise.resolve({ configured: false, hint: '未配置' });
      if (cmd === 'get-app-prefs') return Promise.resolve({ prefs: {} });
      return Promise.resolve({});
    },
    reloadUi: noop, setLoginItem: noop, getLoginItem: function () { return Promise.resolve(false); },
    showLocalNotify: noop,
    openPath: rec('openPath'), showItem: rec('showItem'), openExternal: rec('openExternal'),
    readText: function () { return Promise.resolve(''); }, coverUrl: function () { return Promise.resolve(''); },
    getPaths: function () { return Promise.resolve({}); }, chooseDataDir: noop,
    chooseDir: function () { record('chooseDir'); return Promise.resolve({ ok: true, path: '/Volumes/A区/新复盘目录' }); },
    checkUpdate: noop, biliLogin: noop, douyinLogin: noop,
    listDir: function (dir) { record('listDir', dir); return Promise.resolve(FILES); },
  };
})();
</script>
`
  html = html.replace('<title>', head + '<title>')
  fs.writeFileSync(htmlPath, html, 'utf8')
  return outDir
}

function serve(dir, port) {
  const server = http.createServer((req, res) => {
    const rel = decodeURIComponent(req.url.split('?')[0]).replace(/^\/+/, '') || 'index.html'
    const file = path.join(dir, rel)
    if (!file.startsWith(dir) || !fs.existsSync(file) || fs.statSync(file).isDirectory()) {
      res.writeHead(404); res.end('not found'); return
    }
    res.writeHead(200, { 'Content-Type': MIME[path.extname(file)] || 'application/octet-stream' })
    fs.createReadStream(file).pipe(res)
  })
  return new Promise((r) => server.listen(port, '127.0.0.1', () => r(server)))
}

/** 读探针记录（走 DOM 属性这条可靠信道）。 */
const readProbe = (page) => page.evaluate(() => ({
  calls: document.documentElement.getAttribute('data-probe-calls') || '',
  errors: document.documentElement.getAttribute('data-probe-errors') || '',
}))

/**
 * 按可见文字找一个 <button>，滚到视口里，然后用**真实鼠标**点它的中心。
 * 不用 element.click()：这个环境下它对部分 antd 按钮不触发 React 处理函数。
 * 返回是否点到了。
 */
async function clickButton(page, match, { index = 0 } = {}) {
  const rect = await page.evaluate(({ m, i }) => {
    const isMatch = (b) => {
      const t = (b.innerText || '').trim()
      return m.exact ? t === m.text : t.includes(m.text)
    }
    const btns = [...document.querySelectorAll('button')].filter(isMatch)
    const b = btns[i]
    if (!b) return null
    b.scrollIntoView({ block: 'center' })
    const r = b.getBoundingClientRect()
    return { x: r.x + r.width / 2, y: r.y + r.height / 2, w: r.width, h: r.height, text: (b.innerText || '').trim() }
  }, { m: match, i: index })
  if (!rect || rect.w === 0 || rect.h === 0) return null
  await page.mouse.click(rect.x, rect.y)
  return rect
}

/** 切主标签（导航是原生 button，文字里带角标数字，所以用 includes 匹配）。 */
async function switchTab(page, text) {
  const r = await clickButton(page, { text, exact: false })
  await page.waitForTimeout(700)
  return !!r
}

/** 用真实鼠标点一个 CSS 选择器命中的元素（先滚进视口）。 */
async function clickSelector(page, selector, { index = 0, text = null } = {}) {
  const rect = await page.evaluate(({ sel, i, t }) => {
    let list = [...document.querySelectorAll(sel)]
    if (t) list = list.filter((e) => (e.innerText || '').includes(t))
    const el = list[i]
    if (!el) return null
    el.scrollIntoView({ block: 'center' })
    const r = el.getBoundingClientRect()
    return { x: r.x + r.width / 2, y: r.y + r.height / 2, w: r.width, h: r.height }
  }, { sel: selector, i: index, t: text })
  if (!rect || !rect.w || !rect.h) return null
  await page.mouse.click(rect.x, rect.y)
  return rect
}

const modalText = (page) => page.evaluate(() => {
  const m = document.querySelector('.ant-modal-content')
  return m ? m.innerText : ''
})

;(async () => {
  if (!fs.existsSync(path.join(DIST, 'index.html'))) {
    console.error(`[验收] 找不到 ${DIST}/index.html，先构建前端`)
    process.exit(1)
  }
  const shotDir = path.join(OUT, 'shots')
  fs.mkdirSync(shotDir, { recursive: true })
  const stageDir = stage(DIST, path.join(OUT, 'site'))
  const server = await serve(stageDir, PORT)

  const { chromium } = requirePatchright()
  const browser = await chromium.launch({ channel: 'chrome', headless: true })
  const page = await browser.newPage({ viewport: { width: 1440, height: 1000 } })
  const errors = []
  page.on('pageerror', (e) => errors.push('pageerror: ' + e.message))
  page.on('console', (m) => { if (m.type() === 'error') errors.push('console: ' + m.text().slice(0, 200)) })

  await page.goto(`http://127.0.0.1:${PORT}/index.html`, { waitUntil: 'load' })
  await page.waitForTimeout(3500)

  const report = []
  const base = await page.evaluate(() => {
    const r = document.getElementById('root')
    return { len: r ? r.innerHTML.length : -1, text: (document.body.innerText || '').slice(0, 120) }
  })
  await page.screenshot({ path: path.join(shotDir, '1-首页-直播.png') })
  report.push(`首页：根节点 ${base.len} 字符`)
  if (base.len <= 0) report.push('  ✗ 根节点是空的 —— 界面没有渲染出来')

  // 卡片上必须标出是哪个网站（用户明确要求）
  const bodyText = await page.evaluate(() => document.body.innerText)
  for (const must of ['抖音', '快手', 'B站']) {
    if (!bodyText.includes(must)) report.push(`  ✗ 卡片上没有平台标签「${must}」`)
  }

  // 界面底部必须写出**真实版本号**，且和 package.json 一致。
  // 2026-09-17 吃过亏：换上了新版，但用户看到的还是旧界面，双方都没法自证。
  let pkgVersion = ''
  try { pkgVersion = JSON.parse(fs.readFileSync(path.join(ROOT, 'desktop', 'package.json'), 'utf8')).version } catch (e) { /* 忽略 */ }
  if (pkgVersion) {
    if (!bodyText.includes(pkgVersion)) report.push(`  ✗ 界面没显示版本号 ${pkgVersion}（底部应写「v${pkgVersion}」）`)
  }

  // ── 直播标签页
  // ① 打开存放位置 → 必须真的调到 openPath，且传的是绝对路径
  const liveStash = await clickButton(page, { text: '打开存放位置' })
  await page.waitForTimeout(1200)
  {
    const p = await readProbe(page)
    const hit = /openPath:[^|]*\//.test(p.calls)
    report.push(`直播卡「打开存放位置」：${hit ? '已调到 openPath' : '✗ 没有调到 openPath（按钮是死的）'}`)
    if (!hit && p.calls) report.push(`    实际调用：${p.calls}`)
  }

  // ② 查看内容 → 必须弹详情，且地址/存放位置/文件清单齐全
  const liveDetail = await clickButton(page, { text: '查看内容' })
  await page.waitForTimeout(1500)
  await page.screenshot({ path: path.join(shotDir, '2-直播详情.png') })
  if (!liveDetail) report.push('  ✗ 直播卡上没有「查看内容」按钮')
  else {
    const t = await modalText(page)
    if (!t) report.push('  ✗ 直播卡点「查看内容」没弹窗（按钮是死的）')
    else {
      for (const must of ['监控地址', '存放位置', '文字稿与复盘', '里面的文件']) {
        if (!t.includes(must)) report.push(`  ✗ 详情弹窗里缺「${must}」`)
      }
      if (!/GB|MB|KB/.test(t)) report.push('  ✗ 详情弹窗里没列出文件（大小没显示）')
      if (!t.includes('live.bilibili.com/24931109')) report.push('  ✗ 详情弹窗里没显示监控地址')
    }
    await page.keyboard.press('Escape')
    await page.waitForTimeout(800)
  }

  // ③ 主播设置 → 「存到这里」必须写真实路径
  if (await clickButton(page, { text: '主播设置' })) {
    await page.waitForTimeout(1200)
    await page.screenshot({ path: path.join(shotDir, '3-主播设置.png') })
    const t = await modalText(page)
    if (!t.includes('存到这里')) report.push('  ✗ 主播设置里没有「存到这里」这一块')
    if (!t.includes('/Volumes/A区/尊者产物/downloads')) report.push('  ✗ 主播设置里没显示真实存放路径')
    await page.keyboard.press('Escape')
    await page.waitForTimeout(700)
  } else {
    report.push('  ✗ 直播卡上没有「主播设置」按钮')
  }

  // ── 视频标签页（另一条渲染分支，必须单独验）
  if (!(await switchTab(page, '视频监控'))) report.push('  ✗ 切不到「视频监控」标签')
  else {
    await page.screenshot({ path: path.join(shotDir, '4-视频页.png') })
    const before = (await readProbe(page)).calls
    if (await clickButton(page, { text: '打开存放位置' })) {
      await page.waitForTimeout(1200)
      const after = (await readProbe(page)).calls
      if (after === before) report.push('  ✗ 视频卡「打开存放位置」没反应（按钮是死的）')
    } else report.push('  ✗ 视频卡上没有「打开存放位置」按钮')

    if (await clickButton(page, { text: '查看内容' })) {
      await page.waitForTimeout(1500)
      await page.screenshot({ path: path.join(shotDir, '5-视频详情.png') })
      const t = await modalText(page)
      if (!t) report.push('  ✗ 视频卡点「查看内容」没弹窗（按钮是死的）')
      else if (!t.includes('博主内容')) report.push(`  ✗ 视频详情弹窗标题不对：${t.slice(0, 40)}`)
      await page.keyboard.press('Escape')
      await page.waitForTimeout(700)
    } else report.push('  ✗ 视频卡上没有「查看内容」按钮')
  }

  // ── 复盘笔记页（用户要求：按人分组、点人出全部文章、点文件直接打开、目录可换）
  if (!(await switchTab(page, '复盘笔记'))) report.push('  ✗ 切不到「复盘笔记」标签')
  else {
    await page.screenshot({ path: path.join(shotDir, '6-笔记页.png') })
    const nt = await page.evaluate(() => document.body.innerText)
    if (!nt.includes('关立新') || !nt.includes('侯翠翠')) report.push('  ✗ 笔记页没有按「人/内容」分组（关立新/侯翠翠 没出现）')
    if (!nt.includes('复盘目录')) report.push('  ✗ 笔记页顶部没有「复盘目录」一栏')
    if (!nt.includes('/Volumes/A区/尊者产物/fupan_output')) report.push('  ✗ 笔记页没显示真实复盘目录路径')

    // 展开第一个分组 → 场次和文件必须出来
    if (!(await clickSelector(page, '.notes-group-head', { index: 0 }))) report.push('  ✗ 笔记页没有可展开的分组')
    else {
      await page.waitForTimeout(800)
      const expanded = await page.evaluate(() => document.body.innerText)
      if (!expanded.includes('转写.txt')) report.push('  ✗ 展开分组后看不到转写文件')
      await page.screenshot({ path: path.join(shotDir, '7-笔记展开.png') })

      // 点文件名 → 必须真调到 openPath，入参就是那个文件的路径
      const b1 = (await readProbe(page)).calls
      if (await clickSelector(page, '.file-name', { index: 0 })) {
        await page.waitForTimeout(800)
        const a1 = (await readProbe(page)).calls
        if (a1 === b1) report.push('  ✗ 笔记页点文件名没反应（按钮是死的）')
        else if (!/openPath:\/x\/fupan\//.test(a1)) report.push('  ✗ 点文件名调的 openPath 入参不是那个文件：' + a1.split('||').pop())
      } else report.push('  ✗ 笔记页展开后没有可点的文件名')

      // 点「打开文件夹」→ 必须直接打开那个人自己的文件夹
      const b2 = (await readProbe(page)).calls
      if (await clickButton(page, { text: '打开文件夹' })) {
        await page.waitForTimeout(800)
        const a2 = (await readProbe(page)).calls
        if (a2 === b2) report.push('  ✗ 笔记页「打开文件夹」没反应')
        else if (!a2.includes('openPath:/Volumes/A区/尊者产物/fupan_output/关立新')) {
          report.push('  ✗ 「打开文件夹」开的不是那个人的文件夹：' + a2.split('||').pop())
        }
      } else report.push('  ✗ 笔记页没有「打开文件夹」按钮')

      // 点「换个目录」→ 必须真去选目录并写回配置
      const b3 = (await readProbe(page)).calls
      if (await clickButton(page, { text: '换个目录' })) {
        await page.waitForTimeout(1200)
        const a3 = (await readProbe(page)).calls
        if (!a3.includes('chooseDir')) report.push('  ✗ 「换个目录」没有唤起目录选择')
        if (!a3.includes('cli:set-config')) report.push('  ✗ 选完目录没有写回配置（set-config 没被调到）')
      } else report.push('  ✗ 笔记页没有「换个目录」按钮')
    }
  }

  const probe = await readProbe(page)
  await browser.close()
  server.close()

  const ok = errors.length === 0 && base.len > 0
    && !report.some((l) => l.includes('✗')) && !probe.errors
  console.log('===== 界面验收 =====')
  for (const e of errors) console.log('  ✗ ' + e)
  for (const l of report) console.log(l.startsWith('  ') ? l : '  ' + l)
  if (probe.errors) console.log('  ✗ 页面内报错：' + probe.errors)
  console.log('  探针调用记录：' + (probe.calls || '(无)'))
  console.log(`截图：${shotDir}`)
  console.log(ok ? '\n✅ 界面渲染正常' : '\n❌ 界面有问题，不要发布')
  process.exit(ok ? 0 : 1)
})().catch((e) => { console.error('验收脚本自身出错:', e.message); process.exit(1) })
