const { app, BrowserWindow, session } = require('electron')
const fs = require('fs')
const path = require('path')
const OUT = path.join(app.getPath('temp'), 'bili_test_result.txt')
app.setPath('userData', path.join(app.getPath('temp'), 'fupan-bili-test'))
app.disableHardwareAcceleration()
function log(m){ fs.appendFileSync(OUT, `[${Date.now()}] ${m}\n`) }
try { fs.writeFileSync(OUT, 'start\n') } catch(e){}
const hardStop = setTimeout(() => { log('HARD TIMEOUT exit'); app.exit(2) }, 45000)
app.whenReady().then(async () => {
  log('ready')
  const ses = session.fromPartition('persist:bilibili')
  const win = new BrowserWindow({ show: false, webPreferences: { partition: 'persist:bilibili' } })
  log('window created')
  await Promise.race([
    win.loadURL('https://www.bilibili.com/').then(()=>log('loadURL done')).catch(e=>log('loadURL err '+e.message)),
    new Promise(r=>setTimeout(()=>{log('loadURL timeout');r()}, 15000)),
  ])
  await new Promise(r=>setTimeout(r, 5000))
  const cookies = await ses.cookies.get({ domain: '.bilibili.com' })
  log('cookie count = ' + cookies.length)
  log('cookie names = ' + cookies.map(c=>c.name).join(','))
  clearTimeout(hardStop)
  win.destroy()
  app.exit(0)
}).catch(e => { log('whenReady err ' + e.message); app.exit(3) })
