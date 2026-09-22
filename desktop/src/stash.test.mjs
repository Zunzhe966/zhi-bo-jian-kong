import assert from 'node:assert/strict'
import {
  isTranscribableMedia, pendingTranscriptionItems, processEnqueueFeedback,
  processTaskAction, processTaskForFile, processTaskLabel, stashFieldDefault,
  itemStashDir, itemStashFallback, downloadQueueFeedback,
} from './stash.js'

const paths = {
  media_root: '/data/尊者产物',
  downloads: '/data/尊者产物/downloads',
  fupan_output: '/data/尊者产物/fupan_output',
  logs: '/data/尊者产物/logs',
  database: '/data/尊者产物/database/zunzhe-core.db',
}
assert.equal(stashFieldDefault('mediaRoot', '', paths), '/data/尊者产物')
assert.equal(stashFieldDefault('downloadDir', '', paths), '/data/尊者产物')
assert.equal(
  stashFieldDefault('downloadDir', '', { ...paths, video_root: '/mnt/短视频' }),
  '/mnt/短视频',
)
assert.equal(stashFieldDefault('fupanDir', '', paths), '/data/尊者产物/fupan_output')
assert.equal(stashFieldDefault('logsDir', '', paths), '/data/尊者产物/logs')
assert.equal(stashFieldDefault('databaseFile', '', paths), '/data/尊者产物/database/zunzhe-core.db')
assert.equal(stashFieldDefault('downloadDir', '/mnt/other', paths), '/mnt/other')
assert.equal(stashFieldDefault('unknown', '', paths), '')

console.log('stash field defaults: ok')

const yes = [
  'recording.mp4',
  'recording.MP4',
  'clip.mov',
  'stream.mkv',
  'stream.webm',
  'audio.mp3',
  'audio.wav',
  'audio.m4a',
  'audio.flac',
  'video.ts',
]
const no = [
  'notes.txt',
  'metadata.json',
  'cover.jpg',
  'subtitle.srt',
  'archive.zip',
  'flagship model',
  'Flagship Model',
  'README',
  '',
]

for (const name of yes) {
  assert.equal(isTranscribableMedia(name), true, `${name} 应该可转写`)
}
for (const name of no) {
  assert.equal(isTranscribableMedia(name), false, `${name} 不应该显示转写入口`)
}

console.log('stash media filter: ok')

const tasks = [
  { id: 1, media_path: '/videos/a.mp4', status: 'pending', attempts: 0 },
  { id: 2, media_path: '/videos/b.mp4', status: 'failed', attempts: 3, last_error: '缺少依赖' },
]
assert.equal(processTaskForFile(tasks, '/videos/a.mp4').id, 1)
assert.equal(processTaskForFile(tasks, '/videos/b.mp4').status, 'failed')
assert.equal(processTaskForFile(tasks, '/videos/c.mp4'), null)
assert.equal(processTaskForFile(tasks, ''), null)
assert.equal(processTaskLabel(tasks[0]), '排队中')
assert.equal(processTaskLabel({ status: 'pending', attempts: 1 }), '等待重试（第 1 次）')
assert.equal(processTaskLabel({ status: 'processing', attempts: 2 }), '正在转写（第 2 次）')
assert.equal(processTaskLabel({ status: 'done' }), '转写完成')
assert.equal(
  processTaskLabel({ status: 'done', last_error: '未配置大模型接口，已保留转写文本，流水线结束' }),
  '转写完成（未配置大模型接口，已保留转写文本）',
)
assert.equal(processTaskLabel(tasks[1]), '转写失败（已尝试 3 次）：缺少依赖')
assert.equal(
  processTaskLabel({ status: 'failed', attempts: 3, last_error: 'moov atom not found' }),
  '转写失败（已尝试 3 次）：moov atom not found',
)
assert.equal(processTaskLabel(null), '')
assert.deepEqual(processTaskAction(null), { disabled: false, label: '转写这个视频' })
assert.deepEqual(processTaskAction(tasks[0]), { disabled: true, label: '已在队列' })
assert.deepEqual(processTaskAction({ status: 'processing', attempts: 1 }), { disabled: true, label: '正在转写' })
assert.deepEqual(processTaskAction({ status: 'done' }), { disabled: true, label: '已经转写过' })
assert.deepEqual(
  processTaskAction({ status: 'done', last_error: '未配置大模型接口，已保留转写文本，流水线结束' }),
  { disabled: true, label: '已经转写过' },
)
assert.deepEqual(processTaskAction(tasks[1]), { disabled: false, label: '重新转写' })

console.log('process task display: ok')

assert.deepEqual(
  processEnqueueFeedback({ queued: true, task_id: 9, disposition: 'queued' }),
  { type: 'success', text: '已加入转写队列（任务 9），完成后会出现在「复盘笔记」' },
)
assert.deepEqual(
  processEnqueueFeedback({ queued: false, task_id: 9, disposition: 'already_active' }),
  { type: 'info', text: '这个视频已有转写任务（任务 9），不用重复加入' },
)
assert.deepEqual(
  processEnqueueFeedback({ queued: false, task_id: 9, disposition: 'already_done' }),
  { type: 'success', text: '这个视频已经转写过（任务 9）' },
)
assert.deepEqual(
  processEnqueueFeedback({ error: '文件不存在' }),
  { type: 'error', text: '加入转写队列失败：文件不存在' },
)
assert.deepEqual(
  processEnqueueFeedback({
    queued: true,
    task_id: 9,
    disposition: 'queued',
    startup_error: '找不到 Rust 核心二进制',
  }),
  {
    type: 'warning',
    text: '已加入转写队列（任务 9），但后台服务启动失败：找不到 Rust 核心二进制',
  },
)
assert.deepEqual(
  processEnqueueFeedback({}),
  { type: 'error', text: '加入转写队列失败：核心未返回任务号' },
)

console.log('process enqueue feedback: ok')

const pending = pendingTranscriptionItems({
  paths: { downloads: '/data/downloads' },
  recent_recordings: [
    { file: '短视频/甲/a.mp4', size_mb: 12.5, minutes_ago: 4 },
    { file: '短视频/甲/notes.txt', size_mb: 1, minutes_ago: 1 },
    { file: '/data/downloads/短视频/乙/b.mov', size_mb: 8, minutes_ago: 9 },
  ],
  process_tasks: [
    { id: 1, media_path: '/data/downloads/短视频/甲/a.mp4', status: 'processing', attempts: 1 },
  ],
})
assert.equal(pending.length, 2)
assert.equal(pending[0].path, '/data/downloads/短视频/甲/a.mp4')
assert.equal(pending[0].action.disabled, true)
assert.equal(pending[0].label, '正在转写（第 1 次）')
assert.equal(pending[1].path, '/data/downloads/短视频/乙/b.mov')
assert.equal(pending[1].action.label, '转写这个视频')

console.log('pending transcription list: ok')

// 同一个文件可能由 status 的 recent_recordings 给相对路径，任务表里却是
// 绝对路径；macOS 也可能一边写 /var、一边写 /private/var。按钮不能因为
// 字符串不等就把“正在转写”误显示成可再次入队。
const normalizedPending = pendingTranscriptionItems({
  paths: { downloads: '/var/downloads' },
  recent_recordings: [
    { file: '短视频/甲/a.mp4', size_mb: 12, minutes_ago: 1 },
  ],
  process_tasks: [
    { id: 3, media_path: '/private/var/downloads/短视频/甲/a.mp4', status: 'processing', attempts: 1 },
  ],
})
assert.equal(normalizedPending.length, 1)
assert.equal(normalizedPending[0].action.disabled, true)
assert.equal(normalizedPending[0].label, '正在转写（第 1 次）')

console.log('pending transcription path normalization: ok')

const realPaths = { media_root: '/Volumes/A区/尊者产物' }
assert.equal(
  itemStashDir({ kind: 'video', name: '影视飓风', url: 'https://space.bilibili.com/1', paths: realPaths }),
  '/Volumes/A区/尊者产物/影视飓风/短视频',
)
assert.equal(
  itemStashDir({ kind: 'live', name: '侯翠翠', url: 'https://live.bilibili.com/1', paths: realPaths }),
  '/Volumes/A区/尊者产物/侯翠翠/直播',
)
assert.equal(
  itemStashFallback({ kind: 'video', url: 'https://space.bilibili.com/1', paths: realPaths }),
  '/Volumes/A区/尊者产物',
)
assert.equal(
  itemStashDir({ kind: 'video', name: '影视飓风', url: 'https://space.bilibili.com/1',
    paths: { media_root: '/Volumes/A区/尊者产物', video_root: '/Volumes/B区/短视频' } }),
  '/Volumes/B区/短视频/影视飓风/短视频',
)

console.log('real stash paths: ok')

assert.deepEqual(
  downloadQueueFeedback(
    { queued: 2, duplicated: 1, skipped_downloaded: 3, errors: 0 },
    { name: '影视飓风', paths: realPaths },
  ),
  {
    type: 'success',
    text: '已加入下载队列：2 个，已存在 1 个，已下载跳过 3 个，失败 0 个。完成后文件在 /Volumes/A区/尊者产物/影视飓风/短视频/',
  },
)
assert.deepEqual(
  downloadQueueFeedback(
    { queued: 0, duplicated: 0, skipped_downloaded: 0, errors: 1 },
    { name: '影视飓风', paths: realPaths },
  ).type,
  'warning',
)
assert.equal(downloadQueueFeedback({ queued: 1 }, { name: '', paths: {} }).text.includes('downloads/短视频'), false)

console.log('download queue feedback: ok')
