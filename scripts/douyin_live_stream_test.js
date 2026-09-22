#!/usr/bin/env node
/**
 * 抖音直播推流地址提取验证 (对齐开源 douyin-downloader + 备份软件原理)
 * 核心接口: live.douyin.com/webcast/room/web/enter/
 *  入参: room_id, sec_user_id, webrid, device_platform=web 等
 *  返回: data.room.stream_url.flv_pull_url / hls_pull_url_map
 * 关键: 该接口可能需要 X-Bogus 签名; 本测试用浏览器环境内 fetch 复用页面自身令牌绕过
 *
 * 用法: node douyin_live_stream_test.js <room_id> [--sec <sec_uid>] [--webrid <抖音号>]
 */
const { chromium } = require('../rsshub/node_modules/patchright');
const CHROME = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const UA = 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/151.0.0.0 Safari/537.36';

async function main() {
  const roomId = process.argv[2] || '7678106778139380534';
  const secUid = process.argv.find(a=>a.startsWith('--sec='))?.split('=')[1] || 'MS4wLjABAAAAfRq5hAWoSxwYGwDEI-F3glR1hf7mdi87_S2y1bhL7OjOUkc_ETYzRq6QwGZ_qf2m';
  const webrid = process.argv.find(a=>a.startsWith('--webrid='))?.split('=')[1] || 'Glx6768';
  console.log(`room_id=${roomId}\nwebrid=${webrid}\nsec_uid=${secUid.slice(0,30)}...`);

  const browser = await chromium.launch({ executablePath: CHROME, headless: true, args: ['--no-sandbox','--disable-blink-features=AutomationControlled'] });
  const ctx = await browser.newContext({ userAgent: UA, locale: 'zh-CN' });
  const page = await ctx.newPage();

  // 先访问直播页, 让页面环境带上抖音的cookie/令牌上下文
  console.log('\n[1] 访问直播页建立上下文...');
  await page.goto('https://live.douyin.com/' + webrid, { waitUntil: 'domcontentloaded', timeout: 40000 }).catch(()=>{});
  await page.waitForTimeout(4000);

  // 在页面内 fetch enter 接口
  console.log('[2] 页面内调用 /webcast/room/web/enter/ ...');
  const result = await page.evaluate(async ({roomId, secUid, webrid}) => {
    const params = {
      aid: '6383',
      app_name: 'douyin_web',
      live_id: '1',
      device_platform: 'web',
      language: 'zh-CN',
      enter_from: 'web_live',
      room_id: roomId,
      sec_user_id: secUid,
      webrid: webrid,
    };
    const qs = new URLSearchParams(params).toString();
    const url = 'https://live.douyin.com/webcast/room/web/enter/?' + qs;
    try {
      const r = await fetch(url, {
        headers: {
          'accept': 'application/json, text/plain, */*',
          'referer': 'https://live.douyin.com/' + webrid,
          'user-agent': navigator.userAgent,
        }
      });
      const body = await r.text();
      return { status: r.status, len: body.length, head: body.slice(0, 200), body };
    } catch(e) {
      return { error: e.message };
    }
  }, { roomId, secUid, webrid });

  console.log('HTTP status:', result.status, '| 返回长度:', result.len);
  if (result.error) { console.log('fetch错误:', result.error); }
  if (result.body) {
    try {
      const d = JSON.parse(result.body);
      console.log('status_code:', d.status_code, d.status_msg || '');
      const room = d.data?.room || {};
      console.log('status:', room.status);
      const stream = room.stream_url || {};
      console.log('--- flv_pull_url ---');
      console.log(JSON.stringify(stream.flv_pull_url || {}, null, 2));
      console.log('--- hls_pull_url_map ---');
      console.log(JSON.stringify(stream.hls_pull_url_map || {}, null, 2));
      if (stream.flv_pull_url) {
        const origin = stream.flv_pull_url['ORIGIN'] || stream.flv_pull_url['FULL_HD1'] || stream.flv_pull_url['HD1'];
        console.log('\n✅ 最高清推流地址:', origin?.url ? origin.url.slice(0,120) + '...' : '(无url)');
      }
    } catch(e) {
      console.log('响应非JSON, 原始head:', result.head);
    }
  }
  await browser.close();
}

main().catch(e => { console.error('FATAL:', e.message); process.exit(1); });
