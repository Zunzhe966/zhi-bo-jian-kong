#!/usr/bin/env node
/**
 * 抖音号(unique_id) → sec_uid 可靠解析工具 v2
 * 修复: 直接访问 /user/{抖音号} 可能随机匹配到不同用户, 需多次采样 + 提取昵称交叉验证。
 *
 * 原理: 用系统Chrome访问 /user/{抖音号}, 拦截profile接口响应体,
 *       提取所有候选的 (sec_uid, nickname, unique_id, signature), 去重后输出。
 *       用户可据此判断哪个才是目标账号。
 *
 * 用法: node douyin_resolve_douyinid.js <抖音号> [采样次数]
 */
const { chromium } = require('../rsshub/node_modules/patchright');
const fs = require('fs');

const CHROME = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const UA = 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/151.0.0.0 Safari/537.36';

async function main() {
  const douyinId = process.argv[2];
  const samples = parseInt(process.argv[3]) || 4;
  if (!douyinId) { console.error('用法: node douyin_resolve_douyinid.js <抖音号> [采样次数]'); process.exit(1); }
  console.log(`目标抖音号: ${douyinId}  (采样${samples}次交叉验证)\n`);

  const browser = await chromium.launch({
    executablePath: CHROME, headless: true,
    args: ['--no-sandbox', '--disable-blink-features=AutomationControlled']
  });

  const candidates = {}; // sec_uid -> {nickname, unique_id, signature, count}

  for (let i = 0; i < samples; i++) {
    const ctx = await browser.newContext({ userAgent: UA, locale: 'zh-CN', viewport: { width: 1280, height: 1600 } });
    const page = await ctx.newPage();

    // 建立会话
    await page.goto('https://www.douyin.com/', { waitUntil: 'domcontentloaded', timeout: 40000 }).catch(()=>{});
    await page.waitForTimeout(2500);

    // 拦截profile接口响应
    page.on('response', async (resp) => {
      const url = resp.url();
      const ct = resp.headers()['content-type'] || '';
      if (url.includes('profile') && ct.includes('json')) {
        const txt = await resp.text().catch(()=>'');
        try {
          const d = JSON.parse(txt);
          if (d.status_code === 0 && d.user && d.user.sec_uid) {
            const u = d.user;
            const key = u.sec_uid;
            if (!candidates[key]) candidates[key] = { nickname: u.nickname, unique_id: u.unique_id, signature: (u.signature||'').slice(0,40), count: 0 };
            candidates[key].count++;
          }
        } catch(e){}
      }
    });

    await page.goto(`https://www.douyin.com/user/${douyinId}`, { waitUntil: 'domcontentloaded', timeout: 40000 }).catch(()=>{});
    await page.waitForTimeout(6000);
    await ctx.close();
  }

  await browser.close();

  console.log('=== 解析到的候选账号(按出现次数排序) ===');
  const sorted = Object.entries(candidates).sort((a,b) => b[1].count - a[1].count);
  if (sorted.length === 0) {
    console.log('未解析到任何候选。');
    return;
  }
  sorted.forEach(([sec, info], idx) => {
    const match = info.unique_id === douyinId ? '  ⬅ 抖音号完全匹配!' : '';
    console.log(`${idx+1}. [${info.count}次] 昵称:${info.nickname} | 抖音号:${info.unique_id}${match}`);
    console.log(`   签名:${info.signature}`);
    console.log(`   sec_uid: ${sec}`);
    console.log('');
  });

  // 输出最可能的一个(优先抖音号完全匹配, 否则出现次数最多)
  let best = sorted[0];
  const exact = sorted.find(([sec,info]) => info.unique_id === douyinId);
  if (exact) best = exact;
  console.log('=== 推荐使用的 sec_uid ===');
  console.log(best[0]);
  console.log(`对应账号: ${best[1].nickname} (抖音号 ${best[1].unique_id})`);
  fs.writeFileSync('/tmp/dy_resolve_best.txt', best[0]);
}

main().catch(e => { console.error('FATAL:', e.message); process.exit(1); });
