const { contextBridge, ipcRenderer } = require('electron')

contextBridge.exposeInMainWorld('desktop', {
  cli: (cmd, ...args) => ipcRenderer.invoke('cli', cmd, ...args).catch((e) => {
    const raw = String(e && e.message ? e.message : e || '')
    // 最后一道保险：即使主进程漏掉了某个英文错误，界面也只显示中文提示。
    const friendly = /panicked at|byte index is not a char boundary|RUST_BACKTRACE/.test(raw)
      ? '操作失败：后台程序处理中文内容时出错，请重试。'
      : raw
    throw new Error(friendly)
  }),
  reloadUi: () => ipcRenderer.invoke('reload-ui'),
  setLoginItem: (enabled) => ipcRenderer.invoke('set-login-item', enabled),
  getLoginItem: () => ipcRenderer.invoke('get-login-item'),
  showLocalNotify: (title, body) => ipcRenderer.invoke('show-local-notify', title, body),
  openPath: (p) => ipcRenderer.invoke('open-path', p),
  openExternal: (url) => ipcRenderer.invoke('open-external', url),
  readText: (p) => ipcRenderer.invoke('read-text', p),
  coverUrl: (p) => ipcRenderer.invoke('cover-url', p),
  biliLogin: () => ipcRenderer.invoke('bili-login'),
})
