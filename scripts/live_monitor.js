#!/usr/bin/env node
/**
 * 统一直播监控入口 (抖音/快手自研 + yt-dlp多平台)
 *
 * 统一调度三类直播源:
 *  [douyin]  live.douyin.com/{id}    → 自研: 页面__pace_f.push提取stream_url
 *  [kuaishou] live.kuaishou.com/u/{id} → 自研: 页面__INITIAL_STATE__提取playUrls
 *  [ytdlp]   其它(B站/虎牙/斗鱼/Twitch等) → yt-dlp拿推流
 *
 * 用法:
 *   node live_monitor.js list                                   # 列出配置里的所有监控源
 *   node live_monitor.js check <url或id>                        # 查一次某个源直播状态
 *   node live_monitor.js watch                                  # 持续监控配置里所有源,开播自动录制
 *   node live_monitor.js watch --dry                            # 只监控不录制(调试)
 */
const fs = require('fs');
const path = require('path');
const https = require('https');
const http = require('http');
const { execFile, execFileSync } = require('child_process');

const ROOT = __dirname;
const CONFIG = process.env.CONFIG || path.join(ROOT, '..', 'config', '视频监控列表.ini');
const OUT_DIR = process.env.OUT_DIR || path.join(process.cwd(), 'recordings');
const LOG_DIR = path.join(process.cwd(), 'logs');
const POLL_MS = 60 * 1000;
const UA = 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/151.0.0.0 Safari/537.36';

const YTDLP = process.env.YTDLP || '/Users/zhangxuetao/.workbuddy/binaries/python/versions/3.13.12/bin/yt-dlp';

// ---- 探测ffmpeg ----
function findFfmpeg() {
  const cands = [process.env.FFMPEG, '/Users/zhangxuetao/bin/ffmpeg', '/usr/local/bin/ffmpeg', '/opt/homebrew/bin/ffmpeg', '/usr/bin/ffmpeg'].filter(Boolean);
  for (const c of cands) { try { if (fs.existsSync(c)) return c; } catch {} }
  const r = execFileSync('which', ['ffmpeg'], { encoding: 'utf8', stdio: ['ignore','pipe','ignore'] });
  return r.trim() || null;
}
const FFMPEG = findFfmpeg();

// ---- 读配置 ----
function readConfig() {
  if (!fs.existsSync(CONFIG)) return [];
  const lines = fs.readFileSync(CONFIG, 'utf8').split('\n');
  const out = [];
  for (const raw of lines) {
    const line = raw.trim();
    if (!line || line.startsWith('#') || line.startsWith('﻿#')) continue;
    const parts = line.split(',').map(s => s.trim());
    const addr = parts[0];
    const name = parts[1] || addr;
    const download = parts.some(p => p.includes('下载=是'));
    out.push({ addr, name, download, type: detectType(addr) });
  }
  return out;
}
function detectType(addr) {
  if (/live\.douyin\.com|douyin\.com/.test(addr)) return 'douyin';
  if (/live\.kuaishou\.com|kuaishou/.test(addr)) return 'kuaishou';
  return 'ytdlp';
}

// ---- 通用抓HTML ----
function fetchHtml(url, redirects = 0) {
  return new Promise((resolve, reject) => {
    if (redirects > 5) return reject(new Error('redirect过多'));
    const mod = url.startsWith('https') ? https : http;
    const req = mod.get(url, { headers: { 'User-Agent': UA }, timeout: 20000 }, (res) => {
      if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) { res.resume(); return fetchHtml(res.headers.location, redirects + 1).then(resolve, reject); }
      let d = ''; res.on('data', c => d += c); res.on('end', () => resolve({ status: res.statusCode, html: d }));
    });
    req.on('error', reject); req.on('timeout', () => { req.destroy(); reject(new Error('timeout')); });
  });
}

// ---- 抖音解析 (复用douyin_live.js逻辑) ----
function parseDouyin(html) {
  const clean = html.replace(/\\/g, '');
  const info = { live: false, title: '', roomId: null, userCount: '', streamUrl: null, anchor: '' };
  const roomIdx = clean.indexOf('"room":{');
  if (roomIdx >= 0) {
    const seg = clean.slice(roomIdx, roomIdx + 800);
    const id = seg.match(/"id_str":"(\d{15,})"/);
    const st = seg.match(/"status":(\d)/);
    const ti = seg.match(/"title":"([^"]*?)"/);
    const uc = seg.match(/"user_count_str":"([^"]*?)"/);
    if (id) info.roomId = id[1];
    if (st) info.status = parseInt(st[1]);
    if (ti) info.title = ti[1];
    if (uc) info.userCount = uc[1];
  }
  info.live = info.status === 2;
  const su = clean.match(/"stream_url":(\{[^}]*\}|null)/);
  if (su && su[1] !== 'null') {
    try {
      const s = JSON.parse(su[1]);
      if (s.flv_pull_url) { const order = ['ORIGIN','FULL_HD1','FULL_HD','HD1','HD','SD1','SD','LD']; for (const k of order) if (s.flv_pull_url[k]?.url) { info.streamUrl = s.flv_pull_url[k].url; break; } }
      if (!info.streamUrl && s.hls_pull_url_map) { const h = s.hls_pull_url_map; const k = Object.keys(h)[0]; if (h[k]?.url) info.streamUrl = h[k].url; }
    } catch (e) {}
  }
  const secs = [...new Set(clean.match(/MS4wLjAB[A-Za-z0-9_-]+/g) || [])];
  info.secUid = secs[0] || null;
  return info;
}

// ---- 快手解析 (复用kuaishou_live.js逻辑) ----
function parseKuaishou(html) {
  const info = { live: false, title: '', anchor: '', streamUrl: null };
  const m = html.match(/<script>window\.__INITIAL_STATE__=([\s\S]*?);\(function/);
  if (!m) return { ...info, error: '无__INITIAL_STATE__' };
  let json; try { json = JSON.parse(m[1]); } catch (e) { return { ...info, error: 'JSON解析失败' }; }
  const list = json?.liveroom?.playList?.[0];
  if (!list) return { ...info, error: '无playList' };
  info.title = list.liveStream?.caption || '';
  info.anchor = list.author?.name || '';
  info.isLiving = !!list.isLiving;
  const reps = list.liveStream?.playUrls?.[0]?.adaptationSet?.representation;
  if (Array.isArray(reps) && reps.length > 0) { info.live = true; const last = reps[reps.length - 1]; info.streamUrl = last.url; }
  return info;
}

// ---- yt-dlp查直播(拿推流) ----
function ytdlpInfo(url) {
  return new Promise((resolve) => {
    execFile(YTDLP, ['-J', '--no-warnings', url], { timeout: 30000, maxBuffer: 10 * 1024 * 1024 }, (err, stdout) => {
      if (err) { resolve({ error: (err.message || '').slice(0, 200), live: false }); return; }
      try {
        const j = JSON.parse(stdout);
        const live = !!j.is_live || !!(j.live_status && j.live_status !== 'not_live' && j.live_status !== 'post_live');
        resolve({ live, title: j.title || '', anchor: j.uploader || j.channel || '', streamUrl: j.url || j.webpage_url || null, error: null });
      } catch (e) { resolve({ error: '解析yt-dlp输出失败', live: false }); }
    });
  });
}

// ---- 统一查一个源 ----
async function checkSource(src) {
  if (src.type === 'douyin') {
    const id = src.addr.match(/live\.douyin\.com\/([A-Za-z0-9_-]+)/)?.[1] || src.addr.split('/').pop();
    const { html } = await fetchHtml(`https://live.douyin.com/${id}`).catch(() => ({ html: '' }));
    return { ...parseDouyin(html), id, platform: '抖音', name: src.name };
  }
  if (src.type === 'kuaishou') {
    const id = src.addr.match(/live\.kuaishou\.com\/u\/([A-Za-z0-9_-]+)/)?.[1] || src.addr.split('/').pop();
    const { html } = await fetchHtml(`https://live.kuaishou.com/u/${id}`).catch(() => ({ html: '' }));
    return { ...parseKuaishou(html), id, platform: '快手', name: src.name };
  }
  // ytdlp平台
  const info = await ytdlpInfo(src.addr);
  return { ...info, id: src.addr, platform: 'ytdlp', name: src.name };
}

// ---- 录制 ----
function recordStream(url, outFile) {
  return new Promise((resolve) => {
    if (!FFMPEG) { console.error('❌ 未找到ffmpeg'); return resolve(false); }
    const args = ['-y', '-i', url, '-c', 'copy', '-t', '01:00:00', outFile];
    console.log(`   🎥 录制中: ${path.basename(outFile)}`);
    const child = execFile(FFMPEG, args, { timeout: 0 }, (err) => { if (err && !/signal/i.test(err.message)) console.log('   ffmpeg结束:', err.message.slice(0, 80)); resolve(true); });
    child.stderr?.on('data', d => { const s = d.toString(); if (/time=/i.test(s)) process.stdout.write('\r   ' + s.trim().slice(0, 60)); });
  });
}
function safeName(s, n = 40) { if (!s) return ''; return s.replace(/[\\/:*?"<>|\n]/g, '_').replace(/\s+/g, '_').slice(0, n); }
function log(msg) {
  fs.mkdirSync(LOG_DIR, { recursive: true });
  const line = `[${new Date().toLocaleString('zh-CN')}] ${msg}`;
  console.log(line);
  try { fs.appendFileSync(path.join(LOG_DIR, 'live_monitor.log'), line + '\n'); } catch {}
}

async function main() {
  const args = process.argv.slice(2);
  const cmd = args[0] || 'list';
  const dry = args.includes('--dry');

  if (cmd === 'list') {
    const srcs = readConfig();
    console.log(`监控配置: ${CONFIG}`);
    console.log(`共 ${srcs.length} 个源 | ffmpeg:${FFMPEG || '未找到'}\n`);
    for (const s of srcs) console.log(`  [${s.type}] ${s.name}  ${s.addr}  ${s.download ? '(下载)' : ''}`);
    if (srcs.length === 0) console.log('  (配置为空, 请编辑 config/视频监控列表.ini)');
    return;
  }

  if (cmd === 'check') {
    const url = args[1];
    if (!url) { console.error('用法: live_monitor.js check <url>'); process.exit(1); }
    const src = { addr: url, name: url, type: detectType(url) };
    const r = await checkSource(src);
    console.log(`平台:${r.platform} | 直播中:${r.live ? '🔴是' : '⚪否'} | 标题:${r.title || '-'} | 主播:${r.anchor || '-'}`);
    if (r.streamUrl) console.log(`推流: ${r.streamUrl.slice(0, 110)}...`);
    if (r.error) console.log('状态:', r.error);
    return;
  }

  if (cmd === 'watch') {
    const srcs = readConfig();
    if (srcs.length === 0) { console.error('配置为空'); process.exit(1); }
    fs.mkdirSync(OUT_DIR, { recursive: true });
    log(`启动统一监控 ${srcs.length} 个源 | dry=${dry} | 轮询${POLL_MS / 1000}s`);
    const recording = new Set();
    const tick = async () => {
      for (const src of srcs) {
        const key = src.addr;
        if (recording.has(key)) continue;
        try {
          const r = await checkSource(src);
          const statusLine = r.live ? '🔴直播中' : '⚪未直播';
          if (r.live) {
            log(`[${r.platform}] ${r.name} ${statusLine} | ${r.title || ''} | 观看:${r.userCount || r.anchor || '-'}`);
            if (!dry && r.streamUrl && src.download) {
              recording.add(key);
              const ext = /\.m3u8/i.test(r.streamUrl) ? 'm3u8' : (/\.flv/i.test(r.streamUrl) ? 'flv' : 'ts');
              const f = `${safeName(src.name)}_${Date.now()}_${r.platform}.${ext}`;
              await recordStream(r.streamUrl, path.join(OUT_DIR, f));
              recording.delete(key);
            }
          } else if (r.error && !r.live) {
            log(`[${r.platform}] ${r.name} ${statusLine} (${r.error || ''})`);
          } else {
            log(`[${r.platform}] ${r.name} ${statusLine}`);
          }
        } catch (e) { log(`[${src.platform || '?'}] ${src.name} 检查异常: ${e.message}`); }
      }
    };
    await tick();
    setInterval(tick, POLL_MS);
    await new Promise(() => {});
  }

  console.error('未知命令: ' + cmd + ' (可选 list/check/watch)');
  process.exit(1);
}

main().catch(e => { console.error('FATAL:', e.message); process.exit(1); });
