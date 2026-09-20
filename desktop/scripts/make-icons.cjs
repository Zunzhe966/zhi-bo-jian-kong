const fs = require('fs')
const os = require('os')
const path = require('path')
const { execFileSync } = require('child_process')
const sharp = require('sharp')
const toIco = require('to-ico')

// 图标唯一源：assets/icon.png（1024×1024，透明底 PNG）。
// 所有派生产物都由它生成，不要手工维护：
//   - icon-16~512.png   界面/README 用
//   - icon.ico          Windows 打包用（不需要 macOS 工具链）
//   - icon.icns         macOS 打包用（需要 macOS 的 iconutil；非 macOS 自动跳过）
//
// 历史坑（2026-09-20 修复）：旧版脚本只生成 icon.ico 与 icon-16~256.png，
// 从不生成 icon.icns。而 package.json 的 mac.icon 指向 assets/icon.icns，
// electron-builder 会把 icns 缓存到 release/.icon-icns 并按内容哈希判断是否重建
// —— 结果换了 icon.png 之后 .app 里仍可能带旧图标。现在每次 build 都从
// icon.png 重新生成全部尺寸（含 1024），icns 内容必变，缓存自然失效。

const assets = path.join(__dirname, '..', 'assets')
const src = path.join(assets, 'icon.png')

// iconset 需要的文件名 -> 像素尺寸（含 @2x，缺 512/1024 会让 Dock 放大时发虚）
const ICONSET = [
  ['icon_16x16.png', 16],
  ['icon_16x16@2x.png', 32],
  ['icon_32x32.png', 32],
  ['icon_32x32@2x.png', 64],
  ['icon_128x128.png', 128],
  ['icon_128x128@2x.png', 256],
  ['icon_256x256.png', 256],
  ['icon_256x256@2x.png', 512],
  ['icon_512x512.png', 512],
  ['icon_512x512@2x.png', 1024],
]

async function render(size) {
  return sharp(src)
    .resize(size, size, { fit: 'cover' })
    .png()
    .toBuffer()
}

async function makePngsAndIco() {
  const sizes = [16, 32, 48, 64, 128, 256, 512, 1024]
  const icoPngs = []
  for (const s of sizes) {
    const buf = await render(s)
    fs.writeFileSync(path.join(assets, `icon-${s}.png`), buf)
    // ico 只装到 256，再大 Windows 也不认，白增体积
    if (s <= 256) icoPngs.push(buf)
  }
  fs.writeFileSync(path.join(assets, 'icon.ico'), await toIco(icoPngs))
  console.log('[make-icons] 已生成 icon.ico 与 icon-16~1024.png')
}

async function makeIcns() {
  if (process.platform !== 'darwin') {
    console.log('[make-icons] 非 macOS，跳过 icon.icns（Windows 打包只用 icon.ico）')
    return
  }
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'zunzhe-iconset-'))
  const iconset = path.join(tmp, 'icon.iconset')
  fs.mkdirSync(iconset)
  try {
    for (const [name, size] of ICONSET) {
      fs.writeFileSync(path.join(iconset, name), await render(size))
    }
    // 先删旧 icns：iconutil 在目标已存在时会报错，静默留下旧图标最难查。
    const out = path.join(assets, 'icon.icns')
    fs.rmSync(out, { force: true })
    execFileSync('iconutil', ['-c', 'icns', '-o', out, iconset], { stdio: 'inherit' })
    console.log('[make-icons] 已生成 icon.icns（含 512/1024 高清层）')
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true })
  }
}

async function main() {
  if (!fs.existsSync(src)) {
    console.warn('[make-icons] 找不到 icon.png，跳过')
    return
  }
  await makePngsAndIco()
  await makeIcns()
}

main().catch((e) => {
  console.error('[make-icons]', e)
  process.exit(1)
})
