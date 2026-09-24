// 主播/博主内容详情弹窗（独立组件）。
// 拆分自 App.jsx，目的：详情面板的改动不影响主界面；之前"查看内容黑屏"
// 的漏 import 就发生在这里，独立成文件后改动范围可控。
import React from 'react'
import {
  Button, Modal, Space, Tag, Tooltip,
} from 'antd'
import {
  FolderFilled, FolderOpenFilled, VideoCameraFilled,
} from '@ant-design/icons'
import {
  humanSize, stamp, isTranscribableMedia,
  processTaskForFile, processTaskLabel, processTaskAction,
} from './stash'

export default function DetailModal({
  open,
  row,
  type,
  rowName,
  stashPath,
  path,
  files,
  loading,
  queueing,
  error,
  tasks,
  onClose,
  onRefresh,
  onQueue,
  onOpenStash,
  onReveal,
  onOpenExternal,
}) {
  const title = type === 'live'
    ? `主播内容 — ${rowName || (row && row.url) || ''}`
    : `博主内容 — ${(row && row.name) || ''}`

  return (
    <Modal
      title={title}
      open={open}
      onCancel={onClose}
      footer={null}
      width={720}
      destroyOnClose
    >
      <div className="detail-block">
        <div className="detail-label">监控地址</div>
        <a className="detail-link" onClick={() => row && onOpenExternal(row.url)}>
          {row && row.url}
        </a>
        <div className="detail-hint">点击用浏览器打开</div>
      </div>

      <div className="detail-block">
        <div className="detail-label">存放位置</div>
        <a className="detail-link" onClick={() => row && onOpenStash(row, type)}>
          {path || stashPath || '（未解析）'}
        </a>
        <Space size={8} style={{ marginTop: 6 }} wrap>
          <Button size="small" icon={<FolderOpenFilled />}
            onClick={() => row && onOpenStash(row, type)}>打开文件夹</Button>
          <Button size="small"
            onClick={() => { const p = path || stashPath; if (p) onReveal(p) }}>
            在访达中定位
          </Button>
        </Space>
      </div>

      <div className="detail-block">
        <div className="detail-label">文字稿与复盘</div>
        <a className="detail-link" onClick={() => row && onOpenStash(row, type)}>
          {stashPath || '（未解析）'}
        </a>
        <div className="detail-hint">转写文字稿落在该主播自己的文件夹里（与视频同目录），不是全局目录</div>
        <Space size={8} style={{ marginTop: 6 }} wrap>
          <Button size="small" icon={<FolderOpenFilled />}
            onClick={() => row && onOpenStash(row, type)}>打开文字稿文件夹</Button>
        </Space>
      </div>

      <div className="detail-block">
        <div className="detail-label">
          里面的文件
          <Tag style={{ marginLeft: 8 }}>{files.length}</Tag>
          <Button size="small" type="link" loading={loading}
            onClick={() => row && onRefresh(row, type)}>刷新</Button>
        </div>
        {loading && !files.length ? (
          <div className="detail-empty">正在读取…</div>
        ) : files.length ? (
          <div className="file-list">
            {files.map((f) => {
              const task = processTaskForFile(tasks, f.path)
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
                      loading={queueing === f.path}
                      disabled={taskAction.disabled}
                      onClick={() => onQueue(f)}>{taskAction.label}</Button>
                  )}
                  <Button size="small" type="link"
                    onClick={() => onReveal(f.path)}>在访达中显示</Button>
                </div>
              )
            })}
          </div>
        ) : (
          <div className="detail-empty">
            {error
              ? `读不到目录：${error}`
              : (type === 'live'
                ? '这个主播还没有录像文件。开播并录过一次之后，文件会出现在上面这个文件夹里。'
                : '还没有下载过视频。打开「发现新视频自动下载」后，下载的文件会出现在上面这个文件夹里。')}
          </div>
        )}
      </div>
    </Modal>
  )
}
