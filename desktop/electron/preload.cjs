const { contextBridge, ipcRenderer } = require('electron')

contextBridge.exposeInMainWorld('desktop', {
  cli: (cmd, ...args) => ipcRenderer.invoke('cli', cmd, ...args),
  reloadUi: () => ipcRenderer.invoke('reload-ui'),
  setLoginItem: (enabled) => ipcRenderer.invoke('set-login-item', enabled),
  getLoginItem: () => ipcRenderer.invoke('get-login-item'),
  showLocalNotify: (title, body) => ipcRenderer.invoke('show-local-notify', title, body),
  openPath: (p) => ipcRenderer.invoke('open-path', p),
  openExternal: (url) => ipcRenderer.invoke('open-external', url),
  readText: (p) => ipcRenderer.invoke('read-text', p),
  biliLogin: () => ipcRenderer.invoke('bili-login'),
})
