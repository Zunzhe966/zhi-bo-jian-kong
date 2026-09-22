#!/usr/bin/env node
/**
 * 捕获抖音页面自身发出的 aweme/post 响应体
 * 用 page.route 拦截, 立即读取body, 保存为JSON
 */
const { chromium } = require('../rsshub/node_modules/patchright');
const fs = require('fs');

const CHROME = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const UA = 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/151.0.0.0 Safari/537.36';
const SEC_UID = 'MS4wLjABAAAAfRq5hAWoSxwYGwDEI-F3glR1hf7mdi87_S2y1bhL7OjOUkc_ETYzRq6QwGZ_qf2m';

async function main() {
  const browser = await chromium.launch({
    executablePath: CHROME, headless: true,
    args: ['--no-sandbox', '--disable-blink-features=AutomationControlled']
  });
  const ctx = await browser.newContext({ userAgent: UA, locale: 'zh-CN', viewport: { width: 1280, height: 2400 } });
  const page = await ctx.newPage();

  let postBodies = [];
  await page.route('**/aweme/v1/web/aweme/post/**', async (route) => {
    const resp = await route.fetch();
    let body = '';
    try { body = await resp.text(); } catch(e){ body = ''; }
    console.log(`[route] aweme/post status=${resp.status()} bodyLen=${body.length} hasList=${body.includes('aweme_list')}`);
    if (body.includes('aweme_list')) postBodies.push(body);
    await route.fulfill({ response: resp, body });
  });

  await page.goto('https://www.douyin.com/', { waitUntil: 'domcontentloaded', timeout: 40000 }).catch(()=>{});
  await page.waitForTimeout(3000);

  await page.goto(`https://www.douyin.com/user/${SEC_UID}`, { waitUntil: 'domcontentloaded', timeout: 45000 }).catch(()=>{});
  await page.waitForTimeout(8000);
  console.log('标题:', await page.title().catch(()=>''));

  // 多次滚动触发分页
  for (let i = 0; i < 15; i++) {
    await page.evaluate(() => window.scrollBy(0, 3000)).catch(()=>{});
    await page.waitForTimeout(2500);
  }

  await browser.close();

  console.log(`\n捕获到 ${postBodies.length} 个aweme/post响应`);
  if (postBodies.length > 0) {
    const allVideos = [];
    const seen = new Set();
    for (const b of postBodies) {
      try {
        const d = JSON.parse(b);
        (d.aweme_list || []).forEach(v => {
          if (!seen.has(v.aweme_id)) { seen.add(v.aweme_id); allVideos.push({ id:v.aweme_id, desc:(v.desc||'').slice(0,50), createTime:v.create_time?new Date(v.create_time*1000).toISOString().slice(0,10):'', videoUrl:(v.video?.play_addr?.url_list||[])[0]||'' }); }
        });
      } catch(e){}
    }
    console.log(`共 ${allVideos.length} 个视频`);
    allVideos.slice(0,20).forEach(v => console.log(`  ${v.createTime} | ${v.id} | ${v.desc}`));
    fs.writeFileSync('/tmp/dy_videos_list.json', JSON.stringify(allVideos, null, 2));
    console.log('已保存到 /tmp/dy_videos_list.json');
  }
}

main().catch(e => { console.error('FATAL:', e.message); process.exit(1); });
