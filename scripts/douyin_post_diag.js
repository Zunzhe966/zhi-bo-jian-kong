#!/usr/bin/env node
/**
 * 抖音历史视频回捞 - 诊断版
 * 目的: 精确定位 aweme/post 在无头浏览器中为什么拿不到数据
 * 诊断点: 是否验证码拦截 / 接口是否触发 / 响应体是否为空 / 是否需登录
 */
const { chromium } = require('../rsshub/node_modules/patchright');
const CHROME = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const UA = 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/151.0.0.0 Safari/537.36';
const SEC_UID = process.argv[2] || 'MS4wLjABAAAAfRq5hAWoSxwYGwDEI-F3glR1hf7mdi87_S2y1bhL7OjOUkc_ETYzRq6QwGZ_qf2m';

async function main() {
  const browser = await chromium.launch({
    executablePath: CHROME, headless: true,
    args: ['--no-sandbox', '--disable-blink-features=AutomationControlled']
  });
  const ctx = await browser.newContext({ userAgent: UA, locale: 'zh-CN' });
  const page = await ctx.newPage();

  let apiHits = 0;
  const blocked = new Set();
  // 监听所有请求/响应
  page.on('response', async (res) => {
    const url = res.url();
    if (/aweme|web\/user|web\/im|note|post|challenge/i.test(url) && /api|aweme/i.test(url)) {
      apiHits++;
      const t = await res.text().catch(()=>'');
      console.log(`\n[API] ${res.status()} len=${t.length} ${url.slice(0,110)}`);
      if (t.length < 1000) console.log(`      body: ${t.slice(0,150)}`);
    }
    if (/captcha|verify|slider|risk/i.test(url)) { blocked.add(res.status()); }
  });

  console.log('访问用户主页...');
  await page.goto(`https://www.douyin.com/user/${SEC_UID}`, { waitUntil: 'domcontentloaded', timeout: 45000 }).catch(e=>console.log('goto err:', e.message));
  await page.waitForTimeout(6000);
  // 滚动加载
  for (let i = 0; i < 4; i++) { await page.evaluate(()=>window.scrollBy(0, 2500)).catch(()=>{}); await page.waitForTimeout(2500); }
  await page.waitForTimeout(3000);

  console.log(`\n=== 诊断结果 ===`);
  console.log('API命中次数:', apiHits);
  console.log('验证码/风控响应:', [...blocked]);
  // 页面标题判断是否验证码页
  console.log('页面标题:', await page.title());
  const html = await page.content();
  console.log('页面是否含验证码:', /验证码|安全验证|captcha|滑动|risk/i.test(html));
  console.log('页面是否含视频列表容器:', /video-list|user-post|aweme-list/i.test(html));
  await browser.close();
}
main().catch(e => { console.error('FATAL:', e.message); process.exit(1); });
