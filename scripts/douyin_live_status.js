#!/usr/bin/env node
/**
 * 抖音直播状态查询（免登录）——供 Rust scheduler 轮询调用
 *
 * 原理：打开用户主页 user/{sec_uid}，页面自动请求带签名的 profile/other 接口，
 *       拦截响应拿 live_status / room_id / unique_id（抖音号）/ nickname。
 *       live_status == 2 表示正在直播；开播时 room_id_str / web_rid 有值。
 *
 * 用法: node douyin_live_status.js <sec_uid 或主页URL>
 * 输出: 一行 JSON：{"ok":true,"nickname":"...","unique_id":"...","live_status":0,"room_id_str":"...","web_rid":"..."}
 */
const { chromium } = require('../rsshub/node_modules/patchright');

async function main() {
  let arg = (process.argv[2] || '').trim();
  if (!arg) { console.error(JSON.stringify({ ok: false, error: '缺少 sec_uid' })); process.exit(1); }
  const m = arg.match(/\/user\/([A-Za-z0-9_-]+)/);
  const secUid = m ? m[1] : arg;
  if (!secUid.startsWith('MS4wLjAB')) { console.error(JSON.stringify({ ok: false, error: '不是有效 sec_uid' })); process.exit(1); }

  const browser = await chromium.launch({
    executablePath: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
    headless: true, args: ['--no-sandbox'],
  });
  try {
    const ctx = await browser.newContext({
      userAgent: 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36',
      locale: 'zh-CN',
    });
    const page = await ctx.newPage();
    const result = await new Promise((resolve) => {
      let settled = false;
      const done = (obj) => { if (!settled) { settled = true; resolve(obj); } };
      page.on('response', (resp) => {
        const url = resp.url();
        if (!url.includes('profile/other')) return;
        const ct = resp.headers()['content-type'] || '';
        if (!ct.includes('json')) return;
        resp.text().then((txt) => {
          const g = (re) => { const x = txt.match(re); return x ? x[1] : ''; };
          done({
            ok: true,
            nickname: g(/"nickname":"([^"]+)/),
            unique_id: g(/"unique_id":"([^"]+)/),
            live_status: parseInt(g(/"live_status"\s*:\s*([0-9])/)) || 0,
            room_id_str: g(/"room_id_str":"([^"]+)/),
            web_rid: g(/"web_rid":"([^"]+)/),
          });
        }).catch((e) => done({ ok: false, error: '解析响应失败: ' + e.message.slice(0, 60) }));
      });
      page.goto(`https://www.douyin.com/user/${secUid}`, { waitUntil: 'domcontentloaded', timeout: 40000 })
        .catch((e) => done({ ok: false, error: '页面加载失败: ' + e.message.slice(0, 60) }));
      setTimeout(() => done({ ok: false, error: '超时：10秒内未拿到 profile 接口响应' }), 10000);
    });
    console.log(JSON.stringify(result));
  } finally {
    await browser.close();
  }
}

main().catch((e) => { console.error(JSON.stringify({ ok: false, error: e.message.slice(0, 120) })); process.exit(1); });
