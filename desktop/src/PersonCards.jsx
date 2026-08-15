import React, { memo } from 'react'
import { Button, Switch } from 'antd'
import { SettingOutlined, VideoCameraFilled } from '@ant-design/icons'

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

export const LiveCard = memo(function LiveCard({ row, platformIcon, onToggle, onSettings }) {
  const name = (row.remark || '').replace(/^主播:\s*/, '') || row.url
  return (
    <div className={`person-card ${row.enabled ? '' : 'disabled'}`}>
      <div className="person-head">
        <div className="person-avatar">{platformIcon(row.platform)}</div>
        <div style={{ flex: 1, minWidth: 0 }}>
          <p className="person-name" onClick={() => window.desktop.openExternal(row.url)} title="打开直播间">{name}</p>
          <div className="person-url">{row.url}</div>
        </div>
      </div>
      <div className="person-meta">
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
    </div>
  )
})

export const VideoCard = memo(function VideoCard({ row, onSettings }) {
  return (
    <div className="person-card">
      <div className="person-head">
        <div className="person-avatar"><VideoCameraFilled style={{ fontSize: 24, color: '#fb7299' }} /></div>
        <div style={{ flex: 1, minWidth: 0 }}>
          <p className="person-name" onClick={() => window.desktop.openExternal(row.url)}>{row.name}</p>
          <div className="person-url">{row.url}</div>
        </div>
      </div>
      <div className="person-meta">
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
