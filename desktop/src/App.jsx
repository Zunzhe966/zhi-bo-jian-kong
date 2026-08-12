import React, { useCallback, useEffect, useMemo, useState } from 'react'
import {
  Badge, Button, Empty, Form, Input, InputNumber, message, Modal,
  Popconfirm, Select, Space, Switch, Table, Tooltip,
} from 'antd'
import {
  ControlFilled, DeleteFilled, FolderOpenFilled, MailOutlined,
  PlayCircleFilled, PlusCircleFilled, ReloadOutlined, SettingOutlined, StopFilled,
  VideoCameraFilled, CloudDownloadOutlined,   FileTextOutlined, UnorderedListOutlined,
} from '@ant-design/icons'
import douyinIcon from '../assets/douyin.png'
import kuaishouIcon from '../assets/kuaishou.png'
import appIcon from '../assets/icon.png'
import { LiveCard, VideoCard, statusFingerprint } from './PersonCards'
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
  { label: '自动识别（推荐，支持40+平台）', value: 'Auto' },
  { label: '抖音', value: 'DouYin' },
  { label: '快手', value: 'KuaiShou' },
  { label: 'B站', value: 'Bilibili' },
  { label: '虎牙', value: 'Huya' },
  { label: '斗鱼', value: 'Douyu' },
  { label: '小红书', value: 'Xiaohongshu' },
  { label: 'TikTok', value: 'TikTok' },
  { label: 'YouTube', value: 'Youtube' },
  { label: 'Twitch', value: 'Twitch' },
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

// 构建时间由 vite 注入，用来确认界面是否已更新
const BUILD_LABEL = typeof __APP_BUILD__ !== 'undefined' ? __APP_BUILD__ : '开发版'

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
  return (status.live_watchlist || []).map((room, idx) => {
    const platform = detectPlatform(room.url)
    const remark = (room.remark || '').replace(/^主播:\s*/, '')
    const isRecording = room.enabled && remark &&
      activeFiles.some((f) => f.includes(remark.toLowerCase()))
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
  const [notesOpen, setNotesOpen] = useState(false)
  const [noteContent, setNoteContent] = useState('')
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
  const [mainTab, setMainTab] = useState('live')
  const [wizardOpen, setWizardOpen] = useState(false)

  const pickCfg = (section, key, fallback = '') => {
    const v = section?.[key]
    if (v !== undefined && v !== null && v !== '') return v
    const lower = Object.entries(section || {}).find(([k]) => k.toLowerCase() === key.toLowerCase())
    return lower ? lower[1] : fallback
  }

  const openSettings = async () => {
    setSettingsOpen(true)
    try {
      const [cfg, fupanCfg, prefRes] = await Promise.all([
        window.desktop.cli('get-config', 'config', '推送配置'),
        window.desktop.cli('get-config', 'fupan_config'),
        window.desktop.cli('get-app-prefs'),
      ])
      const push = cfg['config.ini']?.['推送配置'] || {}
      const tx = fupanCfg['fupan_config.ini']?.['转写'] || {}
      const llm = fupanCfg['fupan_config.ini']?.['知识提炼'] || {}
      const prefs = prefRes.prefs || {}
      const mail = await window.desktop.cli('email-status')
      setEmailStatusText(mail.hint || (mail.configured ? '已配置' : '未配置'))
      setEmailConfigured(!!mail.configured)
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
        llmEnabled: pickCfg(llm, '是否启用知识提炼(是/否)', '是') !== '否',
        llmApiUrl: pickCfg(llm, 'api地址', 'https://api.deepseek.com'),
        llmApiKey: '',
        llmModel: pickCfg(llm, '模型', 'deepseek-chat'),
        watchdog: prefs.watchdog !== false,
        autoStartServices: prefs.autoStartServices !== false,
        openAtLogin: !!prefs.openAtLogin,
        localNotify: prefs.localNotify !== false,
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
      await window.desktop.cli('set-config', 'fupan_config', '知识提炼', '是否启用知识提炼(是/否)', v.llmEnabled ? '是' : '否')
      if (v.llmApiUrl?.trim()) {
        await window.desktop.cli('set-config', 'fupan_config', '知识提炼', 'api地址', v.llmApiUrl.trim())
      }
      if (v.llmApiKey?.trim()) {
        await window.desktop.cli('set-config', 'fupan_config', '知识提炼', 'api密钥', v.llmApiKey.trim())
      }
      if (v.llmModel?.trim()) {
        await window.desktop.cli('set-config', 'fupan_config', '知识提炼', '模型', v.llmModel.trim())
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
      message.success('配置已保存')
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
    () => (status?.video_watchlist || []).map((v, i) => ({ ...v, key: v.url, index: i + 1 })),
    [status?.video_watchlist],
  )
  const noteRows = useMemo(
    () => (status?.notes || []).map((n, i) => ({ ...n, key: `${n.anchor}-${n.session}`, index: i + 1 })),
    [status?.notes],
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
      const r = await window.desktop.cli('blogger-videos', row.url, '40')
      const list = (r.videos || []).map((v) => ({
        ...v,
        key: v.url,
        date: v.date || v.published_at || '',
      }))
      setHistoryVideos(list)
      if (!list.length) message.warning('没拉到视频，可能该地址不是博主主页，或需要在设置里先做 B站授权')
    } catch (e) {
      message.error(String(e.message || e))
    } finally {
      setHistoryLoading(false)
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
    let ok = 0
    let fail = 0
    for (const url of historySelected) {
      try {
        const r = await window.desktop.cli('download-video', url, name)
        r.ok ? ok++ : fail++
      } catch { fail++ }
    }
    setHistoryDownloading(false)
    message[fail ? 'warning' : 'success'](`批量下载完成：成功 ${ok}，失败 ${fail}。文件在 downloads/短视频/${name}/`)
    await fetchStatus()
    await openHistory(historyBlogger)
  }

  const downloadAll = async () => {
    const todo = historyVideos.filter((v) => !v.downloaded).map((v) => v.url)
    if (!todo.length) return message.warning('没有未下载的视频')
    setHistoryDownloading(true)
    const name = historyBlogger?.name || '批量下载'
    let ok = 0
    let fail = 0
    for (const url of todo) {
      try {
        const r = await window.desktop.cli('download-video', url, name)
        r.ok ? ok++ : fail++
      } catch { fail++ }
    }
    setHistoryDownloading(false)
    message[fail ? 'warning' : 'success'](`全部下载完成：成功 ${ok}，失败 ${fail}。文件在 downloads/短视频/${name}/`)
    await fetchStatus()
    await openHistory(historyBlogger)
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

  const openNote = async (file) => {
    const text = await window.desktop.readText(file)
    setNoteContent(text)
    setNotesOpen(true)
  }

  const noteColumns = [
    { title: '博主', dataIndex: 'anchor', width: 120 },
    { title: '场次', dataIndex: 'session', ellipsis: true },
    {
      title: '复盘产物', render: (_, r) => (
        <Space>
          {r.transcript_file && <Button size="small" onClick={() => openNote(r.transcript_file)}>转写</Button>}
          {r.notes_file && <Button size="small" type="primary" onClick={() => openNote(r.notes_file)}>笔记</Button>}
        </Space>
      ),
    },
  ]

  const svc = {
    live_monitor: { running: serviceRunning },
    video_monitor: { running: serviceRunning },
  }
  const tabTitles = { live: '直播监控', video: '发布视频监控', notes: '复盘笔记', logs: '运行日志' }
  const tabHints = {
    live: '每个主播一张卡片，点「主播设置」改画质、通知、删除',
    video: '每个博主一张卡片，点「博主设置」改下载、通知、历史视频、删除',
    notes: '已转写和复盘的内容',
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
        <button type="button" className={`nav-item ${mainTab === 'notes' ? 'active' : ''}`} onClick={() => setMainTab('notes')}>
          <FileTextOutlined /> 复盘笔记 <span className="nav-badge">{noteRows.length}</span>
        </button>
        <button type="button" className={`nav-item ${mainTab === 'logs' ? 'active' : ''}`} onClick={() => setMainTab('logs')}>
          <UnorderedListOutlined /> 运行日志
        </button>
        <div className="sidebar-foot">
          <Button block icon={<ReloadOutlined />} onClick={reloadUi}>重新加载界面</Button>
          <Button block icon={<ControlFilled />} onClick={openSettings}>全局设置</Button>
          <Button block icon={<FolderOpenFilled />} onClick={() => window.desktop.openPath(status?.paths?.recordings || '')}>打开保存目录</Button>
          <div className="hotkey-tip">快捷键 Ctrl+Shift+L 唤出窗口</div>
          <div className="app-version">界面 v2 · 构建于 {BUILD_LABEL}</div>
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
          </div>
        </header>

        <div className="content">
          {mainTab === 'live' && (
            rows.length ? (
              <div className="card-grid">{rows.map((r) => (
                <LiveCard key={r.key} row={r} platformIcon={platformIcon} onToggle={toggleLive} onSettings={openItemSettings} />
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
                <VideoCard key={r.key} row={r} onSettings={openItemSettings} />
              ))}</div>
            ) : (
              <div className="empty-wrap">
                <Empty description="还没有博主，点「添加博主」监控新视频" />
              </div>
            )
          )}
          {mainTab === 'notes' && (
            <div className="notes-table-wrap">
              <Table columns={noteColumns} dataSource={noteRows} pagination={false} size="middle" scroll={{ y: 520 }} />
            </div>
          )}
          {mainTab === 'logs' && (
            <LogsPanel logDir={status?.paths?.logs} />
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
                extra="开：自动下载到 downloads/短视频/博主名/ 并转写；关：只通知">
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
        <div style={{ marginTop: 24, paddingTop: 16, borderTop: '1px solid #f0f0f0' }}>
          <Popconfirm
            title={itemSettingsType === 'live' ? '确定删除该主播？将从列表移除，不再监控' : '确定删除该博主？不再关注'}
            onConfirm={deleteFromSettings}
          >
            <Button danger icon={<DeleteFilled />}>删除，不再关注</Button>
          </Popconfirm>
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
        <Form form={emailForm} layout="vertical" initialValues={{ smtpHost: 'smtp.qq.com', smtpPort: '465', senderName: '尊者直播监控', transcribeEnabled: true, llmEnabled: true, watchdog: true, autoStartServices: true, localNotify: true }}>
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
          <Form.Item label="SMTP 授权码" name="authCode" extra="已配置过可留空"><Input.Password /></Form.Item>
          <Form.Item label="发件人显示名" name="senderName"><Input /></Form.Item>
          <Form.Item label="收件邮箱" name="toEmail"><Input /></Form.Item>

          <div style={{ fontWeight: 600, margin: '16px 0 8px' }}>转写 & 复盘（LLM）</div>
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
          <Form.Item label="启用 AI 知识提炼" name="llmEnabled" valuePropName="checked">
            <Switch />
          </Form.Item>
          <Form.Item label="API 地址（OpenAI 兼容）" name="llmApiUrl">
            <Input placeholder="https://api.deepseek.com" />
          </Form.Item>
          <Form.Item label="API 密钥" name="llmApiKey" extra="已配置过可留空不修改">
            <Input.Password placeholder="sk-..." />
          </Form.Item>
          <Form.Item label="模型名称" name="llmModel">
            <Input placeholder="deepseek-chat / gpt-4o-mini 等" />
          </Form.Item>
        </Form>

        <div style={{ fontWeight: 600, margin: '8px 0' }}>B站授权</div>
        <Button onClick={biliLogin}>打开 B站 登录 / 授权</Button>
      </Modal>

      <Modal title="复盘内容" open={notesOpen} onCancel={() => setNotesOpen(false)} footer={null} width={800}>
        <pre style={{ whiteSpace: 'pre-wrap', maxHeight: 500, overflow: 'auto' }}>{noteContent}</pre>
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
          勾选想要的视频，点「下载选中」批量保存到 <b>downloads/短视频/{historyBlogger?.name}/</b> 并自动转写复盘。
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
              render: (c, r) => (c
                ? <img src={`file://${c}`} alt="" style={{ width: 96, height: 54, objectFit: 'cover', borderRadius: 4 }} />
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
    </div>
  )
}
