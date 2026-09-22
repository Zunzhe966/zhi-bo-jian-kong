#!/usr/bin/env node
/**
 * 抖音历史视频回捞 - 深挖版
 * 重点: 检查 aweme/post 请求的 cookie/签名 与 响应头, 确认是否因缺登录态返回空
 */
const { chromium } = require('../rsshub/node_modules/patchright');
const CHROME = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const UA = 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/151.0.0.0 Safari/537.36';
const SEC_UID = process.argv[2] || 'MS4wLjABAAAAfRq5hAWoSxwYGwDEI-F3glR1hf7mdi87_S2y1bhL7OjOUkc_ETYzRq6QwGZ_qf2m';

async function main() {
  const browser = await chromium.launch({ executablePath: CHROME, headless: true, args: ['--no-sandbox','--disable-blink-features=AutomationControlled'] });
  const ctx = await browser.newContext({ userAgent: UA, locale: 'zh-CN' });
  const page = await ctx.newPage();

  page.on('request', (req) => {
    if (!req.url().includes('/aweme/post')) return;
    const headers = req.headers();
    const cookie = headers.cookie || '';
    console.log('\n[aweme/post 请求头分析]');
    console.log('  有cookie:', !!cookie);
    console.log('  cookie摘要:', cookie.slice(0, 120));
    console.log('  含sessionid:', /sessionid/.test(cookie));
    console.log('  含msToken:', /msToken/.test(cookie));
    console.log('  含a_bogus/X-Bogus签名:', /a_bogus|X-Bogus|x-bogus/i.test(JSON.stringify(headers)));
    console.log('  全部header keys:', Object.keys(headers).join(','));
  });
  page.on('response', (res) => {
    if (!res.url().includes('/aweme/post')) return;
    console.log('[aweme/post 响应头]');
    const h = res.headers();
    console.log('  content-type:', h['content-type']);
    console.log('  content-length:', h['content-length']);
    console.log('  server:', h['server']);
    console.log('  x-xxx:', h['x-'] || '无');
  });

  await page.goto(`https://www.douyin.com/user/${SEC_UID}`, { waitUntil: 'domcontentloaded', timeout: 45000 }).catch(e=>console.log('goto:', e.message));
  await page.waitForTimeout(5000);
  for (let i = 0; i < 5; i++) { await page.evaluate(()=>window.scrollBy(0,2500)).catch(()=>{}); await page.waitForTimeout(2500); }
  await page.waitForTimeout(3000);

  // 检查当前cookie里的关键字段
  const cookies = await ctx.cookies();
  console.log('\n=== 当前页面cookie ===');
  const names = cookies.map(c=>c.name);
  console.log('cookie字段:', names.join(', '));
  console.log('含sessionid:', names.includes('sessionid'));
  console.log('含sessionid_ss:', names.includes('sessionid_ss'));
  console.log('含uid_tt:', names.includes('uid_tt'));
  await browser.close();
}
main().catch(e => { console.error('FATAL:', e.message); process.exit(1); });
