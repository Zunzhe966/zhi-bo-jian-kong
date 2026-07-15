const fs = require('fs')
const path = require('path')
const sharp = require('sharp')
const toIco = require('to-ico')

const assets = path.join(__dirname, '..', 'assets')
const src = path.join(assets, 'icon.png')

async function main() {
  if (!fs.existsSync(src)) {
    console.warn('[make-icons] 找不到 icon.png，跳过')
    return
  }
  const sizes = [16, 32, 48, 64, 128, 256]
  const pngs = []
  for (const s of sizes) {
    const buf = await sharp(src).resize(s, s, { fit: 'cover' }).png().toBuffer()
    pngs.push(buf)
    fs.writeFileSync(path.join(assets, `icon-${s}.png`), buf)
  }
  fs.writeFileSync(path.join(assets, 'icon.ico'), await toIco(pngs))
  console.log('[make-icons] 已生成 icon.ico 与 icon-16~256.png')
}

main().catch((e) => {
  console.error('[make-icons]', e)
  process.exit(1)
})
