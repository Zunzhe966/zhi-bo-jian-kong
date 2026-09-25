// 界面常量（纯数据，无逻辑、无组件依赖）。
// 拆分自 App.jsx，目的：改这些选项不影响主界面代码。

export const qualityOptions = [
  { label: '原画', value: '原画' },
  { label: '蓝光', value: '蓝光' },
  { label: '超清', value: '超清' },
  { label: '高清', value: '高清' },
  { label: '标清', value: '标清' },
  { label: '流畅', value: '流畅' },
]

export const notifyOptions = [
  { label: '跟随全局', value: '跟随全局' },
  { label: '邮箱', value: '邮箱' },
  { label: '不通知', value: '不通知' },
]

export const platformOptions = [
  { label: '自动识别（推荐）', value: 'Auto' },
  { label: '抖音', value: 'DouYin' },
  { label: '快手', value: 'KuaiShou' },
  { label: 'B站', value: 'Bilibili' },
  { label: '其他（直接粘贴完整链接）', value: 'Other' },
]

export const statusText = {
  recording: '录制中',
  monitoring: '监控中，等待开播',
  noting: '未开始监控',
}
