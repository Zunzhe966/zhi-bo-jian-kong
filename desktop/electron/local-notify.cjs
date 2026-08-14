// 本地通知去重：桌面端消费 pending_notify.json 时，把已弹出的 id 持久化到
// 单独的 seen 文件。这样桌面进程重启后不会重复弹同一条通知，Rust 侧队列
// 文件仍保留“最近 30 条”的追加语义。
const fs = require('fs')
const path = require('path')

function loadSeenNotifyIds(file) {
  try {
    const ids = JSON.parse(fs.readFileSync(file, 'utf8'))
    if (Array.isArray(ids)) return new Set(ids.map(String))
  } catch (e) {
    // 首次运行或文件损坏时视为空，不影响主流程。
  }
  return new Set()
}

function persistSeenNotifyIds(ids, file) {
  let tmp = ''
  try {
    // 先写同目录临时文件再 rename，避免桌面进程在写一半时退出导致 seen 文件
    // 变成半截 JSON。损坏的 seen 文件会让已弹过的通知重新弹一遍。
    tmp = path.join(
      path.dirname(file),
      `.${path.basename(file)}.${process.pid}.${Date.now()}.tmp`,
    )
    fs.writeFileSync(tmp, JSON.stringify([...ids].slice(-1000)), 'utf8')
    fs.renameSync(tmp, file)
  } catch (e) {
    try { if (tmp) fs.unlinkSync(tmp) } catch { /* 清理失败不影响主流程 */ }
    // 写失败只影响去重，不阻塞通知主流程。
  }
}

/**
 * 消费本地通知队列：只弹未读条目，并把已读 id 持久化到 seenFile。
 * 返回本次实际弹出条数。
 */
function consumeNotifyQueue(queueFile, seenFile, showFn, onReadError) {
  if (!fs.existsSync(queueFile)) return 0
  let items
  try {
    items = JSON.parse(fs.readFileSync(queueFile, 'utf8'))
  } catch (e) {
    if (onReadError) onReadError(e)
    return 0
  }
  if (!Array.isArray(items)) {
    if (onReadError) onReadError(new Error('队列不是 JSON 数组，本轮跳过并保留现场'))
    return 0
  }
  if (!items.length) return 0
  const seen = loadSeenNotifyIds(seenFile)
  let shown = 0
  for (const item of items) {
    const id = String(item.id || '')
    if (!id || seen.has(id)) continue
    seen.add(id)
    if (showFn) showFn(item.title || '', item.body || '')
    shown += 1
  }
  persistSeenNotifyIds(seen, seenFile)
  return shown
}

module.exports = {
  loadSeenNotifyIds,
  persistSeenNotifyIds,
  consumeNotifyQueue,
}
