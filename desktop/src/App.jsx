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
  processTaskForFile, processTaskLabel, stashFieldDefault, downloadQueueFeedback,
} from './stash'
import LogsPanel from './LogsPanel'
import DetailModal from './DetailModal'
import AddModals from './AddModals'
import ItemSettingsModal from './ItemSettingsModal'
import SettingsModal from './SettingsModal'
import HistoryModals from './HistoryModals'
import NotifyModal from './NotifyModal'
import {
  qualityOptions, notifyOptions, platformOptions, statusText,
} from './ui-constants'
import {
  platformLabels, platformColors, detectPlatform, platformLabel, normalizeUrl, buildRows,
} from './platform-utils'
import SetupWizard from './Wizard'

// 构建时间与版本号由 vite 注入，用来确认界面是否已更新
const BUILD_LABEL = typeof __APP_BUILD__ !== 'undefined' ? __APP_BUILD__ : '开发版'
const APP_VERSION = typeof __APP_VERSION__ !== 'undefined' ? __APP_VERSION__ : 'dev'

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
  // 平台自己报告的总条数（B站官方接口）。它和实际列出条数不等时要在弹窗里
  // 说清「差几条、为什么」——否则用户只看到少一条，永远查不出原因。
  const [historyExpected, setHistoryExpected] = useState(null)
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
      // 通知渠道只留邮箱（微信/Server酱/pushplus 已按需求裁掉，入口不再提供）
      const pairs = [
        ['直播状态推送渠道', '邮箱'],
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

  // 抖音号/平台ID → URL：纯数字按 B站 ID 拼地址；裸抖音号（如 Glx6768）原样传给
  // CLI——1.7.2 起 Rust 侧用 iesdouyin 免登录接口解析（秒级），不再走 Chrome 浏览器
  // 反查（被风控、曾 150 秒超时把「添加」按钮拖到卡死）。
  const resolveAnchorIfNeeded = (input, platform) => {
    const v = String(input || '').trim()
    if (!v || /^https?:\/\//i.test(v)) return v
    if (/^\d+$/.test(v)) return normalizeUrl(v, platform)
    return v
  }

  const onAddLive = async () => {
    const v = await addForm.validateFields()
    let url
    try { url = await resolveAnchorIfNeeded(v.channelId, v.providerId) }
    catch (e) { return message.error(String(e.message || e)) }
    // 允许裸抖音号直达 CLI（Rust 侧免登录解析并验证）；CLI 失败会带可读原因抛回
    if (!url) return message.error('请输入有效的直播间地址或ID')
    setLoading(true)
    try {
      const r = await window.desktop.cli('add-live', url, v.remarks ? `主播: ${v.remarks}` : '', v.quality || '')
      // 表单里的「通知方式」不再静默丢弃：非「跟随全局」时走 set-notify 落配置。
      // 裸抖音号输入时 CLI 返回的 r.url 是解析后的直播间地址，通知要挂到那个地址上。
      if (v.notify && v.notify !== '跟随全局') {
        await setItemNotify((r && r.url) || url, v.notify, v.remarks || '')
      }
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
      // 裸抖音号（如 Glx6768）原样传给 CLI——1.7.2 起 Rust 侧 iesdouyin 免登录
      // 秒级解析，不再走 Chrome 浏览器反查（被风控、曾把界面拖到卡死）
      const url = v.url.trim()
      await window.desktop.cli('add-video', url, (v.name || '').trim(), v.autoDownload ? '是' : '否', v.notify || '跟随全局')
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

  // 后端「操作类」命令失败时是 exit 0 + {ok:false, error}（不抛异常），
  // 只看 try/catch 会把失败当成功——必须检查 r.ok。
  const setItemNotify = async (url, channel, name = '') => {
    let reported = false
    try {
      const r = await window.desktop.cli('set-notify', url, channel, name)
      if (r && r.ok === false) {
        reported = true
        message.error(r.error || '保存通知方式失败')
        throw new Error(r.error || '保存通知方式失败')
      }
      await fetchStatus()
    } catch (e) {
      if (!reported) message.error(String(e.message || e))
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
        // 不回退到产物根：博主自己的目录不存在就是「还没有内容」，
        // 避免出现「标题是博主、内容却是全局目录」的误导。
        // 打开存放位置（openItemStash）仍保留退到上一级的逻辑。
        setDetailPath(dir)
        setDetailFiles([])
        setDetailError((res && res.error) || '该博主还没有内容（目录尚未创建）；完成平台授权后，新内容会自动落到这里')
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
        // add-live 只收 URL/NAME/QUALITY 三个参数（1.6.6 踩过坑：多传 notify 必报错）；
        // 通知通道走独立的 set-notify 命令。
        await window.desktop.cli('add-live', v.url, remark, v.quality || '')
        if (!v.enabled) {
          await window.desktop.cli('remove-live', v.url)
        }
        const nextNotify = v.notify || '跟随全局'
        if (nextNotify !== (itemSettingsRow?.notify || '跟随全局')) {
          await setItemNotify(v.url, nextNotify, v.name || '')
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
        const r = await window.desktop.cli('delete-live', itemSettingsRow.url)
        // 后端删除失败是 exit 0 + {ok:false}，不看 r.ok 会谎报成功
        if (r && r.ok === false) { message.error(r.error || '删除失败'); return }
        message.success('已删除，不再关注该主播')
      } else {
        const r = await window.desktop.cli('remove-video', itemSettingsRow.url)
        if (r && r.ok === false) { message.error(r.error || '删除失败'); return }
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
      const r = await window.desktop.cli('add-video', row.url, row.name, checked ? '是' : '否')
      if (r && r.ok === false) { message.error(r.error || '保存下载开关失败'); return }
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
    setHistoryExpected(null)
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
      setHistoryExpected(Number(r.expected_total) || null)
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
      // 后端 notify-retry 的 0 被改写成 100（不是「全部」），文案不许骗人
      const r = await window.desktop.cli('notify-retry', '0')
      const n = r.retried || 0
      message.success(
        n >= 100
          ? `已把 100 条失败通知放回待发送队列（单次上限 100 条，还有更多就再点一次）`
          : `已把 ${n} 条失败通知放回待发送队列，下一轮会自动重试`
      )
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
                  platformCred={status?.platform_credentials}
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
                  platformCred={status?.platform_credentials}
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

      <AddModals
        addOpen={addOpen}
        addForm={addForm}
        onAddLive={onAddLive}
        videoOpen={videoOpen}
        videoForm={videoForm}
        onAddVideo={onAddVideo}
        onCloseAdd={() => setAddOpen(false)}
        onCloseVideo={() => setVideoOpen(false)}
      />

      <ItemSettingsModal
        type={itemSettingsType}
        row={itemSettingsRow}
        open={itemSettingsOpen}
        form={itemSettingsForm}
        paths={paths}
        stashPath={stashOf(itemSettingsRow, itemSettingsType)}
        onCancel={() => setItemSettingsOpen(false)}
        onSave={saveItemSettings}
        onRefresh={refreshLive}
        onOpenHistory={openHistory}
        onOpenStash={openItemStash}
        onOpenDetail={openItemDetail}
        onDelete={deleteFromSettings}
      />

      <DetailModal
        open={detailOpen}
        row={detailRow}
        type={detailType}
        rowName={rowDisplayName(detailRow, detailType)}
        stashPath={stashOf(detailRow, detailType)}
        path={detailPath}
        files={detailFiles}
        loading={detailLoading}
        queueing={detailQueueing}
        error={detailError}
        tasks={status?.process_tasks}
        onClose={() => setDetailOpen(false)}
        onRefresh={loadDetailFiles}
        onQueue={queueTranscription}
        onOpenStash={openItemStash}
        onReveal={(p) => window.desktop.showItem(p)}
        onOpenExternal={(url) => window.desktop.openExternal(url)}
      />

      <SettingsModal
        form={emailForm}
        open={settingsOpen}
        saving={emailSaving}
        emailConfigured={emailConfigured}
        emailStatusText={emailStatusText}
        paths={status?.paths}
        onTest={testEmail}
        onSave={saveEmailSettings}
        onClose={() => setSettingsOpen(false)}
        onPickStash={pickStashDir}
        onPickDatabase={pickDatabaseFile}
        onSetupQq={setupQqMail}
        onBiliLogin={biliLogin}
        onDouyinLogin={douyinLogin}
      />

      <HistoryModals
        batchOpen={batchOpen}
        batchForm={batchForm}
        onBatchBrowse={onBatchBrowse}
        onCloseBatch={() => setBatchOpen(false)}
        blogger={historyBlogger}
        open={historyOpen}
        videos={historyVideos}
        expectedTotal={historyExpected}
        selected={historySelected}
        loading={historyLoading}
        downloading={historyDownloading}
        paths={paths}
        onClose={() => setHistoryOpen(false)}
        onDownloadAll={downloadAll}
        onDownloadSelected={downloadSelected}
        onSelect={(keys) => setHistorySelected(keys)}
      />

      <NotifyModal
        open={notifyOpen}
        events={notifyEvents}
        loading={notifyLoading}
        busy={notifyBusy}
        healthHint={status?.notify_health?.hint}
        healthOk={status?.notify_health?.ok}
        onClose={() => setNotifyOpen(false)}
        onPruneSent={pruneSentNotify}
        onPruneFailed={pruneFailedNotify}
        onRetry={retryFailedNotify}
      />
    </div>
  )
}
