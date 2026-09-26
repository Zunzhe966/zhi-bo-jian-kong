// 产物路径推导：界面要能回答「这个主播的东西到底存在哪」。
//
// 为什么要在前端再算一遍目录名：
//   Rust 落盘时用的是 clean_name() / safe_dir_name() 这套规则清洗主播名，
//   界面若直接拿原始名字去拼路径，遇到「影视飓风 | 官方」这种带符号的名字
//   就会指向一个根本不存在的目录，用户点了没反应——这正是被骂过的毛病。
//   所以这里逐字对齐 Rust 的规则，保证拼出来的路径就是真实路径。
//
// 规则来源（改 Rust 时这里必须跟着改）：
//   zunzhe-core/src/live_monitor.rs  clean_name()
//   zunzhe-core/src/download.rs      safe_dir_name()

// Rust clean_name() 里会被替换成下划线的字符，两边必须完全一致。
const UNSAFE = new Set(['/', '\\', ':', '*', '?', '"', '<', '>', '|', '&', '#', '.', ',', '，', '。', '~', '！', '·', ' '])

/** 对齐 Rust clean_name()：逐字符替换非法字符，再裁掉首尾下划线。 */
export function cleanDirName(input) {
  const src = String(input == null ? '' : input).trim()
  let out = ''
  // 用 for...of 而不是 split('')：中文与 emoji 都按码点走，不会被切坏。
  for (const ch of src) out += UNSAFE.has(ch) ? '_' : ch
  const trimmed = out.replace(/^_+/, '').replace(/_+$/, '')
  return trimmed || '空白昵称'
}

/** 对齐 Rust safe_dir_name()：在 cleanDirName 之上再躲开 Windows 保留设备名。 */
export function safeDirName(input) {
  const cleaned = cleanDirName(input)
  const reserved = ['con', 'prn', 'aux', 'nul',
    'com1', 'com2', 'com3', 'com4', 'com5', 'com6', 'com7', 'com8', 'com9',
    'lpt1', 'lpt2', 'lpt3', 'lpt4', 'lpt5', 'lpt6', 'lpt7', 'lpt8', 'lpt9']
  return reserved.includes(cleaned.toLowerCase()) ? `${cleaned}_` : cleaned
}

/**
 * 「存放位置」选择器的起点。
 *
 * 配置留空表示“跟随有效路径”，所以不能一律退到产物根目录：例如日志目录
 * 应落在 status.paths.logs，数据库应落在 status.paths.database。用户点开
 * 选择器时，先看到这个字段当前真正使用的位置，才不会在多层目录里反复找。
 */
export function stashFieldDefault(field, current, paths) {
  const value = String(current == null ? '' : current).trim()
  if (value) return value
  const p = paths || {}
  const fallback = {
    mediaRoot: p.media_root,
    downloadDir: p.video_root || p.media_root,
    fupanDir: p.fupan_output,
    logsDir: p.logs,
    databaseFile: p.database,
  }
  return String(fallback[field] || '').trim()
}

// 复盘流水线能吃的音视频容器。这里刻意只做用户可见的入口过滤：
// 文件是否真的能转写仍由 Rust/Python 在入队时校验，不能因为界面
// “看起来像是支持的”就伪造成功。
const TRANSCRIBABLE_EXTENSIONS = new Set([
  'mp4', 'm4v', 'mov', 'mkv', 'webm', 'avi', 'flv', 'wmv', 'ts', 'm2ts',
  'mp3', 'm4a', 'aac', 'wav', 'flac', 'ogg', 'wma',
])

/** 当前文件是否值得显示「转写这个视频」入口。 */
export function isTranscribableMedia(name) {
  const base = String(name || '').trim().toLowerCase()
  const dot = base.lastIndexOf('.')
  if (dot <= 0 || dot === base.length - 1) return false
  return TRANSCRIBABLE_EXTENSIONS.has(base.slice(dot + 1))
}

/**
 * macOS 上 /var 与 /private/var 指向同一个位置；扫描层和任务表可能各写一种。
 * 只归一化这两个等价前缀，避免把大小写敏感或其他盘符问题悄悄混在一起。
 */
export function normalizeLocalPath(value) {
  const p = String(value || '').trim().replace(/\/+/g, '/')
  if (!p) return ''
  if (p === '/private/var') return '/var'
  if (p.startsWith('/private/var/')) return p.slice('/private'.length)
  return p
}

/** 找到某个媒体文件最近一次复盘任务；路径按本地等价形式匹配。 */
export function processTaskForFile(tasks, filePath) {
  const needle = normalizeLocalPath(filePath)
  if (!needle) return null
  const rows = Array.isArray(tasks) ? tasks : []
  return rows.find((task) => normalizeLocalPath(task && task.media_path) === needle) || null
}

/** 把队列状态翻译成界面上能直接显示的一行话。 */
export function processTaskLabel(task) {
  if (!task) return ''
  const attempts = Number(task.attempts) || 0
  if (task.status === 'pending') return attempts > 0 ? `等待重试（第 ${attempts} 次）` : '排队中'
  if (task.status === 'processing') return `正在转写（第 ${Math.max(attempts, 1)} 次）`
  if (task.status === 'done') {
    // “转写成功但 LLM 未配置”仍应显示完成，不能把整条任务标失败；说明要
    // 保留给用户，避免以为转写没有产出。
    const note = String(task.last_error || '')
    if (note.includes('未配置大模型接口') && note.includes('已保留转写文本')) {
      return '转写完成（未配置大模型接口，已保留转写文本）'
    }
    return '转写完成'
  }
  if (task.status === 'failed') {
    const base = attempts > 0 ? `转写失败（已尝试 ${attempts} 次）` : '转写失败'
    const reason = String(task.last_error || '').replace(/\s+/g, ' ').trim()
    return reason ? `${base}：${reason.slice(0, 120)}` : base
  }
  return String(task.status || '')
}

/** 转写按钮的真实动作：已经在跑或已完成时不能继续装作还能入队。 */
export function processTaskAction(task) {
  if (!task) return { disabled: false, label: '转写这个视频' }
  if (task.status === 'pending' || task.status === 'processing') {
    return { disabled: true, label: task.status === 'processing' ? '正在转写' : '已在队列' }
  }
  if (task.status === 'done') return { disabled: true, label: '已经转写过' }
  return { disabled: false, label: '重新转写' }
}

/** 把 `process` 命令的返回翻译成真实反馈，不把“已有任务”误报成失败。 */
export function processEnqueueFeedback(result) {
  const r = result || {}
  if (r.error) return { type: 'error', text: `加入转写队列失败：${r.error}` }
  if (r.queued) {
    if (r.startup_error) {
      return {
        type: 'warning',
        text: `已加入转写队列（任务 ${r.task_id}），但后台服务启动失败：${r.startup_error}`,
      }
    }
    return { type: 'success', text: `已加入转写队列（任务 ${r.task_id}），完成后会出现在「复盘笔记」` }
  }
  if (r.disposition === 'already_active') {
    return { type: 'info', text: `这个视频已有转写任务（任务 ${r.task_id}），不用重复加入` }
  }
  if (r.disposition === 'already_done') {
    return { type: 'success', text: `这个视频已经转写过（任务 ${r.task_id}）` }
  }
  return { type: 'error', text: '加入转写队列失败：核心未返回任务号' }
}

/** 批量下载入队后的用户反馈；目录必须取当前真实产物根，不能写死相对路径。 */
export function downloadQueueFeedback(result, { name, paths } = {}) {
  const r = result || {}
  const queued = Number(r.queued) || 0
  const duplicated = Number(r.duplicated) || 0
  const skipped = Number(r.skipped_downloaded) || 0
  const failed = Number(r.errors) || 0
  const dir = itemStashDir({
    kind: 'video',
    name: name || '批量下载',
    url: 'https://space.bilibili.com/0',
    paths,
    category: '批量下载',
  })
  const suffix = dir ? `。完成后文件在 ${dir}/` : ''
  return {
    type: failed ? 'warning' : 'success',
    text: `已加入下载队列：${queued} 个，已存在 ${duplicated} 个，已下载跳过 ${skipped} 个，失败 ${failed} 个${suffix}`,
  }
}

/**
 * 复盘笔记页顶部的「可直接转写的视频」清单。
 *
 * 状态快照里的 recent_recordings 只保留最近 10 个媒体文件，process_tasks 保留
 * 真实队列状态。这个纯函数把两者按完整路径对齐，让用户不用先进博主详情再找
 * 文件行，也能从「复盘笔记」一眼看到还能转什么、什么正在转、什么已完成。
 *
 * 只返回媒体文件；目录、空路径、非音视频文件都排除。失败任务保留在列表里，
 * 由 processTaskAction 显示「重新转写」，这样重试入口也不会消失。
 */
export function pendingTranscriptionItems(status) {
  const files = Array.isArray(status && status.recent_recordings) ? status.recent_recordings : []
  const tasks = Array.isArray(status && status.process_tasks) ? status.process_tasks : []
  const base = String(
    (status && status.paths && (status.paths.media_root || status.paths.downloads)) || '',
  ).replace(/\/+$/, '')
  return files
    .map((row) => {
      const raw = String((row && row.file) || '')
      if (!raw || !isTranscribableMedia(raw)) return null
      const fullPath = raw.startsWith('/')
        ? raw
        : base
          ? `${base}/${raw.replace(/^\/+/, '')}`
          : raw
      const task = processTaskForFile(tasks, fullPath)
      return {
        path: fullPath,
        name: fullPath.split('/').pop() || fullPath,
        sizeMb: Number(row && row.size_mb) || 0,
        minutesAgo: Number(row && row.minutes_ago) || 0,
        task,
        action: processTaskAction(task),
        label: processTaskLabel(task),
      }
    })
    .filter(Boolean)
}

/** 去掉真实路径里可能存在的结尾斜杠，拼路径时不会出现双斜杠。 */
function trimSlash(p) {
  return String(p || '').replace(/\/+$/, '')
}

/**
 * 算出一个主播/博主的产物目录。
 *
 * 布局（主播名在最上层，一个主播的东西都在他自己的文件夹里）：
 * kind='live'  → <录像根>/<主播名>/直播/
 * kind='video' → <短视频根>/<博主名>/短视频/
 * 批量下载      → <短视频根>/<博主名>/批量下载/
 *
 * 目录可能还没建（刚添加、还没跑过一轮），主进程的 resolveOpenablePath 会自动
 * 退到「仍在白名单内、且真实存在的最近祖先」，所以点下去总能到某个真实位置，
 * 不会出现「碰都碰不了」。
 */
export function itemStashDir({ kind, name, url, paths, category }) {
  const cat = category || (kind === 'live' ? '直播' : '短视频')
  const base = trimSlash(
    kind === 'live'
      ? paths && (paths.recordings || paths.media_root)
      : paths && (paths.video_root || paths.media_root),
  )
  if (!base) return ''
  const anchor = kind === 'live' ? cleanDirName(name) : safeDirName(name)
  return `${base}/${anchor}/${cat}`
}

/** 更上一层的兜底目录：主播自己的文件夹还没建时，至少能落到产物根。 */
export function itemStashFallback({ kind, url, paths }) {
  const base = kind === 'live'
    ? trimSlash(paths && paths.media_root)
    : trimSlash(paths && (paths.video_root || paths.media_root))
  return base || ''
}

/** 把字节数说成人话：1.4 GB 比 1503238553 好读得多。 */
export function humanSize(bytes) {
  const n = Number(bytes) || 0
  if (n < 1024) return `${n} B`
  const units = ['KB', 'MB', 'GB', 'TB']
  let v = n / 1024
  let i = 0
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024
    i += 1
  }
  return `${v >= 100 ? v.toFixed(0) : v.toFixed(1)} ${units[i]}`
}

/** 时间戳转「2026-09-17 19:50」，取不到就给个短横线而不是 Invalid Date。 */
export function stamp(ms) {
  const t = Number(ms)
  if (!t) return '-'
  const d = new Date(t)
  if (Number.isNaN(d.getTime())) return '-'
  const p = (x) => String(x).padStart(2, '0')
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`
}
