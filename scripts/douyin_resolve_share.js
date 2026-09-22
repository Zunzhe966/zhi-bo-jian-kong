#!/usr/bin/env node
/**
 * 抖音分享短链 → sec_uid 解析工具
 * 原理: 用系统Chrome打开 v.douyin.com 短链, 短链302跳转到 www.douyin.com/user/{sec_uid},
 *       捕获最终URL/重定向/页面数据, 提取 sec_uid 和用户信息。
 *
 * 用法: node douyin_resolve_share.js <v.douyin.com短链>
 */
const { chromium } = require('../rsshub/node_modules/patchright');
const fs = require('fs');

const CHROME = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const UA = 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/151.0.0.0 Safari/537.36';

async function main() {
  const shareUrl = process.argv[2];
  if (!shareUrl) { console.error('用法: node douyin_resolve_share.js <v.douyin.com短链>'); process.exit(1); }
  console.log('目标分享链接:', shareUrl);

  const browser = await chromium.launch({
    executablePath: CHROME, headless: true,
    args: ['--no-sandbox', '--disable-blink-features=AutomationControlled']
  });
  const ctx = await browser.newContext({ userAgent: UA, locale: 'zh-CN' });
  const page = await ctx.newPage();

  // 记录所有重定向
  const redirects = [];
  page.on('request', req => {
    if (req.url().includes('douyin.com') && req.url().includes('/user/')) {
      const m = req.url().match(/\/user\/([A-Za-z0-9_-]+)/);
      if (m) redirects.push(m[1]);
    }
  });
  page.on('response', resp => {
    if (resp.url().includes('douyin.com')) {
      console.log('  [resp]', resp.status(), resp.url().slice(0, 90));
    }
  });

  // 打开分享短链(跟随重定向)
  console.log('\n打开短链, 跟随重定向...');
  let finalUrl = '';
  try {
    const resp = await page.goto(shareUrl, { waitUntil: 'domcontentloaded', timeout: 45000, referer: 'https://www.douyin.com/' });
    finalUrl = page.url();
  } catch(e) {
    finalUrl = page.url();
    console.log('  加载中异常(可能已跳转):', e.message.slice(0,60));
  }
  await page.waitForTimeout(8000);

  console.log('\n最终URL:', finalUrl.slice(0, 120));

  // 从URL提取sec_uid
  const urlMatch = finalUrl.match(/\/user\/([A-Za-z0-9_-]+)/);
  let secUid = null;
  if (urlMatch && urlMatch[1].startsWith('MS4wLjAB')) secUid = urlMatch[1];
  if (!secUid) console.log('URL里没有直接sec_uid, 尝试从重定向/页面数据提取');

  // 从重定向提取
  if (!secUid) {
    for (const r of redirects) if (r.startsWith('MS4wLjAB')) { secUid = r; break; }
    if (secUid) console.log('[重定向] sec_uid:', secUid);
  }

  // 从页面RENDER_DATA提取用户信息
  const renderData = await page.evaluate(() => {
    const s = document.getElementById('RENDER_DATA');
    return s ? s.textContent : '';
  }).catch(()=>'');

  // 从响应体提取 sec_uid + 昵称
  let foundSec = secUid;
  let foundNick = '';
  const bodyCapture = [];
  page.on('response', async (resp) => {
    const url = resp.url();
    const ct = resp.headers()['content-type'] || '';
    if ((url.includes('profile') || url.includes('user')) && ct.includes('json')) {
      const txt = await resp.text().catch(()=>'');
      if (txt.includes('MS4wLjAB')) {
        const secs = txt.match(/MS4wLjAB[A-Za-z0-9_-]+/g) || [];
        if (!foundSec && secs.length) foundSec = secs[0];
        try {
          const d = JSON.parse(txt);
          if (d.user && d.user.nickname) foundNick = d.user.nickname;
        } catch(e){}
      }
    }
  });

  // 若短链跳转到了 /video/{id} 或需再等, 补一轮访问用户页
  if (!foundSec) {
    console.log('短链未直接给出sec_uid, 尝试再访问主页...');
    const m = finalUrl.match(/\/user\/([A-Za-z0-9_-]+)/);
    if (m) {
      await page.goto(finalUrl, { waitUntil: 'domcontentloaded', timeout: 40000 }).catch(()=>{});
      await page.waitForTimeout(6000);
    }
  }

  // 解析RENDER_DATA(URL解码)
  let decoded = '';
  try { decoded = decodeURIComponent(renderData); } catch(e){ decoded = renderData; }
  const rdSecs = decoded.match(/MS4wLjAB[A-Za-z0-9_-]+/g) || [];
  if (!foundSec && rdSecs.length) foundSec = rdSecs[0];
  // RENDER_DATA里找昵称
  if (!foundNick) {
    const nm = decoded.match(/"nickname":"([^"]+)"/);
    if (nm) foundNick = nm[1];
  }

  await browser.close();

  console.log('\n=== 解析结果 ===');
  console.log('sec_uid:', foundSec || '(未找到)');
  console.log('昵称:', foundNick || '(未找到)');
  if (foundSec) {
    fs.writeFileSync('/tmp/dy_share_secuid.txt', foundSec);
    fs.writeFileSync('/tmp/dy_share_user.txt', `${foundNick}\n${foundSec}`);
    console.log('已保存sec_uid到 /tmp/dy_share_secuid.txt');
  }
}

main().catch(e => { console.error('FATAL:', e.message); process.exit(1); });
