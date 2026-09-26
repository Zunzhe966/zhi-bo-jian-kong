import React, { useState } from 'react'
import { Button, Form, Modal, Steps, Switch, message } from 'antd'

export default function SetupWizard({ open, onDone }) {
  const [step, setStep] = useState(0)
  const [form] = Form.useForm()
  const [saving, setSaving] = useState(false)

  // 服务拉起失败绝不能把界面锁死：配置已经保存，用户回到主界面还能手动重试。
  // 旧实现把 ensure-services 的失败当成致命错误，弹窗又不允许关闭，
  // 一次失败就再也进不去主界面。
  const finish = async () => {
    setSaving(true)
    try {
      const v = form.getFieldsValue()
      await window.desktop.cli('set-app-prefs', JSON.stringify({
        autoStartServices: v.autoStartServices !== false,
        watchdog: v.watchdog !== false,
        localNotify: v.localNotify !== false,
        openAtLogin: !!v.openAtLogin,
        wizardDone: true,
      }))
      if (v.openAtLogin && window.desktop.setLoginItem) {
        await window.desktop.setLoginItem(true)
      }
      try {
        await window.desktop.cli('ensure-services')
        message.success('初始化完成，监控服务已在后台运行')
      } catch (e) {
        message.warning(`配置已保存；后台服务启动失败：${String(e.message || e)}。可在主界面手动启动`)
      }
      onDone()
    } catch (e) {
      message.error(String(e.message || e))
    } finally {
      setSaving(false)
    }
  }

  // 跳过出口：只标记向导已看过，不动其它偏好，也不强行拉服务。
  // 没有这个出口时，向导一旦出错就是死路（弹窗不可关闭）。
  const skip = async () => {
    setSaving(true)
    try {
      await window.desktop.cli('set-app-prefs', JSON.stringify({ wizardDone: true }))
    } catch (e) {
      message.warning(`偏好保存失败：${String(e.message || e)}，下次启动会再次显示本向导`)
    }
    onDone()
  }

  const steps = [
    {
      title: '欢迎',
      content: (
        <div>
          <p>欢迎使用 <b>尊者直播监控</b>。下面 30 秒完成基础配置即可开始用。</p>
          <ul style={{ paddingLeft: 20, lineHeight: 1.8 }}>
            <li>后台会自动监控直播开播 & 博主新视频</li>
            <li>新视频/开播时发邮件通知（稍后在「全局设置」里配邮箱）</li>
            <li>服务崩溃会自动拉起（可关）</li>
          </ul>
        </div>
      ),
    },
    {
      title: '通知',
      content: (
        <>
          <p style={{ color: '#94a3b8', marginBottom: 12 }}>
            通知只走邮箱。完成向导后到「全局设置 → 邮箱推送」填 QQ 邮箱和 SMTP 授权码即可；这里先决定要不要桌面通知兜底。
          </p>
          <Form form={form} layout="vertical" initialValues={{ localNotify: true }}>
            <Form.Item label="同时弹出 桌面通知（远程推送失败时的兜底）" name="localNotify" valuePropName="checked">
              <Switch defaultChecked />
            </Form.Item>
          </Form>
        </>
      ),
    },
    {
      title: '后台',
      content: (
        <Form form={form} layout="vertical" initialValues={{ autoStartServices: true, watchdog: true, openAtLogin: false }}>
          <Form.Item label="打开软件时自动启动监控服务" name="autoStartServices" valuePropName="checked">
            <Switch defaultChecked />
          </Form.Item>
          <Form.Item label="服务异常退出时自动重启（守护）" name="watchdog" valuePropName="checked">
            <Switch defaultChecked />
          </Form.Item>
          <Form.Item label="开机自动启动（登录后托盘运行）" name="openAtLogin" valuePropName="checked">
            <Switch />
          </Form.Item>
        </Form>
      ),
    },
  ]

  return (
    <Modal
      title="首次使用向导"
      open={open}
      closable
      onCancel={skip}
      maskClosable={false}
      width={560}
      footer={[
        step > 0 && <Button key="prev" onClick={() => setStep(step - 1)}>上一步</Button>,
        <Button key="skip" type="text" onClick={skip}>跳过</Button>,
        step < steps.length - 1
          ? <Button key="next" type="primary" onClick={() => setStep(step + 1)}>下一步</Button>
          : <Button key="done" type="primary" loading={saving} onClick={finish}>完成并开始监控</Button>,
      ]}
    >
      <Steps current={step} size="small" items={steps.map((s) => ({ title: s.title }))} style={{ marginBottom: 20 }} />
      {steps[step].content}
    </Modal>
  )
}
