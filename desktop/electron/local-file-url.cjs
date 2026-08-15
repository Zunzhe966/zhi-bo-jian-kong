const path = require('path')

/** 把项目内绝对路径转成 `local-file://` URL；不在项目根时返回空串。 */
function projectLocalUrl(root, filePath, scheme = 'local-file') {
  if (!filePath || typeof filePath !== 'string') return ''
  const resolved = path.resolve(filePath)
  if (resolved !== root && !resolved.startsWith(root + path.sep)) return ''
  const rel = path.relative(root, resolved).split(path.sep).map(encodeURIComponent).join('/')
  return `${scheme}://local/${rel}`
}

module.exports = { projectLocalUrl }
