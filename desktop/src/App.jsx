import React, { useCallback, useEffect, useMemo, useState } from 'react'
import {
  Badge, Button, Empty, Form, Input, InputNumber, message, Modal,
  Popconfirm, Select, Space, Switch, Table, Tag, Tooltip,
} from 'antd'
import {
  ControlFilled, DeleteFilled, FolderFilled, FolderOpenFilled, MailOutlined,
  PlayCircleFilled, PlusCircleFilled, ReloadOutlined, SettingOutlined, StopFilled,
  VideoCameraFilled, CloudDownloadOutlined, FileTextOutlined, UnorderedListOutlined,
  ClearOutlined, RollbackOutlined, CloudSyncOutlined, AppstoreOutlined,
} from '@ant-design/icons'
import douyinIcon from '../assets/douyin.png'
import kuaishouIcon from '../assets/kuaishou.png'
import appIcon from '../assets/icon.png'
import { LiveCard, VideoCard, statusFingerprint } from './PersonCards'
import {
  itemStashDir, itemStashFallback, humanSize, stamp,
  isTranscribableMedia, processEnqueueFeedback, processTaskAction,
  
  downloadQueueFeedback,
} from './stash'
import LogsPanel from './LogsPanel'
import SetupWizard from './Wizard'

const qualityOptions = [
  { label: '原画', value: '原画' },
  { label: '蓝光', value: '蓝光' },
  { label: '超清', value: '超清' },
  { label: '高清', value: '高清' },
  { label: '标清', value: '标清' },
  { label: '流畅', value: '流畅' },
]

const notifyOptions = [
  { label: '跟随全局', value: '跟随全局' },
  { label: '微信（Server酱，省额度给重要的）', value: '微信' },
  { label: '邮箱', value: '邮箱' },
  { label: '不通知', value: '不通知' },
]

const platformOptions = [
  { label: '自动识别（推荐）', value: 'Auto' },
  { label: '抖音', value: 'DouYin' },
  { label: '快手', value: 'KuaiShou' },
  { label: 'B站', value: 'Bilibili' },
  { label: '其他（直接粘贴完整链接）', value: 'Other' },
]

const platformLabels = {
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

const platformColors = {
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

const statusText = {
  recording: '录制中',
  monitoring: '监控中，等待开播',
  noting: '未开始监控',
}

// 构建时间与版本号由 vite 注入，用来确认界面是否已更新
const BUILD_LABEL = typeof __APP_BUILD__ !== 'undefined' ? __APP_BUILD__ : '开发版'
const APP_VERSION = typeof __APP_VERSION__ !== 'undefined' ? __APP_VERSION__ : 'dev'

function detectPlatform(url = '') {
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

function platformIcon(id) {
  if (id === 'DouYin') return <img className="platform-icon" src={douyinIcon} alt="抖音" />
  if (id === 'KuaiShou') return <img className="platform-icon" src={kuaishouIcon} alt="快手" />
  const label = platformLabels[id] || '其他'
  const color = platformColors[id] || '#1677ff'
  return (
    <Tooltip title={label}>
      <VideoCameraFilled style={{ fontSize: 22, color }} />
    </Tooltip>
  )
}

/** 平台的中文名，用于卡片上直接写清「这是哪个网站」。 */
function platformLabel(id) {
  return platformLabels[id] || '其他'
}

function normalizeUrl(input, platform) {
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

function buildRows(status) {
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

export default function App() {
  const [loading, setLoading] = useState(false)
  const [status, setStatus] = useState(null)
  const [addOpen, setAddOpen] = useState(false)
  const [videoOpen, setVideoOpen] = useState(false)
  const [settingsOpen, setSettingsOpen] = useState(false)
  const [settingsDatabaseBaseline, setSettingsDatabaseBaseline] = useState('')
  // 复盘笔记页：哪些「人/内容」的分组被展开了。
  // 复盘目录的结构是 fupan_output/<主播或内容名>/<场次>/<文件>，
  // anchor 就是顶层文件夹——直播录的是主播名，B站投稿复盘的是视频标题，如实展示。
  const [addForm] = Form.useForm()
  const [videoForm] = Form.useForm()
  const [emailForm] = Form.useForm()
  const [emailSaving, setEmailSaving] = useState(false)
  const [emailStatusText, setEmailStatusText] = useState('')
  const [emailConfigured, setEmailConfigured] = useState(false)
  const [historyOpen, setHistoryOpen] = useState(false)
  const [historyLoading, setHistoryLoading] = useState(false)
  const [historyVideos, setHistoryVideos] = useState([])
  const [historyBlogger, setHistoryBlogger] = useState(null)
  const [historySelected, setHistorySelected] = useState([])
  const [historyDownloading, setHistoryDownloading] = useState(false)
  const [batchOpen, setBatchOpen] = useState(false)
  const [batchForm] = Form.useForm()
  const [itemSettingsOpen, setItemSettingsOpen] = useState(false)
  const [itemSettingsType, setItemSettingsType] = useState('live') // live | video
  const [itemSettingsRow, setItemSettingsRow] = useState(null)
  const [itemSettingsForm] = Form.useForm()
  // 主播/博主详情弹窗的状态。
  // 注意：这几个声明曾经漏掉过一次——详情弹窗引用了 detailType 却没人定义，
  // 结果整个界面一渲染就抛 ReferenceError，直接黑屏。改这块必须核对声明存在。
  const [detailOpen, setDetailOpen] = useState(false)
  const [detailType, setDetailType] = useState('live') // live | video
  const [detailRow, setDetailRow] = useState(null)
  const [detailLoading, setDetailLoading] = useState(false)
  const [detailQueueing, setDetailQueueing] = useState('')
  const [detailPath, setDetailPath] = useState('')
  const [detailFiles, setDetailFiles] = useState([])
  const [detailError, setDetailError] = useState('')
  const [mainTab, setMainTab] = useState('live')
  const [wizardOpen, setWizardOpen] = useState(false)
  const [updateBusy, setUpdateBusy] = useState(false)
  const [notifyOpen, setNotifyOpen] = useState(false)
  const [notifyLoading, setNotifyLoading] = useState(false)
  const [notifyEvents, setNotifyEvents] = useState([])
  const [notifyBusy, setNotifyBusy] = useState(false)

  const pickCfg = (section, key, fallback = '') => {
    const v = section?.[key]
    if (v !== undefined && v !== null && v !== '') return v
    const lower = Object.entries(section || {}).find(([k]) => k.toLowerCase() === key.toLowerCase())
    return lower ? lower[1] : fallback
  }

  // 「存放位置」里挑文件夹。defaultPath 用该字段当前值（留空时用其有效路径）。
  const pickStashDir = async (field) => {
    const cur = stashFieldDefault(field, emailForm.getFieldValue(field), status?.paths)
    const r = await window.desktop.chooseDir({ title: '选择存放位置', defaultPath: cur })
    if (r && r.ok && r.path) emailForm.setFieldsValue({ [field]: r.path })
  }

  // 数据库是文件而不是目录，必须用文件选择器；目录选择器会把父目录写进配置。
  const pickDatabaseFile = async () => {
    const cur = stashFieldDefault('databaseFile', emailForm.getFieldValue('databaseFile'), status?.paths)
    const r = await window.desktop.chooseFile({
      title: '选择数据库文件',
      defaultPath: cur,
      filters: [{ name: 'SQLite 数据库', extensions: ['db', 'sqlite', 'sqlite3'] }],
    })
    if (r && r.ok && r.path) emailForm.setFieldsValue({ databaseFile: r.path })
  }

  const openSettings = async () => {
    setSettingsOpen(true)
    try {
      const [cfg, fupanCfg, pathCfg, prefRes] = await Promise.all([
        window.desktop.cli('get-config', 'config', '推送配置'),
        window.desktop.cli('get-config', 'fupan_config'),
        window.desktop.cli('get-config', 'config', '路径设置'),
        window.desktop.cli('get-app-prefs'),
      ])
      const push = cfg['config.ini']?.['推送配置'] || {}
      const tx = fupanCfg['fupan_config.ini']?.['转写'] || {}
      const stash = pathCfg['config.ini']?.['路径设置'] || {}
      const prefs = prefRes.prefs || {}
      const mail = await window.desktop.cli('email-status')
      setEmailStatusText(mail.hint || (mail.configured ? '已配置' : '未配置'))
      setEmailConfigured(!!mail.configured)
      const databaseFile = pickCfg(stash, '数据库文件')
      setSettingsDatabaseBaseline(String(databaseFile || '').trim())
      emailForm.setFieldsValue({
        smtpHost: pickCfg(push, 'smtp邮件服务器', 'smtp.qq.com'),
        smtpPort: pickCfg(push, 'SMTP邮件服务器端口', '465'),
        loginEmail: pickCfg(push, '邮箱登录账号'),
        senderEmail: pickCfg(push, '发件人邮箱'),
        senderName: pickCfg(push, '发件人显示昵称', '尊者直播监控'),
        toEmail: pickCfg(push, '收件人邮箱'),
        authCode: '',
        serverChanKey: '',
        pushplusToken: '',
        transcribeEnabled: pickCfg(tx, '是否启用转写(是/否)', '是') !== '否',
        whisperModel: pickCfg(tx, 'whisper模型', 'base'),
        pipelineTimeout: pickCfg(fupanCfg['fupan_config.ini']?.['复盘'] || {}, '任务超时秒数', '21600'),
        watchdog: prefs.watchdog !== false,
        autoStartServices: prefs.autoStartServices !== false,
        openAtLogin: !!prefs.openAtLogin,
        localNotify: prefs.localNotify !== false,
        mediaRoot: pickCfg(stash, '产物根目录'),
        downloadDir: pickCfg(stash, '短视频下载目录'),
        fupanDir: pickCfg(stash, '复盘输出目录'),
        logsDir: pickCfg(stash, '日志目录'),
        databaseFile,
      })
    } catch (e) {
      message.error(String(e.message || e))
    }
  }

  const saveEmailSettings = async () => {
    setEmailSaving(true)
    try {
      const v = await emailForm.validateFields()
      const login = v.loginEmail?.trim()
      const sender = (v.senderEmail || login)?.trim()
      // 开播推送渠道：邮箱 + （配了 PushPlus 就加上，让直播开播也走抗代理通道）
      const channels = []
      if (login) channels.push('邮箱')
      if (v.pushplusToken?.trim()) channels.push('PUSHPLUS')
      const pairs = [
        ['直播状态推送渠道', channels.join(',') || '邮箱'],
        ['smtp邮件服务器', v.smtpHost || 'smtp.qq.com'],
        ['是否使用SMTP服务SSL加密(是/否)', '是'],
        ['SMTP邮件服务器端口', String(v.smtpPort || 465)],
        ['邮箱登录账号', login || ''],
        ['发件人邮箱', sender || ''],
        ['发件人显示昵称', v.senderName || '尊者直播监控'],
        ['收件人邮箱', v.toEmail?.trim() || login || ''],
        ['开播推送开启(是/否)', '是'],
      ]
      for (const [key, val] of pairs) {
        await window.desktop.cli('set-config', 'config', '推送配置', key, val)
      }
      if (v.authCode?.trim()) {
        await window.desktop.cli('set-config', 'config', '推送配置', '发件人密码(授权码)', v.authCode.trim())
      }
      if (v.serverChanKey?.trim()) {
        await window.desktop.cli('set-config', 'config', '推送配置', 'server酱SendKey', v.serverChanKey.trim())
      }
      if (v.pushplusToken?.trim()) {
        await window.desktop.cli('set-config', 'config', '推送配置', 'pushplus推送token', v.pushplusToken.trim())
      }
      await window.desktop.cli('set-config', 'fupan_config', '转写', '是否启用转写(是/否)', v.transcribeEnabled ? '是' : '否')
      if (v.whisperModel) {
        await window.desktop.cli('set-config', 'fupan_config', '转写', 'whisper模型', v.whisperModel)
      }
      if (v.pipelineTimeout) {
        await window.desktop.cli('set-config', 'fupan_config', '复盘', '任务超时秒数', String(v.pipelineTimeout).trim())
      }
      // 存放位置。留空 = 用默认落点，所以无条件写（写空串即恢复默认）。
      // 产物刻意不落在程序目录里：程序换代时不该连带动用户的录像和转写文本。
      const stashPairs = [
        ['产物根目录', v.mediaRoot],
        ['短视频下载目录', v.downloadDir],
        ['复盘输出目录', v.fupanDir],
        ['日志目录', v.logsDir],
        ['数据库文件', v.databaseFile],
      ]
      for (const [key, val] of stashPairs) {
        await window.desktop.cli('set-config', 'config', '路径设置', key, (val || '').trim())
      }
      await window.desktop.cli('set-app-prefs', JSON.stringify({
        watchdog: v.watchdog !== false,
        autoStartServices: v.autoStartServices !== false,
        openAtLogin: !!v.openAtLogin,
        localNotify: v.localNotify !== false,
      }))
      if (window.desktop.setLoginItem) {
        await window.desktop.setLoginItem(!!v.openAtLogin)
      }
      const databaseChanged = String(v.databaseFile || '').trim() !== settingsDatabaseBaseline
      message.success(databaseChanged
        ? '配置已保存；数据库路径将在后台服务重启后生效'
        : '配置已保存')
    } catch (e) {
      message.error(String(e.message || e))
    } finally {
      setEmailSaving(false)
    }
  }

  const testEmail = async () => {
    try {
      const r = await window.desktop.cli('test-email')
      if (r.ok) {
        message.success('测试通知已发出，请查收邮箱')
      } else {
        message.error('邮箱未配置或发送失败：请先在下方填写 QQ 邮箱与 SMTP 授权码')
      }
    } catch (e) {
      message.error(String(e.message || e))
    }
  }

  const setupQqMail = async () => {
    const v = emailForm.getFieldsValue()
    const login = v.loginEmail?.trim()
    if (!login) return message.warning('请先填写发件邮箱（QQ 邮箱地址）')
    emailForm.setFieldsValue({
      smtpHost: 'smtp.qq.com',
      smtpPort: '465',
      loginEmail: login,
      senderEmail: login,
      senderName: v.senderName || '尊者直播监控',
      toEmail: v.toEmail?.trim() || login,
    })
    message.success('已填入 QQ 邮箱模板，填好授权码后点「保存配置」')
  }

  const biliLogin = async () => {
    try {
      message.info('正在打开 B站，可选择登录账号（下载会员/1080P需要），浏览或登录后关闭该窗口即可')
      const r = await window.desktop.biliLogin()
      message[r?.ok ? 'success' : 'error'](r?.ok
        ? `B站授权完成，已保存 ${r.count} 项登录状态，之后下载自动使用`
        : `B站授权失败：${r?.error || '未知错误'}`)
    } catch (e) {
      message.error(String(e.message || e))
    }
  }

  const douyinLogin = async () => {
    try {
      message.info('正在打开抖音登录页，请用手机抖音扫码登录，登录成功后会自动保存（约 5 分钟内有效）')
      const r = await window.desktop.douyinLogin()
      message[r?.ok ? 'success' : 'error'](r?.ok
        ? `抖音授权完成，已保存 ${r.count} 项登录状态，之后短视频监控/下载自动使用`
        : `抖音授权失败：${r?.error || '未知错误'}`)
    } catch (e) {
      message.error(String(e.message || e))
    }
  }

  const fetchStatus = useCallback(async () => {
    const data = await window.desktop.cli('status')
    setStatus((prev) => {
      const nextFp = statusFingerprint(data)
      const prevFp = statusFingerprint(prev)
      if (prev && nextFp === prevFp) return prev
      return data
    })
    return data
  }, [])

  const refresh = useCallback(async ({ showLoading = true } = {}) => {
    if (showLoading) setLoading(true)
    try {
      await fetchStatus()
    } catch (e) {
      message.error(String(e.message || e))
    } finally {
      if (showLoading) setLoading(false)
    }
  }, [fetchStatus])

  const reloadUi = async () => {
    if (!window.desktop.reloadUi) {
      message.warning('请完全退出后重新运行启动脚本')
      return
    }
    try {
      await window.desktop.reloadUi()
    } catch (e) {
      message.error(String(e.message || e))
    }
  }

  // 检查更新：更新源是「数据目录/releases」，离线可用。
  // 只替换程序包，数据目录一个字节都不动，所以不存在「更新一次数据没了」。
  const checkUpdate = async () => {
    if (!window.desktop.checkUpdate) {
      message.warning('当前程序不支持在线更新，请重新打包安装')
      return
    }
    setUpdateBusy(true)
    try {
      const r = await window.desktop.checkUpdate({ quiet: true })
      if (r?.error) {
        message.error(`检查更新失败：${r.error}`)
      } else if (r?.applied) {
        message.success(`已更新到 ${r.latest}，程序正在重启`)
      } else if (r?.hasUpdate) {
        message.warning(`发现 ${r.latest}，本次未更新`)
      } else {
        message.info(`已是最新版本（${r?.current || '未知'}）`)
      }
    } catch (e) {
      message.error(String(e.message || e))
    } finally {
      setUpdateBusy(false)
    }
  }

  useEffect(() => {
    let cancelled = false
    ;(async () => {
      setLoading(true)
      try {
        if (!cancelled) await fetchStatus()
        if (!cancelled) {
          const prefRes = await window.desktop.cli('get-app-prefs')
          const data = status
          if (!prefRes.prefs?.wizardDone) {
            const st = await window.desktop.cli('status')
            const hasItems = ((st?.live_watchlist?.length || 0) + (st?.video_watchlist?.length || 0)) > 0
            if (hasItems) {
              await window.desktop.cli('wizard-done')
            } else {
              setWizardOpen(true)
            }
          }
        }
      } catch (e) {
        if (!cancelled) message.error(String(e.message || e))
      } finally {
        if (!cancelled) setLoading(false)
      }
    })()
    // 窗口可见时才轮询；无变化不触发 React 重绘
    const tick = () => {
      if (!document.hidden) fetchStatus().catch(() => {})
    }
    const t = setInterval(tick, 30000)
    document.addEventListener('visibilitychange', tick)
    return () => {
      cancelled = true
      clearInterval(t)
      document.removeEventListener('visibilitychange', tick)
    }
  }, [fetchStatus])

  const rows = useMemo(() => buildRows(status || {}), [status])
  const videoRows = useMemo(
    () => (status?.video_watchlist || []).map((v, i) => ({ ...v, key: v.url, index: i + 1, platform: v.platform || detectPlatform(v.url) })),
    [status?.video_watchlist],
  )



  const ensureStarted = async () => {
    if (!status?.rust_daemon && !status?.services?.live_monitor?.running) {
      await window.desktop.cli('start')
      message.success('监控服务已启动')
      await fetchStatus()
    }
  }

  const onAddLive = async () => {
    const v = await addForm.validateFields()
    const url = normalizeUrl(v.channelId, v.providerId)
    if (!url.startsWith('http')) return message.error('请输入有效的直播间地址或ID')
    setLoading(true)
    try {
      await window.desktop.cli('add-live', url, v.remarks ? `主播: ${v.remarks}` : '', v.quality || '', v.notify || '跟随全局')
      await ensureStarted()
      message.success('已添加直播监控')
      setAddOpen(false)
      addForm.resetFields()
      await fetchStatus()
    } catch (e) {
      message.error(String(e.message || e))
    } finally {
      setLoading(false)
    }
  }

  const onAddVideo = async () => {
    const v = await videoForm.validateFields()
    setLoading(true)
    try {
      await window.desktop.cli('add-video', v.url.trim(), v.name.trim(), v.autoDownload ? '是' : '否', v.notify || '跟随全局')
      await ensureStarted()
      message.success('已添加发布视频监控')
      setVideoOpen(false)
      videoForm.resetFields()
      await fetchStatus()
    } catch (e) {
      message.error(String(e.message || e))
    } finally {
      setLoading(false)
    }
  }

  const setItemNotify = async (url, channel, name = '') => {
    try {
      await window.desktop.cli('set-notify', url, channel, name)
      await fetchStatus()
    } catch (e) {
      message.error(String(e.message || e))
      throw e
    }
  }

  // ---------- 存放位置 / 主播详情 ----------
  // 用户的原话是「东西放哪儿了，在设置里直接写上，点击就能直接去」。
  // 所以这里三个能力必须都是真的：算出真实路径 → 能打开 → 能列出里面有什么。
  const paths = status?.paths || {}

  /** 主播/博主在配置里的显示名。直播的写在 remark 里（带「主播: 」前缀），视频的是 name。 */
  const rowDisplayName = (row, kind) => {
    if (!row) return ''
    if (kind === 'live') return (row.remark || '').replace(/^主播:\s*/, '') || ''
    return row.name || ''
  }

  const stashOf = (row, kind) =>
    itemStashDir({ kind, name: rowDisplayName(row, kind), url: row && row.url, paths })

  /** 上一级目录：主播自己的文件夹还没建时，至少能落到平台/分类目录。 */
  const fallbackStashOf = (row, kind) =>
    itemStashFallback({ kind, url: row && row.url, paths })

  /** 「打开存放位置」：真的进到那个文件夹。openPath 返回空串=成功，返回字符串=失败。 */
  const openItemStash = async (row, kind) => {
    const dir = stashOf(row, kind)
    if (!dir) {
      message.warning('还没解析出存放位置，请先在「全局设置」里确认产物根目录')
      return
    }
    const err = await window.desktop.openPath(dir)
    if (err) {
      // 精确目录不存在（这个主播还没录过）时退一级。主进程的 resolveOpenablePath
      // 通常已经帮我们退到存在的祖先了，这里再兜一次，避免「点了没反应」。
      const up = fallbackStashOf(row, kind)
      if (up && up !== dir) {
        const err2 = await window.desktop.openPath(up)
        if (!err2) message.info('该主播还没有自己的文件夹，已打开上一级目录')
      }
    }
  }

  const loadDetailFiles = async (row, kind) => {
    setDetailLoading(true)
    try {
      const dir = stashOf(row, kind)
      const res = await window.desktop.listDir(dir)
      if (res && res.ok) {
        setDetailPath(res.path || dir)
        setDetailFiles(res.items || [])
        setDetailError('')
      } else {
        const up = fallbackStashOf(row, kind)
        const res2 = up ? await window.desktop.listDir(up) : null
        if (res2 && res2.ok) {
          setDetailPath(res2.path || up)
          setDetailFiles(res2.items || [])
          setDetailError('')
        } else {
          setDetailPath(dir)
          setDetailFiles([])
          setDetailError((res && res.error) || '读不到目录')
        }
      }
    } catch (e) {
      setDetailError(String(e.message || e))
    } finally {
      setDetailLoading(false)
    }
  }

  /** 点一个主播/博主：进详情页，里面是他的地址、存放位置、文件清单。 */
  const openItemDetail = async (row, kind) => {
    setDetailRow(row)
    setDetailType(kind)
    setDetailOpen(true)
    setDetailFiles([])
    setDetailError('')
    setDetailPath('')
    await loadDetailFiles(row, kind)
  }


  /** 把一个真实存在的本地媒体文件交给 daemon 复盘队列。
   *
   * 以前设置页只有「录制/下载完成后自动转写」开关：打开它不会处理已经下载
   * 的视频，详情页也没有入口，用户看到文字稿目录空了会以为转写坏了。这里
   * 直接调用现有 process 能力，不复制转写逻辑；成功与否以核心返回的
   * task_id / error 为准，不伪造“已转写”。
   */
  const queueTranscription = async (file) => {
    if (!file || file.dir) return
    const p = file.path || ''
    if (!p) return message.warning('这个文件没有可用路径，请先刷新文件列表')
    setDetailQueueing(p)
    try {
      const r = await window.desktop.cli('process', p, rowDisplayName(detailRow, detailType))
      const feedback = processEnqueueFeedback(r)
      message[feedback.type](feedback.text)
      // 已有活动任务或已完成任务时，后台状态也已经变化；仍然刷新一次，
      // 让按钮/标签立刻对齐真实队列，而不是等下一次 30 秒轮询。
      await fetchStatus()
    } catch (e) {
      message.error(`加入转写队列失败：${String(e.message || e)}`)
    } finally {
      setDetailQueueing('')
    }
  }

  const openItemSettings = (row, type) => {
    setItemSettingsRow(row)
    setItemSettingsType(type)
    setItemSettingsOpen(true)
    if (type === 'live') {
      const name = (row.remark || '').replace(/^主播:\s*/, '')
      itemSettingsForm.setFieldsValue({
        name,
        url: row.url,
        enabled: row.enabled,
        quality: row.quality || '原画',
        notify: row.notify || '跟随全局',
      })
    } else {
      itemSettingsForm.setFieldsValue({
        name: row.name,
        url: row.url,
        autoDownload: row.download === null || row.download === undefined ? true : !!row.download,
        notify: row.notify || '跟随全局',
      })
    }
  }

  const saveItemSettings = async () => {
    const v = await itemSettingsForm.validateFields()
    setLoading(true)
    try {
      if (itemSettingsType === 'live') {
        const remark = v.name ? `主播: ${v.name}` : ''
        if (v.enabled) {
          await window.desktop.cli('add-live', v.url, remark, v.quality || '', v.notify || '跟随全局')
        } else {
          await window.desktop.cli('add-live', v.url, remark, v.quality || '', v.notify || '跟随全局')
          await window.desktop.cli('remove-live', v.url)
        }
      } else {
        await window.desktop.cli('add-video', v.url, v.name, v.autoDownload ? '是' : '否', v.notify || '跟随全局')
      }
      message.success('设置已保存')
      setItemSettingsOpen(false)
      await fetchStatus()
    } catch (e) {
      message.error(String(e.message || e))
    } finally {
      setLoading(false)
    }
  }

  const deleteFromSettings = async () => {
    if (!itemSettingsRow) return
    setLoading(true)
    try {
      if (itemSettingsType === 'live') {
        await window.desktop.cli('delete-live', itemSettingsRow.url)
        message.success('已删除，不再关注该主播')
      } else {
        await window.desktop.cli('remove-video', itemSettingsRow.url)
        message.success('已删除，不再关注该博主')
      }
      setItemSettingsOpen(false)
      await fetchStatus()
    } catch (e) {
      message.error(String(e.message || e))
    } finally {
      setLoading(false)
    }
  }

  const toggleVideoDownload = async (row, checked) => {
    setLoading(true)
    try {
      await window.desktop.cli('add-video', row.url, row.name, checked ? '是' : '否')
      message.success(checked ? '发现新视频将自动下载' : '发现新视频只通知，不下载')
      await fetchStatus()
    } catch (e) {
      message.error(String(e.message || e))
    } finally {
      setLoading(false)
    }
  }

  const refreshLive = async (row) => {
    setLoading(true)
    try {
      if (!row.enabled) {
        const name = (row.remark || '').replace(/^主播:\s*/, '')
        await window.desktop.cli('add-live', row.url, name ? `主播: ${name}` : '', row.quality || '')
      }
      await window.desktop.cli('check-live', row.url)
      await ensureStarted()
      message.success('已刷新，后台将在下一轮循环检测该直播间')
      await fetchStatus()
    } catch (e) {
      message.error(String(e.message || e))
    } finally {
      setLoading(false)
    }
  }

  const toggleLive = async (row, checked) => {
    setLoading(true)
    try {
      const name = (row.remark || '').replace(/^主播:\s*/, '')
      if (checked) {
        await window.desktop.cli('add-live', row.url, name ? `主播: ${name}` : '', row.quality || '')
        message.success('已开启监控')
      } else {
        await window.desktop.cli('remove-live', row.url)
        message.success('已暂停监控')
      }
      await fetchStatus()
    } catch (e) {
      message.error(String(e.message || e))
    } finally {
      setLoading(false)
    }
  }

  const deleteLive = async (url) => {
    setLoading(true)
    try {
      await window.desktop.cli('delete-live', url)
      message.success('已从列表删除，不再关注该主播')
      await fetchStatus()
    } catch (e) {
      message.error(String(e.message || e))
    } finally {
      setLoading(false)
    }
  }

  const removeVideo = async (url) => {
    setLoading(true)
    try {
      await window.desktop.cli('remove-video', url)
      message.success('已删除，不再关注该博主')
      await fetchStatus()
    } catch (e) {
      message.error(String(e.message || e))
    } finally {
      setLoading(false)
    }
  }

  const openHistory = async (row) => {
    setHistoryBlogger(row)
    setHistoryVideos([])
    setHistorySelected([])
    setHistoryOpen(true)
    setHistoryLoading(true)
    try {
      const r = await window.desktop.cli('blogger-videos', row.url, '0')
      const list = await Promise.all((r.videos || []).map(async (v) => {
        const coverCache = v.cover_cache || ''
        const coverUrl = coverCache ? (await window.desktop.coverUrl(coverCache)) : ''
        return {
          ...v,
          key: v.url,
          date: v.date || v.published_at || '',
          cover_url: coverUrl || '',
        }
      }))
      setHistoryVideos(list)
      if (r.warning) message.warning(r.warning)
      else if (!list.length) message.warning('没拉到视频，可能该地址不是博主主页，或需要在设置里先做 B站授权')
    } catch (e) {
      message.error(String(e.message || e))
    } finally {
      setHistoryLoading(false)
    }
  }

  const openNotifyPanel = async () => {
    setNotifyOpen(true)
    setNotifyLoading(true)
    try {
      const r = await window.desktop.cli('notify-list', 'failed', '50')
      setNotifyEvents(r.notify || [])
    } catch (e) {
      message.error(String(e.message || e))
    } finally {
      setNotifyLoading(false)
    }
  }

  const reloadNotifyPanel = async () => {
    setNotifyLoading(true)
    try {
      const r = await window.desktop.cli('notify-list', 'failed', '50')
      setNotifyEvents(r.notify || [])
      await fetchStatus()
    } catch (e) {
      message.error(String(e.message || e))
    } finally {
      setNotifyLoading(false)
    }
  }

  const retryFailedNotify = async () => {
    setNotifyBusy(true)
    try {
      const r = await window.desktop.cli('notify-retry', '0')
      message.success(`已把 ${r.retried || 0} 条失败通知放回待发送队列，下一轮会自动重试`)
      await reloadNotifyPanel()
    } catch (e) {
      message.error(String(e.message || e))
    } finally {
      setNotifyBusy(false)
    }
  }

  const pruneFailedNotify = async () => {
    setNotifyBusy(true)
    try {
      const r = await window.desktop.cli('notify-prune', 'failed', '0')
      message.success(`已清理 ${r.removed || 0} 条失败通知记录`)
      await reloadNotifyPanel()
    } catch (e) {
      message.error(String(e.message || e))
    } finally {
      setNotifyBusy(false)
    }
  }

  const pruneSentNotify = async () => {
    setNotifyBusy(true)
    try {
      const r = await window.desktop.cli('notify-prune', 'sent', '0')
      message.success(`已清理 ${r.removed || 0} 条已发送通知记录`)
      await reloadNotifyPanel()
    } catch (e) {
      message.error(String(e.message || e))
    } finally {
      setNotifyBusy(false)
    }
  }

  const onBatchBrowse = async () => {
    const v = await batchForm.validateFields()
    setBatchOpen(false)
    await openHistory({ url: v.url.trim(), name: v.name?.trim() || '批量下载' })
    batchForm.resetFields()
  }

  const downloadSelected = async () => {
    if (!historySelected.length) return message.warning('请先勾选要下载的视频')
    setHistoryDownloading(true)
    const name = historyBlogger?.name || '批量下载'
    try {
      const items = historySelected.map((url) => ({ url, name }))
      const r = await window.desktop.cli('download-videos', JSON.stringify(items))
      const feedback = downloadQueueFeedback(r, { name, paths })
      message[feedback.type](feedback.text)
      await fetchStatus()
      await openHistory(historyBlogger)
    } catch (e) {
      message.error(String(e.message || e))
    } finally {
      setHistoryDownloading(false)
    }
  }

  const downloadAll = async () => {
    const todo = historyVideos.filter((v) => !v.downloaded).map((v) => v.url)
    if (!todo.length) return message.warning('没有未下载的视频')
    setHistoryDownloading(true)
    const name = historyBlogger?.name || '批量下载'
    try {
      const items = todo.map((url) => ({ url, name }))
      const r = await window.desktop.cli('download-videos', JSON.stringify(items))
      const feedback = downloadQueueFeedback(r, { name, paths })
      message[feedback.type](feedback.text)
      await fetchStatus()
      await openHistory(historyBlogger)
    } catch (e) {
      message.error(String(e.message || e))
    } finally {
      setHistoryDownloading(false)
    }
  }

  const toggleService = async (start) => {
    setLoading(true)
    try {
      await window.desktop.cli(start ? 'start' : 'stop')
      message.success(start ? '服务已启动' : '服务已停止')
      await fetchStatus()
    } catch (e) {
      message.error(String(e.message || e))
    } finally {
      setLoading(false)
    }
  }

  const serviceRunning = !!(
    status?.rust_daemon ||
    (status?.services?.live_monitor?.running && status?.services?.video_monitor?.running)
  )


  const svc = {
    live_monitor: { running: serviceRunning },
    video_monitor: { running: serviceRunning },
  }

  const tabTitles = {
    live: '直播监控',
    video: '短视频监控',
    logs: '运行日志',
  }

  const tabHints = {
    live: '每个主播一张卡片，点「主播设置」改画质、通知、删除',
    video: '每个博主一张卡片，点「博主设置」改下载、通知、历史视频、删除',
    logs: '直播/视频监控后台日志，排查报错用',
  }

  return (
    <div className="app-shell">
      <aside className="sidebar">
        <div className="brand-block">
          <img className="brand-logo" src={appIcon} alt="尊者直播监控" />
          <div>
            <div className="brand-title">尊者直播监控</div>
            <div className="brand-sub">界面 v2 · 卡片版</div>
          </div>
        </div>
        <button type="button" className={`nav-item ${mainTab === 'live' ? 'active' : ''}`} onClick={() => setMainTab('live')}>
          <VideoCameraFilled /> 直播监控 <span className="nav-badge">{rows.length}</span>
        </button>
        <button type="button" className={`nav-item ${mainTab === 'video' ? 'active' : ''}`} onClick={() => setMainTab('video')}>
          <CloudDownloadOutlined /> 视频监控 <span className="nav-badge">{videoRows.length}</span>
        </button>
        <button type="button" className={`nav-item ${mainTab === 'logs' ? 'active' : ''}`} onClick={() => setMainTab('logs')}>
          <UnorderedListOutlined /> 运行日志
        </button>
        <div className="sidebar-foot">
          <Button block icon={<ControlFilled />} onClick={openSettings}>全局设置</Button>
          {/* 两个按钮必须一眼分得清：产物是用户自己的文件，程序是安装目录。
              以前叫「打开保存目录 / 打开数据目录」，用户点了后者跳到程序目录，
              看不出是故意的还是出错——名字直接说清就好了。 */}
          <Button block icon={<CloudSyncOutlined />} loading={updateBusy} onClick={checkUpdate}>检查更新</Button>
          <div className="app-version">界面 v2 · v{APP_VERSION} · 构建于 {BUILD_LABEL}</div>
        </div>
      </aside>

      <main className="main-panel">
        <header className="top-bar">
          <div className="top-row">
            <div>
              <h1 className="top-title">{tabTitles[mainTab]}</h1>
              <p className="top-desc">{tabHints[mainTab]}</p>
            </div>
            <div className="top-actions">
              {mainTab === 'live' && (
                <Button type="primary" icon={<PlusCircleFilled />} onClick={() => setAddOpen(true)}>添加主播</Button>
              )}
              {mainTab === 'video' && (
                <>
                  <Button type="primary" icon={<VideoCameraFilled />} onClick={() => setVideoOpen(true)}>添加博主</Button>
                  <Button icon={<CloudDownloadOutlined />} onClick={() => setBatchOpen(true)}>批量下载</Button>
                </>
              )}
              <Button icon={serviceRunning ? <StopFilled /> : <PlayCircleFilled />}
                type={serviceRunning ? 'default' : 'primary'}
                onClick={() => toggleService(!serviceRunning)}>
                {serviceRunning ? '停止服务' : '启动服务'}
              </Button>
              <Button icon={<ReloadOutlined />} loading={loading} onClick={() => refresh({ showLoading: true })}>刷新数据</Button>
            </div>
          </div>
          <div className="status-pills">
            <span className={`status-pill ${svc.live_monitor?.running ? 'on' : 'off'}`}>
              <i className={`dot ${svc.live_monitor?.running ? 'on' : ''}`} />直播监控 {svc.live_monitor?.running ? '运行中' : '未运行'}
            </span>
            <span className={`status-pill ${svc.video_monitor?.running ? 'on' : 'off'}`}>
              <i className={`dot ${svc.video_monitor?.running ? 'on' : ''}`} />视频监控 {svc.video_monitor?.running ? '运行中' : '未运行'}
            </span>
            <span className={`status-pill ${status?.source_health?.ok ? 'on' : status?.source_health?.ok === false ? 'off' : ''}`} title="平台风控或网络故障会让源进入冷却，冷却期间新视频不会通知/下载">
              <i className={`dot ${status?.source_health?.ok ? 'on' : ''}`} />监控源 {status?.source_health?.ok === true
                ? '健康'
                : status?.source_health?.ok === false
                  ? `异常（${status?.source_health?.cooling ?? '?'} 个冷却中）`
                  : '未知'}
            </span>
            <button type="button" className={`status-pill status-pill-btn ${status?.notify_health?.ok ? 'on' : status?.notify_health?.ok === false ? 'off' : ''}`}
              title="点击查看失败通知，可重试或清理" onClick={openNotifyPanel}>
              <i className={`dot ${status?.notify_health?.ok ? 'on' : ''}`} />通知 {status?.notify_health?.ok === true
                ? '正常'
                : status?.notify_health?.ok === false
                  ? `异常（${status?.notify_health?.failed ?? '?'} 失败）`
                  : '未知'}
            </button>
          </div>
        </header>

        <div className="content">
          {mainTab === 'live' && (
            rows.length ? (
              <div className="card-grid">{rows.map((r) => (
                <LiveCard key={r.key} row={r} platformIcon={platformIcon} platformLabel={platformLabel}
                  onToggle={toggleLive} onSettings={openItemSettings}
                  onOpenStash={openItemStash} onDetail={openItemDetail} />
              ))}</div>
            ) : (
              <div className="empty-wrap">
                <Empty description="还没有主播，点右上角「添加主播」开始监控" />
              </div>
            )
          )}
          {mainTab === 'video' && (
            videoRows.length ? (
              <div className="card-grid">{videoRows.map((r) => (
                <VideoCard key={r.key} row={r} platformIcon={platformIcon} platformLabel={platformLabel}
                  onSettings={openItemSettings}
                  onOpenStash={openItemStash} onDetail={openItemDetail} />
              ))}</div>
            ) : (
              <div className="empty-wrap">
                <Empty description="还没有博主，点「添加博主」监控新视频" />
              </div>
            )
          )}
          {mainTab === 'logs' && (
            <LogsPanel logDir={status?.paths?.logs} logPaths={status?.log_paths} />
          )}
        </div>
      </main>

      <SetupWizard open={wizardOpen} onDone={() => setWizardOpen(false)} />

      <Modal title="添加直播监控" open={addOpen} onOk={onAddLive} onCancel={() => setAddOpen(false)} okText="确认并添加" width={700} destroyOnClose>
        <Form form={addForm} layout="horizontal" labelCol={{ span: 6 }} wrapperCol={{ span: 16 }} initialValues={{ providerId: 'Auto', quality: '原画', notify: '跟随全局' }}>
          <Form.Item label="平台" name="providerId" rules={[{ required: true }]}>
            <Select options={platformOptions} />
          </Form.Item>
          <Form.Item label="链接/频道ID" name="channelId" rules={[{ required: true, message: '请输入' }]}
            extra="底层支持 40+ 平台，直接粘贴直播间链接；智能体请通过 MCP/CLI 连接，不必在此配置大模型">
            <Input placeholder="例如 https://live.bilibili.com/21686237" />
          </Form.Item>
          <Form.Item label="主播名/备注" name="remarks" rules={[{ required: true, message: '请输入' }]}>
            <Input placeholder="方便识别的名字" />
          </Form.Item>
          <Form.Item label="录制画质" name="quality">
            <Select options={qualityOptions} />
          </Form.Item>
          <Form.Item label="通知方式" name="notify"
            extra="重要的选「微信」；不重要的选「邮箱」或「不通知」，省 Server酱 每天 5 条额度">
            <Select options={notifyOptions} />
          </Form.Item>
        </Form>
      </Modal>

      <Modal title="添加发布视频监控" open={videoOpen} onOk={onAddVideo} onCancel={() => setVideoOpen(false)} okText="确认并添加" width={700} destroyOnClose>
        <Form form={videoForm} layout="horizontal" labelCol={{ span: 6 }} wrapperCol={{ span: 16 }} initialValues={{ autoDownload: true, notify: '跟随全局' }}>
          <Form.Item label="博主名称" name="name" rules={[{ required: true }]}>
            <Input placeholder="例如 影视飓风" />
          </Form.Item>
          <Form.Item label="监控地址" name="url" rules={[{ required: true }]}
            extra="B站填 space.bilibili.com/数字；抖音/快手可用 RSSHub 订阅地址">
            <Input placeholder="https://space.bilibili.com/946974" />
          </Form.Item>
          <Form.Item label="自动下载新视频" name="autoDownload" valuePropName="checked"
            extra="开：发现新视频立即下载；关：只通知不下载">
            <Switch />
          </Form.Item>
          <Form.Item label="通知方式" name="notify"
            extra="重要的选「微信」；一般博主选「邮箱」或「不通知」">
            <Select options={notifyOptions} />
          </Form.Item>
        </Form>
      </Modal>

      <Modal
        title={itemSettingsType === 'live'
          ? `主播设置 — ${(itemSettingsRow?.remark || '').replace(/^主播:\s*/, '') || ''}`
          : `博主设置 — ${itemSettingsRow?.name || ''}`}
        open={itemSettingsOpen}
        onCancel={() => setItemSettingsOpen(false)}
        onOk={saveItemSettings}
        okText="保存设置"
        cancelText="取消"
        width={560}
        destroyOnClose
      >
        <Form form={itemSettingsForm} layout="vertical">
          {itemSettingsType === 'live' ? (
            <>
              <Form.Item label="主播名称" name="name" rules={[{ required: true, message: '请输入名称' }]}>
                <Input placeholder="例如 影视飓风" />
              </Form.Item>
              <Form.Item label="直播间地址" name="url">
                <Input readOnly />
              </Form.Item>
              <Form.Item label="开启监控" name="enabled" valuePropName="checked"
                extra="关闭后不再检测开播，但保留在列表中（也可下方直接删除）">
                <Switch />
              </Form.Item>
              <Form.Item label="录制画质" name="quality">
                <Select options={qualityOptions} />
              </Form.Item>
              <Form.Item label="通知方式" name="notify"
                extra="重要的选「微信」（占 Server酱 每天约 5 条）；一般选「不通知」">
                <Select options={notifyOptions} />
              </Form.Item>
              <Button icon={<ReloadOutlined />} onClick={() => itemSettingsRow && refreshLive(itemSettingsRow)}>
                立即刷新检测
              </Button>
            </>
          ) : (
            <>
              <Form.Item label="博主名称" name="name" rules={[{ required: true, message: '请输入名称' }]}>
                <Input placeholder="例如 影视飓风" />
              </Form.Item>
              <Form.Item label="监控地址" name="url">
                <Input readOnly />
              </Form.Item>
              <Form.Item label="发现新视频自动下载" name="autoDownload" valuePropName="checked"
                extra={`开：自动下载到 ${paths.video_root || paths.media_root || "<产物根目录>"}/${itemSettingsRow?.name || "博主名"}/短视频/ 并转写；关：只通知`}>
                <Switch />
              </Form.Item>
              <Form.Item label="通知方式" name="notify">
                <Select options={notifyOptions} />
              </Form.Item>
              <Button type="link" onClick={() => { setItemSettingsOpen(false); openHistory(itemSettingsRow) }}>
                查看历史视频 / 批量下载
              </Button>
            </>
          )}
        </Form>
        {/* 「东西存哪儿了」——用户要求直接写在设置里，点了就能去。
            这里显示的是按 Rust 落盘规则算出来的真实路径，不是占位文案。 */}
        <div className="stash-box">
          <div className="stash-box-title">
            {itemSettingsType === 'live' ? '录像存到这里' : '视频与文字稿存到这里'}
          </div>
          <div className="stash-box-path"
            title="点击打开这个文件夹"
            onClick={() => itemSettingsRow && openItemStash(itemSettingsRow, itemSettingsType)}>
            {stashOf(itemSettingsRow, itemSettingsType) || '（还没解析出存放位置，请先在全局设置里确认产物根目录）'}
          </div>
          <Space size={8} wrap>
            <Button size="small" icon={<FolderOpenFilled />}
              onClick={() => itemSettingsRow && openItemStash(itemSettingsRow, itemSettingsType)}>
              打开这个文件夹
            </Button>
            <Button size="small"
              onClick={() => { setItemSettingsOpen(false); openItemDetail(itemSettingsRow, itemSettingsType) }}>
              看里面有什么
            </Button>
          </Space>
          <div className="stash-box-hint">
            {itemSettingsType === 'live'
              ? '按主播名分开存放：主播名 / 直播 /'
              : '按博主名分开存放：博主名 / 短视频 /'}
          </div>
        </div>
        <div style={{ marginTop: 24, paddingTop: 16, borderTop: '1px solid #f0f0f0' }}>
          <Popconfirm
            title={itemSettingsType === 'live' ? '确定删除该主播？将从列表移除，不再监控' : '确定删除该博主？不再关注'}
            onConfirm={deleteFromSettings}
          >
            <Button danger icon={<DeleteFilled />}>删除，不再关注</Button>
          </Popconfirm>
        </div>
      </Modal>

      {/* 点一个主播/博主 → 进来看他的东西。
          地址、存放位置、文件清单，每一项都是真能点的：地址开浏览器，
          文件夹进访达，文件用系统默认程序打开。不留「假的碰都碰不了」的展示位。 */}
      <Modal
        title={detailType === 'live'
          ? `主播内容 — ${rowDisplayName(detailRow, 'live') || (detailRow && detailRow.url) || ''}`
          : `博主内容 — ${(detailRow && detailRow.name) || ''}`}
        open={detailOpen}
        onCancel={() => setDetailOpen(false)}
        footer={null}
        width={720}
        destroyOnClose
      >
        <div className="detail-block">
          <div className="detail-label">监控地址</div>
          <a className="detail-link" onClick={() => detailRow && window.desktop.openExternal(detailRow.url)}>
            {detailRow && detailRow.url}
          </a>
          <div className="detail-hint">点击用浏览器打开</div>
        </div>

        <div className="detail-block">
          <div className="detail-label">存放位置</div>
          <a className="detail-link" onClick={() => detailRow && openItemStash(detailRow, detailType)}>
            {detailPath || stashOf(detailRow, detailType) || '（未解析）'}
          </a>
          <Space size={8} style={{ marginTop: 6 }} wrap>
            <Button size="small" icon={<FolderOpenFilled />}
              onClick={() => detailRow && openItemStash(detailRow, detailType)}>打开文件夹</Button>
            <Button size="small"
              onClick={() => { const p = detailPath || stashOf(detailRow, detailType); if (p) window.desktop.showItem(p) }}>
              在访达中定位
            </Button>
          </Space>
        </div>

        <div className="detail-block">
          <div className="detail-label">文字稿与复盘</div>
          <a className="detail-link" onClick={() => detailRow && openItemStash(detailRow, detailType)}>
            {stashOf(detailRow, detailType) || '（未解析）'}
          </a>
          <div className="detail-hint">转写文字稿落在该主播自己的文件夹里（与视频同目录），不是全局目录</div>
          <Space size={8} style={{ marginTop: 6 }} wrap>
            <Button size="small" icon={<FolderOpenFilled />}
              onClick={() => detailRow && openItemStash(detailRow, detailType)}>打开文字稿文件夹</Button>
          </Space>
        </div>

        <div className="detail-block">
          <div className="detail-label">
            里面的文件
            <Tag style={{ marginLeft: 8 }}>{detailFiles.length}</Tag>
            <Button size="small" type="link" loading={detailLoading}
              onClick={() => detailRow && loadDetailFiles(detailRow, detailType)}>刷新</Button>
          </div>
          {detailLoading && !detailFiles.length ? (
            <div className="detail-empty">正在读取…</div>
          ) : detailFiles.length ? (
            <div className="file-list">
              {detailFiles.map((f) => {
                const task = processTaskForFile(status?.process_tasks, f.path)
                const taskLabel = processTaskLabel(task)
                const taskAction = processTaskAction(task)
                return (
                  <div className="file-row" key={f.path || f.name}>
                    <span className="file-icon">
                      {f.dir ? <FolderFilled style={{ color: '#fbbf24' }} /> : <VideoCameraFilled style={{ color: '#818cf8' }} />}
                    </span>
                    <span className="file-name" title="点击用默认程序打开"
                      onClick={() => window.desktop.openPath(f.path)}>{f.name}</span>
                    <span className="file-size">{f.dir ? '文件夹' : humanSize(f.size)}</span>
                    <span className="file-time">{stamp(f.mtime)}</span>
                    {!f.dir && taskLabel && (
                      <Tooltip title={task?.last_error || ''}>
                        <Tag color={task?.status === 'failed' ? 'error' : task?.status === 'done' ? 'success' : 'processing'}>
                          {taskLabel}
                        </Tag>
                      </Tooltip>
                    )}
                    {!f.dir && isTranscribableMedia(f.name) && (
                      <Button size="small" type="primary" ghost
                        loading={detailQueueing === f.path}
                        disabled={taskAction.disabled}
                        onClick={() => queueTranscription(f)}>{taskAction.label}</Button>
                    )}
                    <Button size="small" type="link"
                      onClick={() => window.desktop.showItem(f.path)}>在访达中显示</Button>
                  </div>
                )
              })}
            </div>
          ) : (
            <div className="detail-empty">
              {detailError
                ? `读不到目录：${detailError}`
                : (detailType === 'live'
                  ? '这个主播还没有录像文件。开播并录过一次之后，文件会出现在上面这个文件夹里。'
                  : '还没有下载过视频。打开「发现新视频自动下载」后，下载的文件会出现在上面这个文件夹里。')}
            </div>
          )}
        </div>
      </Modal>

      <Modal
        title="全局设置"
        open={settingsOpen}
        onCancel={() => setSettingsOpen(false)}
        width={680}
        styles={{ body: { maxHeight: '70vh', overflow: 'auto' } }}
        footer={[
          <Button key="test" icon={<MailOutlined />} onClick={testEmail}>测试通知</Button>,
          <Button key="save" type="primary" loading={emailSaving} onClick={saveEmailSettings}>保存配置</Button>,
          <Button key="close" onClick={() => setSettingsOpen(false)}>关闭</Button>,
        ]}
      >
  <Form form={emailForm} layout="vertical" initialValues={{ smtpHost: 'smtp.qq.com', smtpPort: '465', senderName: '尊者监控直播', transcribeEnabled: true, pipelineTimeout: '21600', llmEnabled: true, watchdog: true, autoStartServices: true, localNotify: true, mediaRoot: '', downloadDir: '', fupanDir: '', logsDir: '', databaseFile: '' }}>
          <div style={{ fontWeight: 600, margin: '4px 0 8px' }}>应用行为</div>
          <Form.Item label="打开软件时自动启动监控" name="autoStartServices" valuePropName="checked">
            <Switch />
          </Form.Item>
          <Form.Item label="服务崩溃自动重启（守护）" name="watchdog" valuePropName="checked">
            <Switch />
          </Form.Item>
          <Form.Item label="开机自动启动（登录后托盘运行）" name="openAtLogin" valuePropName="checked">
            <Switch />
          </Form.Item>
          <Form.Item label="Windows 桌面通知（远程推送失败时的兜底）" name="localNotify" valuePropName="checked">
            <Switch />
          </Form.Item>

          <div style={{ fontWeight: 600, margin: '16px 0 8px' }}>存放位置</div>
          <div style={{ marginBottom: 10, padding: '8px 12px', borderRadius: 6, background: '#f6ffed', border: '1px solid #b7eb8f', fontSize: 13 }}>
            产物（直播录像、下载的短视频、转写文本、切片）和程序是分开的，默认不放在程序目录里。
          </div>
          <Form.Item label="产物根目录" name="mediaRoot"
            extra={<span>留空 = 默认位置 <code>~/Documents/尊者产物</code>。录像、下载、转写全部落在它下面。</span>}>
            <Input placeholder="留空用默认位置"
              addonAfter={<a onClick={() => pickStashDir('mediaRoot')} style={{ cursor: 'pointer' }}>选择…</a>} />
          </Form.Item>
          <Form.Item label="短视频下载目录" name="downloadDir"
            extra="留空 = <产物根目录>（新下载落在 <该目录>/博主名/短视频/）">
            <Input placeholder="留空自动跟随产物根目录"
              addonAfter={<a onClick={() => pickStashDir('downloadDir')} style={{ cursor: 'pointer' }}>选择…</a>} />
          </Form.Item>
          <Form.Item label="复盘输出目录" name="fupanDir"
            extra="留空 = <产物根目录>/fupan_output（转写、知识点、切片、封面都在这下面）">
            <Input placeholder="留空自动跟随产物根目录"
              addonAfter={<a onClick={() => pickStashDir('fupanDir')} style={{ cursor: 'pointer' }}>选择…</a>} />
          </Form.Item>
          <Form.Item label="日志目录" name="logsDir"
            extra="留空 = <产物根目录>/logs。运行日志和心跳记录可单独换到别的盘。">
            <Input placeholder="留空使用 <产物根目录>/logs"
              addonAfter={<a onClick={() => pickStashDir('logsDir')} style={{ cursor: 'pointer' }}>选择…</a>} />
          </Form.Item>
          <Form.Item label="数据库文件" name="databaseFile"
            extra="留空 = <产物根目录>/database/zunzhe-core.db；已有旧库会继续沿用，不会自动覆盖或搬动数据。修改后需重启后台服务才生效。">
            <Input placeholder="留空使用 <产物根目录>/database/zunzhe-core.db"
              addonAfter={<a onClick={pickDatabaseFile} style={{ cursor: 'pointer' }}>选择…</a>} />
          </Form.Item>
          <div style={{ marginBottom: 4 }}>
            <Button size="small" icon={<FolderOpenFilled />}
              onClick={() => window.desktop.openPath(status?.paths?.media_root || '')}>打开产物目录</Button>
            <Button size="small" icon={<FolderOpenFilled />} style={{ marginLeft: 8 }}
              onClick={() => window.desktop.openPath(status?.paths?.logs || '')}>打开日志目录</Button>
            <Button size="small" icon={<FolderOpenFilled />} style={{ marginLeft: 8 }}
              onClick={() => window.desktop.openPath(status?.paths?.database || '')}>打开数据库位置</Button>
            <span style={{ marginLeft: 10, color: '#888', fontSize: 12 }}>
              当前：{status?.paths?.media_root || '-'} · 数据库：{status?.paths?.database || '-'}
            </span>
          </div>

          <div style={{ fontWeight: 600, margin: '16px 0 8px' }}>抗代理推送（推荐）</div>
          <Form.Item label="Server酱 SendKey" name="serverChanKey"
            extra={<span>在 <a onClick={() => window.desktop.openExternal('https://sct.ftqq.com/')}>sct.ftqq.com</a> 微信登录后复制 SendKey。已配置可留空。</span>}>
            <Input.Password placeholder="SCTxxxxxxxx..." />
          </Form.Item>
          <Form.Item label="PushPlus Token" name="pushplusToken">
            <Input.Password placeholder="留空则不启用" />
          </Form.Item>

          <div style={{ fontWeight: 600, margin: '16px 0 8px' }}>邮箱推送</div>
          <div style={{ marginBottom: 8, padding: '8px 12px', borderRadius: 6, background: emailConfigured ? '#f6ffed' : '#fff7e6', border: `1px solid ${emailConfigured ? '#b7eb8f' : '#ffd591'}` }}>
            {emailConfigured ? '✅ 邮箱已配置' : `⚠️ ${emailStatusText || '邮箱未配置'}`}
            <Button size="small" style={{ marginLeft: 12 }} onClick={setupQqMail}>QQ 邮箱一键填入</Button>
          </div>
          <Form.Item label="SMTP 服务器" name="smtpHost"><Input placeholder="smtp.qq.com" /></Form.Item>
          <Form.Item label="SMTP 端口" name="smtpPort"><Input placeholder="465" /></Form.Item>
          <Form.Item label="发件邮箱" name="loginEmail"><Input placeholder="xxx@qq.com" /></Form.Item>
          <Form.Item label="SMTP 授权码" name="authCode" extra={
            <span>QQ 邮箱获取：登录 <a onClick={() => window.desktop.openExternal('https://mail.qq.com/')}>mail.qq.com</a> → 设置 → 账户 → 开启「SMTP 服务」→ 按提示生成授权码（不是 QQ 密码）。已配置过可留空。</span>
          }>
            <Input.Password placeholder="16 位授权码，如 abcdefghijklmnop" />
          </Form.Item>
          <Form.Item label="发件人显示名" name="senderName"><Input /></Form.Item>
          <Form.Item label="收件邮箱" name="toEmail"><Input /></Form.Item>

          <div style={{ fontWeight: 600, margin: '16px 0 8px' }}>转写（本地 whisper，不接任何大模型 API）</div>
          <Form.Item label="录制/下载完成后自动转写" name="transcribeEnabled" valuePropName="checked">
            <Switch />
          </Form.Item>
          <Form.Item label="Whisper 模型" name="whisperModel">
            <Select options={[
              { label: 'tiny（最快）', value: 'tiny' },
              { label: 'base（推荐）', value: 'base' },
              { label: 'small', value: 'small' },
              { label: 'medium', value: 'medium' },
            ]} />
          </Form.Item>
          <Form.Item label="长视频转写超时（秒）" name="pipelineTimeout"
            extra="默认 21600（6 小时）。纯 CPU 转写长视频会接近 1 小时，建议保留充足余量。">
            <Input placeholder="21600" />
          </Form.Item>
        </Form>

        <div style={{ fontWeight: 600, margin: '8px 0' }}>B站授权</div>
        <Button onClick={biliLogin}>打开 B站 登录 / 授权</Button>

        <div style={{ fontWeight: 600, margin: '16px 0 8px' }}>抖音授权</div>
        <Button onClick={douyinLogin}>打开抖音 扫码登录（短视频监控用）</Button>
        <div style={{ fontSize: 12, color: '#999', marginTop: 4 }}>登录后自动保存，用于监控/下载抖音短视频；约一个月后需重新扫码</div>
      </Modal>

      <Modal title="批量下载视频" open={batchOpen} onOk={onBatchBrowse} onCancel={() => setBatchOpen(false)} okText="列出历史视频" width={640} destroyOnClose>
        <p style={{ marginBottom: 12, color: '#666' }}>
          输入博主主页地址，列出其历史视频后勾选批量下载（无需先加入监控）。
        </p>
        <Form form={batchForm} layout="vertical">
          <Form.Item label="博主名称" name="name">
            <Input placeholder="用于文件夹命名，如 影视飓风" />
          </Form.Item>
          <Form.Item label="博主主页地址" name="url" rules={[{ required: true }]}
            extra="B站填 space.bilibili.com/数字；其他平台填 RSS 订阅地址">
            <Input placeholder="https://space.bilibili.com/946974" />
          </Form.Item>
        </Form>
      </Modal>

      <Modal
        title={`历史视频 - ${historyBlogger?.name || ''}`}
        open={historyOpen}
        onCancel={() => setHistoryOpen(false)}
        width={860}
        footer={[
          <span key="tip" style={{ float: 'left', color: '#999', fontSize: 12, lineHeight: '32px' }}>
            已选 {historySelected.length} / {historyVideos.length}
          </span>,
          <Button key="close" onClick={() => setHistoryOpen(false)}>关闭</Button>,
          <Button key="all" loading={historyDownloading} disabled={!historyVideos.some((v) => !v.downloaded)} onClick={downloadAll}>
            下载全部（未下载）
          </Button>,
          <Button key="dl" type="primary" loading={historyDownloading} disabled={!historySelected.length} onClick={downloadSelected}>
            下载选中（{historySelected.length}）
          </Button>,
        ]}
      >
        <p style={{ marginBottom: 12, color: '#666' }}>
          勾选想要的视频，点「下载选中」批量保存到 <b>{paths.video_root || paths.media_root || "产物根目录"}/{historyBlogger?.name || "博主名"}/短视频/</b> 并自动转写复盘。
        </p>
        <Table
          rowKey="key"
          loading={historyLoading}
          dataSource={historyVideos}
          pagination={false}
          size="small"
          scroll={{ y: 460 }}
          rowSelection={{
            selectedRowKeys: historySelected,
            onChange: (keys) => setHistorySelected(keys),
            getCheckboxProps: (r) => ({ disabled: !!r.downloaded }),
          }}
          columns={[
            { title: '封面', dataIndex: 'cover_cache', width: 100, align: 'center',
              render: (c, r) => (r.cover_url
                ? <img src={r.cover_url} alt="" style={{ width: 96, height: 54, objectFit: 'cover', borderRadius: 4 }} />
                : <a onClick={() => r.cover && window.desktop.openExternal(r.cover)} style={{ color: '#999' }}>无缓存</a>) },
            { title: '标题', dataIndex: 'title', ellipsis: true,
              render: (t, r) => (
                <span>
                  <a onClick={() => window.desktop.openExternal(r.url)}>{t}</a>
                  {r.downloaded ? <span style={{ marginLeft: 8, color: '#52c41a', fontSize: 12 }}>✓ 已下载</span> : null}
                </span>
              ) },
            { title: '发布日期', dataIndex: 'date', width: 150, align: 'center' },
            { title: '时长', dataIndex: 'duration', width: 80, align: 'center',
              render: (d) => (d ? `${Math.floor(d / 60)}:${String(d % 60).padStart(2, '0')}` : '-') },
          ]}
        />
      </Modal>

      <Modal
        title="通知治理"
        open={notifyOpen}
        onCancel={() => setNotifyOpen(false)}
        width={880}
        footer={[
          <Button key="close" onClick={() => setNotifyOpen(false)}>关闭</Button>,
          <Popconfirm key="prune-sent" title="清理所有已发送通知记录？只影响历史列表，不影响已送达" onConfirm={pruneSentNotify}>
            <Button icon={<ClearOutlined />} loading={notifyBusy}>清理已发送</Button>
          </Popconfirm>,
          <Popconfirm key="prune-failed" title="清理所有失败通知记录？将不再保留这些失败提醒" onConfirm={pruneFailedNotify}>
            <Button danger icon={<DeleteFilled />} loading={notifyBusy}>清理失败记录</Button>
          </Popconfirm>,
          <Button key="retry" type="primary" icon={<RollbackOutlined />} loading={notifyBusy} onClick={retryFailedNotify}>
            重试全部失败通知
          </Button>,
        ]}
      >
        <p style={{ marginBottom: 12, color: '#666' }}>
          这里显示最近 50 条失败通知。配置邮箱/微信补齐后点「重试全部失败通知」；
          不需要的历史失败记录可以清理，避免通知健康一直显示异常。
        </p>
        {status?.notify_health?.hint ? (
          <div style={{
            marginBottom: 12,
            padding: '10px 12px',
            borderRadius: 8,
            background: status?.notify_health?.ok ? '#f6ffed' : '#fff1f0',
            border: `1px solid ${status?.notify_health?.ok ? '#b7eb8f' : '#ffccc7'}`,
            color: '#333',
          }}>
            <strong>诊断建议：</strong> {status?.notify_health?.hint}
          </div>
        ) : null}
        <Table
          rowKey="id"
          loading={notifyLoading}
          dataSource={notifyEvents}
          pagination={false}
          size="small"
          scroll={{ y: 380 }}
          columns={[
            { title: 'ID', dataIndex: 'id', width: 70 },
            { title: '事件', dataIndex: 'event', width: 110,
              render: (v) => (v === 'live' ? '开播' : v === 'video' ? '新视频' : v || '-') },
            { title: '主播/博主', dataIndex: 'anchor', width: 140, ellipsis: true },
            { title: '标题', dataIndex: 'title', ellipsis: true },
            { title: '渠道', dataIndex: 'channel', width: 100,
              render: (v) => <Tag color={v === '微信' ? 'green' : v === '邮箱' ? 'blue' : 'default'}>{v || '未知'}</Tag> },
            { title: '状态', dataIndex: 'status', width: 90,
              render: (v) => <Tag color={v === 'failed' ? 'red' : v === 'sent' ? 'green' : 'orange'}>{v}</Tag> },
            { title: '重试', dataIndex: 'attempts', width: 70, align: 'center' },
            { title: '时间', dataIndex: 'created_at', width: 160 },
          ]}
          locale={{ emptyText: '没有失败通知记录，通知链路健康' }}
        />
      </Modal>
    </div>
  )
}
