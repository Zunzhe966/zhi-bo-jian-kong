#!/usr/bin/env node
/**
 * 攻破 aweme/post bodyLen=0 风控
 * 思路: 访问用户页触发页面自身真实post请求, 捕获完整URL(含msToken),
 *       再在同一页面上下文内fetch该真实URL, 看能否拿到视频列表。
 */
const { chromium } = require('../rsshub/node_modules/patchright');
const CHROME = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const UA = 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/151.0.0.0 Safari/537.36';
const SEC_UID = 'MS4wLjABAAAAfRq5hAWoSxwYGwDEI-F3glR1hf7mdi87_S2y1bhL7OjOUkc_ETYzRq6QwGZ_qf2m';

async function main() {
  const browser = await chromium.launch({ executablePath: CHROME, headless: true, args: ['--no-sandbox','--disable-blink-features=AutomationControlled'] });
  const ctx = await browser.newContext({ userAgent: UA, locale: 'zh-CN' });
  const page = await ctx.newPage();

  await page.goto('https://www.douyin.com/', { waitUntil: 'domcontentloaded', timeout: 40000 }).catch(()=>{});
  await page.waitForTimeout(3500);

  // 捕获真实post请求URL
  let realUrl = '';
  let postResponses = 0;
  page.on('request', req => {
    if (req.url().includes('aweme/post')) {
      realUrl = req.url();
      console.log('[真实post请求捕获]');
    }
  });
  page.on('response', async (resp) => {
    if (resp.url().includes('aweme/post')) {
      postResponses++;
      const t = await resp.text().catch(()=>'');
      console.log('[post响应] status:', resp.status(), 'len:', t.length, 'head:', t.slice(0,80));
      if (postResponses === 1) console.log('[完整post请求URL]\n  ' + realUrl.slice(0,600));
    }
  });

  // 访问用户页触发真实post请求
  console.log('访问用户页触发post请求...');
  await page.goto('https://www.douyin.com/user/' + SEC_UID, { waitUntil: 'domcontentloaded', timeout: 45000 }).catch(()=>{});
  await page.waitForTimeout(5000);
  // 滚动加载更多
  for (let i=0;i<3;i++){ await page.evaluate(()=>window.scrollBy(0,3000)).catch(()=>{}); await page.waitForTimeout(2500); }

  // 用捕获的真实URL在页面内fetch重放
  if (realUrl) {
    console.log('\n=== 用真实URL在页面内fetch重放 ===');
    const res = await page.evaluate(async (url) => {
      const r = await fetch(url, { headers: { 'accept':'application/json', 'referer':'https://www.douyin.com/' }});
      return { status: r.status, len: (await r.text()).length };
    }, realUrl).catch(e=>({error:e.message}));
    console.log('重放结果:', JSON.stringify(res));
  }

  await browser.close();
}
main().catch(e => { console.error('FATAL:', e.message); process.exit(1); });
