// Node 单测：验证 Netscape Cookie 解析与注入参数读取，不启动 Electron。
const assert = require('assert')
const fs = require('fs')
const os = require('os')
const path = require('path')
const { parseNetscape } = require('./bilibili-cookie-loader.cjs')

const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'zunzhe-cookie-loader-'))
const file = path.join(dir, 'cookies.txt')
fs.writeFileSync(
  file,
  [
    '# Netscape HTTP Cookie File',
    '.bilibili.com\tTRUE\t/\tFALSE\t1786886977\tSESSDATA\tabc123',
    '.bilibili.com\tTRUE\t/\tFALSE\t1786886977\tbuvid3\txyz',
    'bad line',
    '',
  ].join('\n') + '\n',
  'utf8',
)

const cookies = parseNetscape(file)
assert.strictEqual(cookies.length, 2, '应解析出两条有效 Cookie')
assert.strictEqual(cookies[0].name, 'SESSDATA')
assert.strictEqual(cookies[0].value, 'abc123')
assert.strictEqual(cookies[0].url, 'https://www.bilibili.com/')
assert.strictEqual(cookies[1].name, 'buvid3')

fs.rmSync(dir, { recursive: true, force: true })
console.log('bilibili-cookie-loader test ok')
