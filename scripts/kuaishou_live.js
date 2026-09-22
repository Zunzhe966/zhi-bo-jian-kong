#!/usr/bin/env node
/**
 * 快手直播监控 + 推流地址提取 + 录制工具（基于 live-seam 开源机制 + 抖音脚本框架）
 *
 * 原理（借鉴开源社区 live-seam 的快手实现）:
 *  1. 免登录抓 https://live.kuaishou.com/u/{快手ID}
 *  2. 正则提取 <script>window.__INITIAL_STATE__=([\s\S]*?);\(function 拿 SSR JSON
 *  3. 从 liveroom.playList[0] 提取: liveStream.caption=标题, author.name=主播名,
 *     playUrls[0].adaptationSet.representation[] 取最高清 url = 推流地址(开播时才有)
 *     —— 与抖音 __pace_f.push 类似, 免签名/免登录, 公开页面直接带推流地址
 *
 * 用法:
 *   node kuaishou_live.js <快手ID>                          # 查一次直播状态
 *   node kuaishou_live.js <快手ID> --watch                  # 每60秒轮询监控开播
 *   node kuaishou_live.js <快手ID> --watch --record         # 监控到开播就自动录制
 *   node kuaishou_live.js <快手ID> --record                 # 当前若开播则立即录制
 */
const https = require('https');
const http = require('http');
const fs = require('fs');
const path = require('path');
const { execFile, spawnSync } = require('child_process');

const UA = 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/117.0.0.0 Safari/537.36 Edg/117.0.2045.31';
const OUT_DIR = process.env.OUT_DIR || path.join(process.cwd(), 'recordings');
const LOG_DIR = path.join(process.cwd(), 'logs');
const POLL_MS = 60 * 1000;

// ---- 自动探测 ffmpeg ----
function findFfmpeg() {
  const cands = [process.env.FFMPEG, '/Users/zhangxuetao/bin/ffmpeg', '/usr/local/bin/ffmpeg', '/opt/homebrew/bin/ffmpeg', '/usr/bin/ffmpeg'].filter(Boolean);
  for (const c of cands) { try { if (fs.existsSync(c)) { spawnSync(c, ['-version'], { stdio: 'ignore' }); return c; } } catch {} }
  const r = spawnSync('which', ['ffmpeg'], { encoding: 'utf8' });
  if (r.status === 0 && r.stdout.trim()) return r.stdout.trim();
  return null;
}
const FFMPEG = findFfmpeg();

// ---- 抓取页面 ----
function fetchHtml(url, redirects = 0) {
  return new Promise((resolve, reject) => {
    if (redirects > 5) return reject(new Error('重定向过多'));
    const mod = url.startsWith('https') ? https : http;
    const req = mod.get(url, {
      headers: { 'User-Agent': UA, 'Accept': 'text/html,application/xhtml+xml,*/*;q=0.8', 'Accept-Language': 'zh-CN,zh;q=0.9' },
      timeout: 20000
    }, (res) => {
      if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
        res.resume();
        return fetchHtml(res.headers.location, redirects + 1).then(resolve, reject);
      }
      let data = '';
      res.on('data', c => data += c);
      res.on('end', () => resolve({ status: res.statusCode, html: data }));
    });
    req.on('error', reject);
    req.on('timeout', () => { req.destroy(); reject(new Error('timeout')); });
  });
}

// ---- 解析快手页面 ----
function parseKs(html) {
  const info = { title: '', anchor: '', live: false, streams: [], raw: null, isLiving: false };
  const m = html.match(/<script>window\.__INITIAL_STATE__=([\s\S]*?);\(function/);
  if (!m) { info.error = '未找到 __INITIAL_STATE__'; return info; }
  let json;
  try { json = JSON.parse(m[1]); } catch (e) { info.error = 'JSON解析失败: ' + e.message; return info; }
  const room = json?.liveroom;
  const list = room?.playList?.[0];
  if (!list) { info.error = 'playList为空(可能未开播或无人直播)'; info.raw = room ? 'room存在但无playList' : '无liveroom'; return info; }
  const ls = list.liveStream || {};
  info.title = ls.caption || '';
  info.anchor = list.author?.name || '';
  // 直播状态: 优先用 isLiving 字段(最可靠), 兼容只靠 playUrls 判断
  info.isLiving = !!list.isLiving;
  // 错误类型: 22=未开播/浏览其他内容
  if (list.errorType && list.errorType.type) info.errType = list.errorType.type;
  // 推流地址: playUrls[0].adaptationSet.representation[] 最后一个通常是最高清
  const reps = list.liveStream?.playUrls?.[0]?.adaptationSet?.representation;
  if (Array.isArray(reps) && reps.length > 0) {
    info.live = true;
    for (let i = 0; i < reps.length; i++) {
      const u = reps[i].url;
      if (u) info.streams.push({ quality: 'rep' + i, url: u });
    }
  }
  // isLiving=true 但没解析到流(个别情况), 标记需要再抓
  if (info.isLiving && info.streams.length === 0) info.warn = 'isLiving=true但未取到推流';
  return info;
}

// ---- 清洗文件名 ----
function safeName(s, maxLen = 40) {
  if (!s) return '';
  return s.replace(/[\\/:*?"<>|\n\r\t]/g, '_').replace(/\s+/g, '_').slice(0, maxLen);
}

// ---- ffmpeg录制 ----
function recordStream(url, outFile) {
  return new Promise((resolve) => {
    if (!FFMPEG) { console.error('❌ 未找到ffmpeg'); return resolve({ ok: false }); }
    const args = ['-y', '-i', url, '-c', 'copy', '-t', '01:00:00', outFile];
    console.log(`\n🎥 开始录制 → ${path.basename(outFile)}`);
    console.log(`   ${FFMPEG} ${args.join(' ')}`);
    const child = execFile(FFMPEG, args, { timeout: 0 }, (err) => {
      if (err && !/signal/i.test(err.message)) console.log('   ffmpeg结束(可能直播中断):', err.message.slice(0, 100));
      resolve({ ok: true });
    });
    child.stdout?.on('data', d => process.stdout.write('   ' + d.toString()));
    child.stderr?.on('data', d => {
      const s = d.toString();
      if (/frame=|time=/i.test(s)) process.stdout.write('\r   ' + s.trim().slice(0, 70));
    });
  });
}

function log(msg) {
  fs.mkdirSync(LOG_DIR, { recursive: true });
  const line = `[${new Date().toLocaleString('zh-CN')}] ${msg}`;
  console.log(line);
  try { fs.appendFileSync(path.join(LOG_DIR, 'kuaishou_live.log'), line + '\n'); } catch {}
}

async function checkLive(ksId) {
  const url = `https://live.kuaishou.com/u/${ksId}`;
  const { status, html } = await fetchHtml(url);
  if (status !== 200) return { error: `HTTP ${status}` };
  const info = parseKs(html);
  return { ...info, pageSize: html.length };
}

async function main() {
  const args = process.argv.slice(2);
  const id = args[0];
  const watch = args.includes('--watch');
  const record = args.includes('--record');
  if (!id) { console.error('用法: node kuaishou_live.js <快手ID> [--watch] [--record]'); process.exit(1); }
  if (record && !FFMPEG) { console.error('❌ 未找到ffmpeg'); process.exit(1); }
  fs.mkdirSync(OUT_DIR, { recursive: true });
  log(`启动监控 → live.kuaishou.com/u/${id} | watch=${watch} record=${record}`);

  const doCheck = async () => {
    const r = await checkLive(id).catch(() => ({ error: '网络错误' }));
    const t = new Date().toLocaleTimeString('zh-CN');
    if (r.error) { log(`[${t}] ${r.error}`); return null; }
    const statusLine = r.live ? '🔴 直播中' : '⚪ 未直播';
    log(`[${t}] ${statusLine} | 主播:${r.anchor || '-'} | 标题:${r.title || '-'} | 清晰度:${r.streams.length} | 页面:${r.pageSize}B`);
    if (r.live && r.streams[0]) log(`     推流地址: ${r.streams[0].url.slice(0, 90)}...`);
    return r;
  };

  const recordOnce = async (r) => {
    if (!r.live || r.streams.length === 0) { log('   ⚠️ 无推流地址'); return false; }
    const st = r.streams[r.streams.length - 1]; // 最高清
    const fname = `${id}_${new Date().toISOString().slice(0,19).replace(/[:T]/g,'-')}${r.title?('_'+safeName(r.title)):''}.${/\.m3u8/i.test(st.url)?'m3u8':'ts'}`;
    const outFile = path.join(OUT_DIR, fname);
    log(`   🎬 开始录制 → ${fname}`);
    await recordStream(st.url, outFile);
    let size = 0; try { size = fs.statSync(outFile).size; } catch {}
    log(`   📦 录制结束, 大小: ${(size/1024/1024).toFixed(2)}MB`);
    return true;
  };

  if (!watch) {
    const r = await doCheck();
    if (record && r && r.live) await recordOnce(r);
    return;
  }

  log('开始持续监控 (每60秒一次, Ctrl+C停止)...');
  let recording = false;
  const tick = async () => {
    const r = await doCheck();
    if (!record || !r || !r.live || recording) return;
    recording = true;
    try { await recordOnce(r); } catch (e) { log('   录制异常: ' + e.message); }
    recording = false;
  };
  await tick();
  setInterval(tick, POLL_MS);
  await new Promise(() => {});
}

main().catch(e => { console.error('FATAL:', e.message); process.exit(1); });
