import React, { useCallback, useEffect, useState } from 'react'
import { Button, Select, Space } from 'antd'
import { ReloadOutlined, FolderOpenFilled } from '@ant-design/icons'

const LOG_OPTIONS = [
  { label: '直播监控', value: 'live_monitor' },
  { label: '视频监控', value: 'video_monitor' },
  { label: '桌面启动', value: 'desktop-start' },
]

export default function LogsPanel({ logDir }) {
  const [name, setName] = useState('live_monitor')
  const [content, setContent] = useState('')
  const [loading, setLoading] = useState(false)
  const [path, setPath] = useState('')

  const load = useCallback(async () => {
    setLoading(true)
    try {
      const r = await window.desktop.cli('tail-logs', name, '300')
      setContent(r.content || '（空）')
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
          {logDir && (
            <Button icon={<FolderOpenFilled />} onClick={() => window.desktop.openPath(logDir)}>打开日志目录</Button>
          )}
        </Space>
        {path && <span className="logs-path">{path}</span>}
      </div>
      <pre className="logs-body">{content}</pre>
    </div>
  )
}
