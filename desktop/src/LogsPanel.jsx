import React, { useCallback, useEffect, useState } from 'react'
import { Alert, Button, Select, Space } from 'antd'
import { ReloadOutlined, FolderOpenFilled } from '@ant-design/icons'

const LOG_OPTIONS = [
  { label: '核心服务', value: 'zunzhe-core' },
  { label: '核心错误日志', value: 'zunzhe-core.err' },
  { label: '直播监控（旧）', value: 'live_monitor' },
  { label: '视频监控（旧）', value: 'video_monitor' },
  { label: '桌面启动', value: 'desktop-start' },
]

function dirname(filePath) {
  const text = String(filePath || '')
  const cut = Math.max(text.lastIndexOf('/'), text.lastIndexOf('\\'))
  return cut > 0 ? text.slice(0, cut) : text
}

/** 日志清洗：剥离 ANSI 颜色码；把 UTC 时间戳（Z 后缀）转成本地时间。 */
function cleanLogText(raw) {
  const text = String(raw || '')
    // ANSI 转义序列（\u001b[...m）剥掉，只留可读文本
    .replace(/\u001b\[[0-9;]*m/g, '')
    // UTC 时间戳 → 本地时间（+8）：2026-09-24T05:28:37.647255Z
    .replace(/(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?)Z/g, (_m, utc) => {
      const d = new Date(utc + 'Z')
      if (Number.isNaN(d.getTime())) return utc + 'Z'
      const p = (n) => String(n).padStart(2, '0')
      return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`
    })
  return text
}

export default function LogsPanel({ logDir, logPaths }) {
  const [name, setName] = useState('zunzhe-core')
  const [content, setContent] = useState('')
  const [loading, setLoading] = useState(false)
  const [path, setPath] = useState('')

  const activeDir = dirname(path) || logDir || ''
  const inconsistent = logPaths && logPaths.consistent === false
  const driftHint = inconsistent
    ? (logPaths.hint || '当前后台服务仍在写历史日志目录，重新加载后台服务后才会统一。')
    : ''

  const load = useCallback(async () => {
    setLoading(true)
    try {
      const r = await window.desktop.cli('tail-logs', name, '300')
      setContent(cleanLogText(r.content) || '（空）')
      setPath(r.path || '')
    } catch (e) {
      setContent(String(e.message || e))
    } finally {
      setLoading(false)
    }
  }, [name])

  useEffect(() => {
    load()
    const t = setInterval(load, 15000)
    return () => clearInterval(t)
  }, [load])

  return (
    <div className="logs-panel">
      <div className="logs-toolbar">
        <Space wrap>
          <Select value={name} options={LOG_OPTIONS} onChange={setName} style={{ width: 160 }} />
          <Button icon={<ReloadOutlined />} loading={loading} onClick={load}>刷新日志</Button>
          {activeDir && (
            <Button icon={<FolderOpenFilled />} onClick={() => window.desktop.openPath(activeDir)}>打开日志目录</Button>
          )}
        </Space>
        {path && <span className="logs-path">{path}</span>}
      </div>
      {driftHint && (
        <Alert
          type="warning"
          showIcon
          className="logs-warning"
          message="日志目录尚未统一"
          description={driftHint}
        />
      )}
      <pre className="logs-body">{content}</pre>
    </div>
  )
}
