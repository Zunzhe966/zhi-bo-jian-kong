import React, { memo } from 'react'
import { Button, Switch, Tooltip } from 'antd'
import {
  SettingOutlined, VideoCameraFilled, FolderOpenFilled, ProfileOutlined,
} from '@ant-design/icons'

const statusText = {
  recording: '录制中',
  monitoring: '监控中，等待开播',
  noting: '未开始监控',
}

function notifyLabel(n) {
  if (n === '微信') return '微信通知'
  if (n === '邮箱') return '邮箱通知'
  if (n === '不通知') return '不通知'
  return '跟随全局'
}

/** 直播间/主页地址：做成真链接，点一下就用浏览器打开，不再是只能看的死文本。 */
function UrlLine({ url }) {
  if (!url) return null
  return (
    <Tooltip title="点击用浏览器打开">
      <span className="person-url person-url-link" onClick={() => window.desktop.openExternal(url)}>
        {url}
      </span>
    </Tooltip>
  )
}

export const LiveCard = memo(function LiveCard({
  row, platformIcon, platformLabel, onToggle, onSettings, onOpenStash, onDetail,
}) {
  const name = (row.remark || '').replace(/^主播:\s*/, '') || row.url
  const platName = platformLabel ? platformLabel(row.platform) : ''
  return (
    <div className={`person-card ${row.enabled ? '' : 'disabled'}`}>
      <div className="person-head">
        <div className="person-avatar">{platformIcon(row.platform)}</div>
        <div style={{ flex: 1, minWidth: 0 }}>
          <p className="person-name" onClick={() => window.desktop.openExternal(row.url)} title="打开直播间">{name}</p>
          <UrlLine url={row.url} />
        </div>
      </div>
      <div className="person-meta">
        {platName && <span className="meta-tag meta-platform">{platName}</span>}
        <span className={`meta-tag ${row.state === 'recording' ? 'state-recording' : row.state === 'monitoring' ? 'state-live' : ''}`}>
          {statusText[row.state]}
        </span>
        <span className="meta-tag">{row.quality || '原画'}</span>
        <span className={`meta-tag ${row.notify === '微信' ? 'notify-wechat' : row.notify === '不通知' ? 'notify-none' : ''}`}>
          {notifyLabel(row.notify)}
        </span>
        {!row.enabled && <span className="meta-tag">已暂停</span>}
      </div>
      <div className="person-actions">
        <Switch checked={row.enabled} onChange={(c) => onToggle(row, c)} checkedChildren="监控" unCheckedChildren="暂停" />
        <Button type="primary" className="btn-settings" icon={<SettingOutlined />} onClick={() => onSettings(row, 'live')}>
          主播设置
        </Button>
      </div>
      {/* 「东西存哪了」和「存了什么」是两个不同的问题，各给一个入口，
          点下去真的跳到那个目录 / 真的列出文件，不是摆设。 */}
      <div className="person-actions person-actions-sub">
        <Button size="small" icon={<FolderOpenFilled />} onClick={() => onOpenStash(row, 'live')}>打开存放位置</Button>
        <Button size="small" icon={<ProfileOutlined />} onClick={() => onDetail(row, 'live')}>查看内容</Button>
      </div>
    </div>
  )
})

export const VideoCard = memo(function VideoCard({
  row, platformIcon, platformLabel, onSettings, onOpenStash, onDetail,
}) {
  const platName = platformLabel ? platformLabel(row.platform) : ''
  // 平台图标以前是写死的 B站图标：不管监控的是哪个站，卡片长得一模一样，
  // 用户没法一眼看出「这条是哪个网站」。现在跟着 row.platform 走。
  const avatar = platformIcon && row.platform
    ? platformIcon(row.platform)
    : <VideoCameraFilled style={{ fontSize: 24, color: '#fb7299' }} />
  return (
    <div className="person-card">
      <div className="person-head">
        <div className="person-avatar">{avatar}</div>
        <div style={{ flex: 1, minWidth: 0 }}>
          <p className="person-name" onClick={() => window.desktop.openExternal(row.url)}>{row.name}</p>
          <UrlLine url={row.url} />
        </div>
      </div>
      <div className="person-meta">
        {platName && <span className="meta-tag meta-platform">{platName}</span>}
        <span className="meta-tag">{row.download ? '自动下载' : '只通知'}</span>
        <span className={`meta-tag ${row.notify === '微信' ? 'notify-wechat' : row.notify === '不通知' ? 'notify-none' : ''}`}>
          {notifyLabel(row.notify)}
        </span>
      </div>
      <div className="person-actions">
        <Button type="primary" className="btn-settings" icon={<SettingOutlined />} onClick={() => onSettings(row, 'video')}>
          博主设置
        </Button>
      </div>
      <div className="person-actions person-actions-sub">
        <Button size="small" icon={<FolderOpenFilled />} onClick={() => onOpenStash(row, 'video')}>打开存放位置</Button>
        <Button size="small" icon={<ProfileOutlined />} onClick={() => onDetail(row, 'video')}>查看内容</Button>
      </div>
    </div>
  )
})

/** 只比较会影响界面展示的稳定字段，忽略 minutes_ago 等每次都在变的噪音 */
export function statusFingerprint(data) {
  if (!data) return ''
  return JSON.stringify({
    services: data.services,
    sourceHealth: data.source_health,
    notifyHealth: data.notify_health,
    live: data.live_watchlist,
    video: data.video_watchlist,
    processTasks: data.process_tasks,
    notes: data.notes,
    recordingSessions: (data.recording_sessions || [])
      .map((s) => s.url)
      .sort(),
    recording: (data.recent_recordings || [])
      .filter((r) => r.probably_recording)
      .map((r) => r.file)
      .sort(),
  })
}
