// Node 单测：验证本地通知跨重启去重，不启动 Electron。
const assert = require('assert')
const fs = require('fs')
const os = require('os')
const path = require('path')
const { consumeNotifyQueue } = require('./local-notify.cjs')

const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'zunzhe-local-notify-'))
const queueFile = path.join(dir, 'pending_notify.json')
const seenFile = path.join(dir, 'seen.json')
const shown = []

fs.writeFileSync(
  queueFile,
  JSON.stringify([
    { id: '1', title: '第一条', body: '内容一' },
    { id: '2', title: '第二条', body: '内容二' },
  ]),
  'utf8',
)

// 首次消费：两条都弹，并写入 seen 文件。
const first = consumeNotifyQueue(queueFile, seenFile, (t, b) => shown.push(t + b))
assert.strictEqual(first, 2, '首次应弹出两条通知')
assert.strictEqual(shown.length, 2)
const persisted = JSON.parse(fs.readFileSync(seenFile, 'utf8'))
assert.deepStrictEqual(persisted, ['1', '2'], '已读 id 应持久化')

// 模拟桌面重启后再次消费：同一队列不应重复弹。
shown.length = 0
const second = consumeNotifyQueue(queueFile, seenFile, (t, b) => shown.push(t + b))
assert.strictEqual(second, 0, '重启后不应重复弹出已读通知')
assert.strictEqual(shown.length, 0)

// 新追加一条：只弹新条目。
const items = JSON.parse(fs.readFileSync(queueFile, 'utf8'))
items.push({ id: '3', title: '第三条', body: '内容三' })
fs.writeFileSync(queueFile, JSON.stringify(items), 'utf8')
const third = consumeNotifyQueue(queueFile, seenFile, (t, b) => shown.push(t + b))
assert.strictEqual(third, 1, '新追加的通知应弹出')
assert.deepStrictEqual(shown, ['第三条内容三'])

// 模拟 Rust daemon 正在原子替换队列文件时读到的半截 JSON：本轮跳过，
// 不能把调用方异常抛出；恢复成完整 JSON 后下一轮仍能消费。
fs.writeFileSync(queueFile, '[{"id":"4","title":"半截', 'utf8')
let readError = null
const corrupt = consumeNotifyQueue(queueFile, seenFile, (t, b) => shown.push(t + b), (e) => {
  readError = e
})
assert.strictEqual(corrupt, 0, '半截 JSON 不应弹出通知')
assert.ok(readError, '应把读失败交给调用方诊断')
// 下一轮读取前，Rust daemon 已用原子替换恢复成合法数组；旧半截内容不复存在。
// 原子替换不会恢复“读取时那半截内容”，而是把整个文件替换为完整合法数组。
const recovered = [
  { id: '1', title: '第一条', body: '内容一' },
  { id: '2', title: '第二条', body: '内容二' },
  { id: '3', title: '第三条', body: '内容三' },
  { id: '4', title: '第四条', body: '内容四' },
]
fs.writeFileSync(queueFile, JSON.stringify(recovered), 'utf8')
const fourth = consumeNotifyQueue(queueFile, seenFile, (t, b) => shown.push(t + b))
assert.strictEqual(fourth, 1, '队列恢复后新通知应弹出')
assert.deepStrictEqual(shown, ['第三条内容三', '第四条内容四'])

fs.rmSync(dir, { recursive: true, force: true })
console.log('local-notify test ok')
