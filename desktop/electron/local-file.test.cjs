const assert = require('assert')
const path = require('path')
const { DEFAULT_SCHEME, localFileUrl, parseLocalFileUrl } = require('./local-file-url.cjs')

const abs = path.join('/', 'tmp', 'zunzhe-project', 'fupan_output', 'covers', 'BV1.jpg')

// 编码：绝对路径进 URL。含空格/中文/井号的路径必须能原样还原。
assert.strictEqual(
  localFileUrl(abs),
  `${DEFAULT_SCHEME}://local/${encodeURIComponent(abs)}`,
  '绝对路径应编码成协议 URL',
)
assert.strictEqual(localFileUrl(''), '', '空路径返回空')
assert.strictEqual(localFileUrl(null), '', 'null 返回空')
assert.strictEqual(localFileUrl(123), '', '非字符串返回空')

// 解码：往返一致。
assert.strictEqual(parseLocalFileUrl(localFileUrl(abs)), abs, '编解码应往返一致')

const nasty = path.join('/', 'tmp', '产物 根', '封面#1.jpg')
assert.strictEqual(parseLocalFileUrl(localFileUrl(nasty)), nasty, '含空格/中文/井号的路径应往返一致')

// 解码：非法输入一律 null，不做「看起来像」的兜底。
assert.strictEqual(parseLocalFileUrl('http://example.com/a.jpg'), null, '别的 scheme 应拒绝')
assert.strictEqual(parseLocalFileUrl(`${DEFAULT_SCHEME}://evil/tmp/a.jpg`), null, 'host 不是 local 应拒绝')
assert.strictEqual(parseLocalFileUrl(`${DEFAULT_SCHEME}://local/`), null, '空路径应拒绝')
assert.strictEqual(parseLocalFileUrl(`${DEFAULT_SCHEME}://local/relative%2Fa.jpg`), null, '相对路径应拒绝')
assert.strictEqual(parseLocalFileUrl(`${DEFAULT_SCHEME}://local/%E0%A4%A`), null, '解码失败应拒绝')
assert.strictEqual(parseLocalFileUrl(''), null, '空串应拒绝')
assert.strictEqual(parseLocalFileUrl(undefined), null, 'undefined 应拒绝')

// 自定义 scheme 也要能工作（打包/开发可能用不同 scheme）。
assert.strictEqual(parseLocalFileUrl(localFileUrl(abs, 'zf'), 'zf'), abs, '自定义 scheme 应往返一致')
assert.strictEqual(parseLocalFileUrl(localFileUrl(abs, 'zf'), DEFAULT_SCHEME), null, 'scheme 不匹配应拒绝')

console.log('local-file test ok')
