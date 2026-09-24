// 全局设置弹窗（独立组件）。
// 拆分自 App.jsx，目的：配置表单的改动不影响主界面。
import React from 'react'
import {
  Button, Form, Input, Modal, Select, Switch,
} from 'antd'
import { FolderOpenFilled, MailOutlined } from '@ant-design/icons'

export default function SettingsModal({
  form,
  open,
  saving,
  emailConfigured,
  emailStatusText,
  paths,
  onTest,
  onSave,
  onClose,
  onPickStash,
  onPickDatabase,
  onSetupQq,
  onBiliLogin,
  onDouyinLogin,
}) {
  return (
    <Modal
      title="全局设置"
      open={open}
      onCancel={onClose}
      width={680}
      styles={{ body: { maxHeight: '70vh', overflow: 'auto' } }}
      footer={[
        <Button key="test" icon={<MailOutlined />} onClick={onTest}>测试通知</Button>,
        <Button key="save" type="primary" loading={saving} onClick={onSave}>保存配置</Button>,
        <Button key="close" onClick={onClose}>关闭</Button>,
      ]}
    >
      <Form form={form} layout="vertical" initialValues={{ smtpHost: 'smtp.qq.com', smtpPort: '465', senderName: '尊者监控直播', transcribeEnabled: true, pipelineTimeout: '21600', llmEnabled: true, watchdog: true, autoStartServices: true, localNotify: true, mediaRoot: '', downloadDir: '', fupanDir: '', logsDir: '', databaseFile: '' }}>
        <div style={{ fontWeight: 600, margin: '4px 0 8px' }}>应用行为</div>
        <Form.Item label="打开软件时自动启动监控" name="autoStartServices" valuePropName="checked">
          <Switch />
        </Form.Item>
        <Form.Item label="服务崩溃自动重启（守护）" name="watchdog" valuePropName="checked">
          <Switch />
        </Form.Item>
        <Form.Item label="开机自动启动（登录后托盘运行）" name="openAtLogin" valuePropName="checked">
          <Switch />
        </Form.Item>
        <Form.Item label="Windows 桌面通知（远程推送失败时的兜底）" name="localNotify" valuePropName="checked">
          <Switch />
        </Form.Item>

        <div style={{ fontWeight: 600, margin: '16px 0 8px' }}>存放位置</div>
        <div style={{ marginBottom: 10, padding: '8px 12px', borderRadius: 6, background: '#f6ffed', border: '1px solid #b7eb8f', fontSize: 13 }}>
          产物（直播录像、下载的短视频、转写文本、切片）和程序是分开的，默认不放在程序目录里。
        </div>
        <Form.Item label="产物根目录" name="mediaRoot"
          extra={<span>留空 = 默认位置 <code>~/Documents/尊者产物</code>。录像、下载、转写全部落在它下面。</span>}>
          <Input placeholder="留空用默认位置"
            addonAfter={<a onClick={() => onPickStash('mediaRoot')} style={{ cursor: 'pointer' }}>选择…</a>} />
        </Form.Item>
        <Form.Item label="短视频下载目录" name="downloadDir"
          extra="留空 = <产物根目录>（新下载落在 <该目录>/博主名/短视频/）">
          <Input placeholder="留空自动跟随产物根目录"
            addonAfter={<a onClick={() => onPickStash('downloadDir')} style={{ cursor: 'pointer' }}>选择…</a>} />
        </Form.Item>
        <Form.Item label="复盘输出目录" name="fupanDir"
          extra="留空 = <产物根目录>/fupan_output（转写、知识点、切片、封面都在这下面）">
          <Input placeholder="留空自动跟随产物根目录"
            addonAfter={<a onClick={() => onPickStash('fupanDir')} style={{ cursor: 'pointer' }}>选择…</a>} />
        </Form.Item>
        <Form.Item label="日志目录" name="logsDir"
          extra="留空 = <产物根目录>/logs。运行日志和心跳记录可单独换到别的盘。">
          <Input placeholder="留空使用 <产物根目录>/logs"
            addonAfter={<a onClick={() => onPickStash('logsDir')} style={{ cursor: 'pointer' }}>选择…</a>} />
        </Form.Item>
        <Form.Item label="数据库文件" name="databaseFile"
          extra="留空 = <产物根目录>/database/zunzhe-core.db；已有旧库会继续沿用，不会自动覆盖或搬动数据。修改后需重启后台服务才生效。">
          <Input placeholder="留空使用 <产物根目录>/database/zunzhe-core.db"
            addonAfter={<a onClick={onPickDatabase} style={{ cursor: 'pointer' }}>选择…</a>} />
        </Form.Item>
        <div style={{ marginBottom: 4 }}>
          <Button size="small" icon={<FolderOpenFilled />}
            onClick={() => window.desktop.openPath(paths?.media_root || '')}>打开产物目录</Button>
          <Button size="small" icon={<FolderOpenFilled />} style={{ marginLeft: 8 }}
            onClick={() => window.desktop.openPath(paths?.logs || '')}>打开日志目录</Button>
          <Button size="small" icon={<FolderOpenFilled />} style={{ marginLeft: 8 }}
            onClick={() => window.desktop.openPath(paths?.database || '')}>打开数据库位置</Button>
          <span style={{ marginLeft: 10, color: '#888', fontSize: 12 }}>
            当前：{paths?.media_root || '-'} · 数据库：{paths?.database || '-'}
          </span>
        </div>

        <div style={{ fontWeight: 600, margin: '16px 0 8px' }}>抗代理推送（推荐）</div>
        <Form.Item label="Server酱 SendKey" name="serverChanKey"
          extra={<span>在 <a onClick={() => window.desktop.openExternal('https://sct.ftqq.com/')}>sct.ftqq.com</a> 微信登录后复制 SendKey。已配置可留空。</span>}>
          <Input.Password placeholder="SCTxxxxxxxx..." />
        </Form.Item>
        <Form.Item label="PushPlus Token" name="pushplusToken">
          <Input.Password placeholder="留空则不启用" />
        </Form.Item>

        <div style={{ fontWeight: 600, margin: '16px 0 8px' }}>邮箱推送</div>
        <div style={{ marginBottom: 8, padding: '8px 12px', borderRadius: 6, background: emailConfigured ? '#f6ffed' : '#fff7e6', border: `1px solid ${emailConfigured ? '#b7eb8f' : '#ffd591'}` }}>
          {emailConfigured ? '✅ 邮箱已配置' : `⚠️ ${emailStatusText || '邮箱未配置'}`}
          <Button size="small" style={{ marginLeft: 12 }} onClick={onSetupQq}>QQ 邮箱一键填入</Button>
        </div>
        <Form.Item label="SMTP 服务器" name="smtpHost"><Input placeholder="smtp.qq.com" /></Form.Item>
        <Form.Item label="SMTP 端口" name="smtpPort"><Input placeholder="465" /></Form.Item>
        <Form.Item label="发件邮箱" name="loginEmail"><Input placeholder="xxx@qq.com" /></Form.Item>
        <Form.Item label="SMTP 授权码" name="authCode" extra={
          <span>QQ 邮箱获取：登录 <a onClick={() => window.desktop.openExternal('https://mail.qq.com/')}>mail.qq.com</a> → 设置 → 账户 → 开启「SMTP 服务」→ 按提示生成授权码（不是 QQ 密码）。已配置过可留空。</span>
        }>
          <Input.Password placeholder="16 位授权码，如 abcdefghijklmnop" />
        </Form.Item>
        <Form.Item label="发件人显示名" name="senderName"><Input /></Form.Item>
        <Form.Item label="收件邮箱" name="toEmail"><Input /></Form.Item>

        <div style={{ fontWeight: 600, margin: '16px 0 8px' }}>转写（本地 whisper，不接任何大模型 API）</div>
        <Form.Item label="录制/下载完成后自动转写" name="transcribeEnabled" valuePropName="checked">
          <Switch />
        </Form.Item>
        <Form.Item label="Whisper 模型" name="whisperModel">
          <Select options={[
            { label: 'tiny（最快）', value: 'tiny' },
            { label: 'base（推荐）', value: 'base' },
            { label: 'small', value: 'small' },
            { label: 'medium', value: 'medium' },
          ]} />
        </Form.Item>
        <Form.Item label="长视频转写超时（秒）" name="pipelineTimeout"
          extra="默认 21600（6 小时）。纯 CPU 转写长视频会接近 1 小时，建议保留充足余量。">
          <Input placeholder="21600" />
        </Form.Item>
      </Form>

      <div style={{ fontWeight: 600, margin: '8px 0' }}>B站授权</div>
      <Button onClick={onBiliLogin}>打开 B站 登录 / 授权</Button>

      <div style={{ fontWeight: 600, margin: '16px 0 8px' }}>抖音授权</div>
      <Button onClick={onDouyinLogin}>打开抖音 扫码登录（短视频监控用）</Button>
      <div style={{ fontSize: 12, color: '#999', marginTop: 4 }}>登录后自动保存，用于监控/下载抖音短视频；约一个月后需重新扫码</div>
    </Modal>
  )
}
