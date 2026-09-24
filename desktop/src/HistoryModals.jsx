// 批量下载 + 历史视频 弹窗（独立组件）。
// 拆分自 App.jsx，目的：下载流程的改动不影响主界面。
import React from 'react'
import { Button, Form, Input, Modal, Table } from 'antd'

export default function HistoryModals({
  batchOpen, batchForm, onBatchBrowse, onCloseBatch,
  blogger, open, videos, selected, loading, downloading, paths,
  onClose, onDownloadAll, onDownloadSelected, onSelect,
}) {
  return (
    <>
      <Modal title="批量下载视频" open={batchOpen} onOk={onBatchBrowse} onCancel={onCloseBatch} okText="列出历史视频" width={640} destroyOnClose>
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
        title={`历史视频 - ${blogger?.name || ''}`}
        open={open}
        onCancel={onClose}
        width={860}
        footer={[
          <span key="tip" style={{ float: 'left', color: '#999', fontSize: 12, lineHeight: '32px' }}>
            已选 {selected.length} / {videos.length}
          </span>,
          <Button key="close" onClick={onClose}>关闭</Button>,
          <Button key="all" loading={downloading} disabled={!videos.some((v) => !v.downloaded)} onClick={onDownloadAll}>
            下载全部（未下载）
          </Button>,
          <Button key="dl" type="primary" loading={downloading} disabled={!selected.length} onClick={onDownloadSelected}>
            下载选中（{selected.length}）
          </Button>,
        ]}
      >
        <p style={{ marginBottom: 12, color: '#666' }}>
          勾选想要的视频，点「下载选中」批量保存到 <b>{paths.video_root || paths.media_root || "产物根目录"}/{blogger?.name || "博主名"}/短视频/</b> 并自动转写复盘。
        </p>
        <Table
          rowKey="key"
          loading={loading}
          dataSource={videos}
          pagination={false}
          size="small"
          scroll={{ y: 460 }}
          rowSelection={{
            selectedRowKeys: selected,
            onChange: onSelect,
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
    </>
  )
}
