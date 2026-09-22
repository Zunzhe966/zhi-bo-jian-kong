#!/usr/bin/env node
/**
 * 抖音号 → sec_uid 解析诊断版 (基于已验证成功的 v1 逻辑)
 * 在同一会话内多次访问 /user/{抖音号}, 打印每次 profile 响应的用户信息, 交叉验证。
 */
const { chromium } = require('../rsshub/node_modules/patchright');

const CHROME = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const UA = 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/151.0.0.0 Safari/537.36';

async function main() {
  const douyinId = process.argv[2];
  const rounds = parseInt(process.argv[3]) || 4;
  if (!douyinId) { console.error('用法: node douyin_resolve_diag.js <抖音号> [轮数]'); process.exit(1); }
  console.log(`目标抖音号: ${douyinId}  共${rounds}轮\n`);

  const browser = await chromium.launch({
    executablePath: CHROME, headless: true,
    args: ['--no-sandbox', '--disable-blink-features=AutomationControlled']
  });
  const ctx = await browser.newContext({ userAgent: UA, locale: 'zh-CN', viewport: { width: 1280, height: 1600 } });
  const page = await ctx.newPage();

  // 建立会话(同一context保留cookie)
  await page.goto('https://www.douyin.com/', { waitUntil: 'domcontentloaded', timeout: 40000 }).catch(()=>{});
  await page.waitForTimeout(3000);

  const seen = {}; // sec_uid -> info
  page.on('response', async (resp) => {
    const url = resp.url();
    const ct = resp.headers()['content-type'] || '';
    if (url.includes('profile/other') && ct.includes('json')) {
      const txt = await resp.text().catch(()=>'');
      try {
        const d = JSON.parse(txt);
        if (d.status_code === 0 && d.user && d.user.sec_uid) {
          const u = d.user;
          const key = u.sec_uid;
          if (!seen[key]) seen[key] = { nickname: u.nickname, unique_id: u.unique_id, sig: (u.signature||'').slice(0,30), rounds: [] };
          seen[key].rounds.push(roundNo);
        }
      } catch(e){}
    }
  });

  let roundNo = 0;
  for (let i = 0; i < rounds; i++) {
    roundNo = i + 1;
    await page.goto(`https://www.douyin.com/user/${douyinId}`, { waitUntil: 'domcontentloaded', timeout: 40000 }).catch(()=>{});
    await page.waitForTimeout(6000);
    console.log(`[第${roundNo}轮] 访问 /user/${douyinId}, 当前URL: ${page.url().slice(0,60)}`);
  }

  await browser.close();

  console.log('\n=== 所有解析到的候选 ===');
  for (const [sec, info] of Object.entries(seen)) {
    const exact = info.unique_id === douyinId ? ' ⬅抖音号完全匹配!' : '';
    console.log(` 昵称:${info.nickname} | 抖音号:${info.unique_id}${exact}`);
    console.log(`   出现轮次: ${info.rounds.join(',')}`);
    console.log(`   sec_uid: ${sec}`);
    console.log('');
  }
  if (Object.keys(seen).length === 0) console.log('(无候选)');
}

main().catch(e => { console.error('FATAL:', e.message); process.exit(1); });
