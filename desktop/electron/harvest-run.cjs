// 独立采集 B站 Cookie 的 electron 入口（无条件执行，不依赖 require.main）。
// 用法: electron harvest-run.cjs <cookie输出路径> <状态文件路径>
const path = require('path')
const { runStandalone, DEFAULT_OUT } = require('./bili-cookie.cjs')

const out = process.argv[2] || DEFAULT_OUT
const statusFile = process.argv[3] || path.join(path.dirname(out), 'harvest_status.json')
runStandalone(out, statusFile)
