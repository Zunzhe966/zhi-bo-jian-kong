// 通知治理弹窗（独立组件）。
// 拆分自 App.jsx，目的：通知历史/重试的改动不影响主界面。
import React from 'react'
import { Button, Modal, Popconfirm, Table, Tag } from 'antd'
import { ClearOutlined, DeleteFilled, RollbackOutlined } from '@ant-design/icons'

export default function NotifyModal({
  open,
  events,
  loading,
  busy,
  healthHint,
  healthOk,
  onClose,
  onPruneSent,
  onPruneFailed,
  onRetry,
}) {
  return (
    <Modal
      title="通知治理"
      open={open}
      onCancel={onClose}
      width={880}
      footer={[
        <Button key="close" onClick={onClose}>关闭</Button>,
        <Popconfirm key="prune-sent" title="清理所有已发送通知记录？只影响历史列表，不影响已送达" onConfirm={onPruneSent}>
          <Button icon={<ClearOutlined />} loading={busy}>清理已发送</Button>
        </Popconfirm>,
        <Popconfirm key="prune-failed" title="清理所有失败通知记录？将不再保留这些失败提醒" onConfirm={onPruneFailed}>
          <Button danger icon={<DeleteFilled />} loading={busy}>清理失败记录</Button>
        </Popconfirm>,
        <Button key="retry" type="primary" icon={<RollbackOutlined />} loading={busy} onClick={onRetry}>
          重试全部失败通知
        </Button>,
      ]}
    >
      <p style={{ marginBottom: 12, color: '#666' }}>
        这里显示最近 50 条失败通知。配置好邮箱后点「重试全部失败通知」；
        不需要的历史失败记录可以清理，避免通知健康一直显示异常。
      </p>
      {healthHint ? (
        <div style={{
          marginBottom: 12,
          padding: '10px 12px',
          borderRadius: 8,
          background: healthOk ? '#f6ffed' : '#fff1f0',
          border: `1px solid ${healthOk ? '#b7eb8f' : '#ffccc7'}`,
          color: '#333',
        }}>
          <strong>诊断建议：</strong> {healthHint}
        </div>
      ) : null}
      <Table
        rowKey="id"
        loading={loading}
        dataSource={events}
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
  )
}
