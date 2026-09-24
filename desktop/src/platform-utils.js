// 平台识别与地址归一化（纯逻辑，无 React 依赖）。
// 拆分自 App.jsx，目的：平台判断/链接处理的改动不影响主界面代码。

export const platformLabels = {
  Auto: '自动',
  DouYin: '抖音',
  KuaiShou: '快手',
  Bilibili: 'B站',
  Huya: '虎牙',
  Douyu: '斗鱼',
  Xiaohongshu: '小红书',
  TikTok: 'TikTok',
  Youtube: 'YouTube',
  Twitch: 'Twitch',
  Other: '其他',
}

export const platformColors = {
  DouYin: '#111',
  KuaiShou: '#ff4906',
  Bilibili: '#fb7299',
  Huya: '#ffba00',
  Douyu: '#ff5d23',
  Xiaohongshu: '#fe2c55',
  TikTok: '#010101',
  Youtube: '#ff0000',
  Twitch: '#9146ff',
  Other: '#1677ff',
}

export function detectPlatform(url = '') {
  const u = url.toLowerCase()
  if (u.includes('douyin')) return 'DouYin'
  if (u.includes('kuaishou')) return 'KuaiShou'
  if (u.includes('bilibili') || u.includes('live.bilibili')) return 'Bilibili'
  if (u.includes('huya.com')) return 'Huya'
  if (u.includes('douyu.com')) return 'Douyu'
  if (u.includes('xiaohongshu') || u.includes('xhslink')) return 'Xiaohongshu'
  if (u.includes('tiktok')) return 'TikTok'
  if (u.includes('youtube.com') || u.includes('youtu.be')) return 'Youtube'
  if (u.includes('twitch.tv') || u.includes('twitch')) return 'Twitch'
  return 'Other'
}

/** 平台的中文名，用于卡片上直接写清「这是哪个网站」。 */
export function platformLabel(id) {
  return platformLabels[id] || '其他'
}

export function normalizeUrl(input, platform) {
  let url = String(input || '').trim()
  if (!url) return ''
  if (url.startsWith('http')) return url
  const p = platform === 'Auto' ? detectPlatform(url) : platform
  if (p === 'DouYin') return `https://live.douyin.com/${url}`
  if (p === 'KuaiShou') return `https://live.kuaishou.com/u/${url}`
  if (p === 'Bilibili') return `https://live.bilibili.com/${url}`
  if (p === 'Huya') return `https://www.huya.com/${url}`
  if (p === 'Douyu') return `https://www.douyu.com/${url}`
  return url
}

export function buildRows(status) {
  const activeFiles = (status.recent_recordings || [])
    .filter((r) => r.probably_recording)
    .map((r) => r.file.toLowerCase())
  const recordingUrls = new Set(
    (status.recording_sessions || []).map((s) => String(s.url || '').toLowerCase())
  )
  return (status.live_watchlist || []).map((room, idx) => {
    const platform = detectPlatform(room.url)
    const remark = (room.remark || '').replace(/^主播:\s*/, '')
    const roomUrl = room.url.toLowerCase()
    const isRecording = room.enabled && (
      recordingUrls.has(roomUrl) ||
      (remark && activeFiles.some((f) => f.includes(remark.toLowerCase())))
    )
    let state = 'noting'
    if (!room.enabled) state = 'noting'
    else if (isRecording) state = 'recording'
    else if (status.services?.live_monitor?.running) state = 'monitoring'
    return { key: room.url, index: idx + 1, platform, remark: room.remark || room.url, url: room.url, enabled: room.enabled, quality: room.quality, notify: room.notify || '跟随全局', state }
  })
}
