#!/usr/bin/env node
/**
 * 抖音监控核心接口综合测试(基于真实sec_uid)
 * 1. webcast 直播接口(备份软件核心): room/reflow/info
 * 2. aweme/post 视频列表接口(带浏览器签名上下文)
 */
const { chromium } = require('../rsshub/node_modules/patchright');

const CHROME = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const UA = 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/151.0.0.0 Safari/537.36';
const SEC_UID = 'MS4wLjABAAAAfRq5hAWoSxwYGwDEI-F3glR1hf7mdi87_S2y1bhL7OjOUkc_ETYzRq6QwGZ_qf2m';

async function main() {
  const browser = await chromium.launch({
    executablePath: CHROME, headless: true,
    args: ['--no-sandbox', '--disable-blink-features=AutomationControlled']
  });
  const ctx = await browser.newContext({ userAgent: UA, locale: 'zh-CN' });
  await ctx.addInitScript(() => {
    Object.defineProperty(navigator, 'webdriver', { get: () => undefined });
  });
  const page = await ctx.newPage();

  await page.goto('https://www.douyin.com/', { waitUntil: 'domcontentloaded', timeout: 40000 }).catch(()=>{});
  await page.waitForTimeout(3000);

  // 测试1: webcast 直播接口 (备份软件核心链路)
  console.log('=== 测试1: webcast room/reflow/info 直播接口 ===');
  const reflow = await page.evaluate(async (sec) => {
    const url = `https://webcast.amemv.com/webcast/room/reflow/info/?type_id=0&live_id=1&sec_user_id=${sec}`;
    const r = await fetch(url, { headers: { 'referer': 'https://live.douyin.com/', 'accept': 'application/json' } });
    return await r.text();
  }, SEC_UID);
  console.log('reflow响应:', reflow.slice(0, 300));
  console.log('');

  // 测试2: aweme/post 视频列表(手动调用, 带浏览器上下文签名)
  console.log('=== 测试2: aweme/post 视频列表 ===');
  const post = await page.evaluate(async (sec) => {
    const qs = new URLSearchParams({
      device_platform:'webapp', aid:'6383', channel:'channel_pc_web', pc_client_type:'1',
      pc_libra_divert:'Mac', version_code:'170400', version_name:'17.4.0', cookie_enabled:'true',
      platform:'PC', max_cursor:'0', count:'18', sec_user_id: sec, publish_video_strategy_type:'2',
      source:'channel_pc_web'
    });
    const r = await fetch('https://www.douyin.com/aweme/v1/web/aweme/post/?'+qs, {
      headers: { 'referer': `https://www.douyin.com/user/${sec}`, 'accept': 'application/json' }
    });
    return { status: r.status, text: (await r.text()).slice(0, 300) };
  }, SEC_UID);
  console.log('post状态:', post.status);
  console.log('post响应:', post.text);

  // 测试3: web 用户页完整加载后, 直接读DOM里的视频数据(RENDER_DATA)
  console.log('\n=== 测试3: 用户页RENDER_DATA里的视频 ===');
  await page.goto(`https://www.douyin.com/user/${SEC_UID}`, { waitUntil: 'domcontentloaded', timeout: 45000 }).catch(()=>{});
  await page.waitForTimeout(6000);
  const rd = await page.evaluate(() => {
    const s = document.getElementById('RENDER_DATA');
    if (!s) return '无RENDER_DATA';
    let t = s.textContent;
    try { t = decodeURIComponent(t); } catch(e){}
    return t;
  });
  // 检查是否有aweme_list
  const hasList = rd.includes('aweme_list');
  const videoCount = (rd.match(/"aweme_id":"/g)||[]).length;
  console.log('RENDER_DATA大小:', rd.length, '| 含aweme_list:', hasList, '| 含aweme_id:', videoCount);
  // 提取视频
  if (videoCount > 0) {
    const ids = rd.match(/"aweme_id":"(\d+)"/g) || [];
    const descs = rd.match(/"desc":"([^"]{0,30})"/g) || [];
    console.log('前5个视频:');
    ids.slice(0,5).forEach((id,i) => console.log('  ', id, '|', descs[i]||''));
  }

  await browser.close();
}

main().catch(e => { console.error('FATAL:', e.message); process.exit(1); });
