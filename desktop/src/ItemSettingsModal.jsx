// 主播/博主设置弹窗（独立组件）。
// 拆分自 App.jsx，目的：设置表单与删除逻辑的改动不影响主界面。
import React from 'react'
import {
  Button, Form, Input, Modal, Popconfirm, Select, Space, Switch,
} from 'antd'
import { DeleteFilled, FolderOpenFilled, ReloadOutlined } from '@ant-design/icons'
import { qualityOptions, notifyOptions } from './ui-constants'

export default function ItemSettingsModal({
  type,
  row,
  open,
  form,
  paths,
  stashPath,
  onCancel,
  onSave,
  onRefresh,
  onOpenHistory,
  onOpenStash,
  onOpenDetail,
  onDelete,
}) {
  const title = type === 'live'
    ? `主播设置 — ${(row?.remark || '').replace(/^主播:\s*/, '') || ''}`
    : `博主设置 — ${row?.name || ''}`

  return (
    <Modal
      title={title}
      open={open}
      onCancel={onCancel}
      onOk={onSave}
      okText="保存设置"
      cancelText="取消"
      width={560}
      destroyOnClose
    >
      <Form form={form} layout="vertical">
        {type === 'live' ? (
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
              extra="默认跟随全局（全局渠道为邮箱）；可单独指定「邮箱」或「不通知」">
              <Select options={notifyOptions} />
            </Form.Item>
            <Button icon={<ReloadOutlined />} onClick={() => row && onRefresh(row)}>
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
              extra={`开：自动下载到 ${paths.video_root || paths.media_root || "<产物根目录>"}/${row?.name || "博主名"}/短视频/ 并转写；关：只通知`}>
              <Switch />
            </Form.Item>
            <Form.Item label="通知方式" name="notify">
              <Select options={notifyOptions} />
            </Form.Item>
            <Button type="link" onClick={() => { onCancel(); onOpenHistory(row) }}>
              查看历史视频 / 批量下载
            </Button>
          </>
        )}
      </Form>
      {/* 「东西存哪儿了」——用户要求直接写在设置里，点了就能去。
          这里显示的是按 Rust 落盘规则算出来的真实路径，不是占位文案。 */}
      <div className="stash-box">
        <div className="stash-box-title">
          {type === 'live' ? '录像存到这里' : '视频与文字稿存到这里'}
        </div>
        <div className="stash-box-path"
          title="点击打开这个文件夹"
          onClick={() => row && onOpenStash(row, type)}>
          {stashPath || '（还没解析出存放位置，请先在全局设置里确认产物根目录）'}
        </div>
        <Space size={8} wrap>
          <Button size="small" icon={<FolderOpenFilled />}
            onClick={() => row && onOpenStash(row, type)}>
            打开这个文件夹
          </Button>
          <Button size="small"
            onClick={() => { onCancel(); onOpenDetail(row, type) }}>
            看里面有什么
          </Button>
        </Space>
        <div className="stash-box-hint">
          {type === 'live'
            ? '按分类存放：直播 / 主播名 /'
            : '按分类存放：短视频 / 博主名 /'}
        </div>
      </div>
      <div style={{ marginTop: 24, paddingTop: 16, borderTop: '1px solid #f0f0f0' }}>
        <Popconfirm
          title={type === 'live' ? '确定删除该主播？将从列表移除，不再监控' : '确定删除该博主？不再关注'}
          onConfirm={onDelete}
        >
          <Button danger icon={<DeleteFilled />}>删除，不再关注</Button>
        </Popconfirm>
      </div>
    </Modal>
  )
}
