// 添加直播监控 / 添加发布视频监控 弹窗（独立组件）。
// 拆分自 App.jsx，目的：添加流程的改动不影响主界面。
import React from 'react'
import { Form, Input, Modal, Select, Switch } from 'antd'
import {
  qualityOptions, notifyOptions, platformOptions,
} from './ui-constants'

export default function AddModals({
  addOpen, addForm, onAddLive,
  videoOpen, videoForm, onAddVideo,
  onCloseAdd, onCloseVideo,
}) {
  return (
    <>
      <Modal title="添加直播监控" open={addOpen} onOk={onAddLive} onCancel={onCloseAdd} okText="确认并添加" width={700} destroyOnClose>
        <Form form={addForm} layout="horizontal" labelCol={{ span: 6 }} wrapperCol={{ span: 16 }} initialValues={{ providerId: 'Auto', quality: '原画', notify: '跟随全局' }}>
          <Form.Item label="平台" name="providerId" rules={[{ required: true }]}>
            <Select options={platformOptions} />
          </Form.Item>
          <Form.Item label="链接/频道ID" name="channelId" rules={[{ required: true, message: '请输入' }]}
            extra="支持 B站/抖音/快手直播，直接粘贴直播间链接；智能体请通过 MCP/CLI 连接，不必在此配置大模型">
            <Input placeholder="直播间链接，或抖音号（如 xiguahaochi96，自动解析）" />
          </Form.Item>
          <Form.Item label="主播名/备注" name="remarks"
            extra="输抖音号时留空即可——软件会自动用解析到的昵称">
            <Input placeholder="留空自动获取" />
          </Form.Item>
          <Form.Item label="录制画质" name="quality">
            <Select options={qualityOptions} />
          </Form.Item>
          <Form.Item label="通知方式" name="notify"
            extra="默认跟随全局（全局渠道为邮箱）；可单独指定「邮箱」或「不通知」">
            <Select options={notifyOptions} />
          </Form.Item>
        </Form>
      </Modal>

      <Modal title="添加发布视频监控" open={videoOpen} onOk={onAddVideo} onCancel={onCloseVideo} okText="确认并添加" width={700} destroyOnClose>
        <Form form={videoForm} layout="horizontal" labelCol={{ span: 6 }} wrapperCol={{ span: 16 }} initialValues={{ autoDownload: true, notify: '跟随全局' }}>
          <Form.Item label="博主名称" name="name"
            extra="输抖音号时留空即可——软件会自动用解析到的昵称">
            <Input placeholder="留空自动获取" />
          </Form.Item>
          <Form.Item label="监控地址" name="url" rules={[{ required: true }]}
            extra="B站填 space.bilibili.com/数字；抖音/快手可用 RSSHub 订阅地址">
            <Input placeholder="主页链接，或抖音号（如 xiguahaochi96，自动解析）" />
          </Form.Item>
          <Form.Item label="自动下载新视频" name="autoDownload" valuePropName="checked"
            extra="开：发现新视频立即下载；关：只通知不下载">
            <Switch />
          </Form.Item>
          <Form.Item label="通知方式" name="notify"
            extra="默认跟随全局（全局渠道为邮箱）；可单独指定「邮箱」或「不通知」">
            <Select options={notifyOptions} />
          </Form.Item>
        </Form>
      </Modal>
    </>
  )
}
