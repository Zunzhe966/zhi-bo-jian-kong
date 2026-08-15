const assert = require('assert')
const path = require('path')
const { projectLocalUrl } = require('./local-file-url.cjs')

const root = path.join('/', 'tmp', 'zunzhe-project')
const inside = path.join(root, 'fupan_output', 'covers', 'BV1.jpg')
const outside = path.join(root, '..', 'other', 'secret.txt')

assert.strictEqual(
  projectLocalUrl(root, inside),
  'local-file://local/fupan_output/covers/BV1.jpg',
  '项目内路径应转成协议 URL',
)
assert.strictEqual(projectLocalUrl(root, outside), '', '项目外路径不应暴露')
assert.strictEqual(projectLocalUrl(root, ''), '', '空路径返回空')
assert.strictEqual(projectLocalUrl(root, 123), '', '非字符串返回空')

console.log('local-file test ok')
