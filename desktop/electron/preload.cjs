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
  showItem: (p) => ipcRenderer.invoke('show-item', p),
  // 列一层目录（只读、白名单内），主播详情页用它显示「这个主播存了哪些文件」。
  listDir: (p) => ipcRenderer.invoke('list-dir', p),
  openExternal: (url) => ipcRenderer.invoke('open-external', url),
  readText: (p) => ipcRenderer.invoke('read-text', p),
  // 封面图：历史视频列表用它把 core 给的封面缓存路径换成 <img> 能直接吃的地址。
  coverUrl: (p) => ipcRenderer.invoke('cover-url', p),
  // 数据目录查询/切换：界面可以显示「文件到底存哪」，并让用户换目录。
  getPaths: () => ipcRenderer.invoke('get-paths'),
  chooseDataDir: () => ipcRenderer.invoke('choose-data-dir'),
  chooseDir: (opts) => ipcRenderer.invoke('choose-dir', opts || {}),
  chooseFile: (opts) => ipcRenderer.invoke('choose-file', opts || {}),
  // 检查更新：只替换程序，数据目录不动。更新源 = <数据目录>/releases/
  // quiet=true 时「已是最新版本」不弹窗，交给界面自己 toast。
  checkUpdate: (opts) => ipcRenderer.invoke('check-update', opts || {}),
  biliLogin: () => ipcRenderer.invoke('bili-login'),
  douyinLogin: () => ipcRenderer.invoke('douyin-login'),
})
