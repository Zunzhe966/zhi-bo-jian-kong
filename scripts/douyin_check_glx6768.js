#!/usr/bin/env node
/**
 * 判定抖音号是否真实存在 & 尝试多种输入格式解析 sec_uid
 * 测试: Glx6768 是否是有效抖音号(unique_id)? 纯数字uid? 需要什么格式?
 */
const { chromium } = require('../rsshub/node_modules/patchright');

const CHROME = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const UA = 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/151.0.0.0 Safari/537.36';

async function main() {
  const browser = await chromium.launch({
    executablePath: CHROME, headless: true,
    args: ['--no-sandbox', '--disable-blink-features=AutomationControlled']
  });
  const ctx = await browser.newContext({ userAgent: UA, locale: 'zh-CN' });
  const page = await ctx.newPage();

  await page.goto('https://www.douyin.com/', { waitUntil: 'domcontentloaded', timeout: 40000 }).catch(()=>{});
  await page.waitForTimeout(3000);

  // 测试1: 用 Glx6768 当 user_id 请求 profile, 看返回什么(是否UserId不合法)
  console.log('=== 测试1: Glx6768 当 user_id 请求 profile/other ===');
  const base = 'device_platform=webapp&aid=6383&channel=channel_pc_web&pc_client_type=1&pc_libra_divert=Mac&version_code=170400&version_name=17.4.0&cookie_enabled=true&platform=PC&source=channel_pc_web';
  for (const input of ['Glx6768', 'glx6768', 'GLX6768']) {
    const r = await page.evaluate(async ({input, base}) => {
      const resp = await fetch('https://www.douyin.com/aweme/v1/web/user/profile/other/?sec_user_id='+input+'&'+base, {
        headers: { 'referer': 'https://www.douyin.com/user/'+input, 'accept': 'application/json' }
      });
      return await resp.text();
    }, {input, base});
    try {
      const d = JSON.parse(r);
      console.log(`  ${input}: status=${d.status_code} msg=${d.status_msg||''} user=${d.user?.nickname||'无'}`);
    } catch(e){ console.log(`  ${input}: 响应非JSON`); }
  }

  // 测试2: 用数字格式(可能是纯数字uid)
  console.log('\n=== 测试2: 数字uid格式 ===');
  // Glx6768 去掉字母后是 6768, 但真uid是长数字。测试常见格式
  // 实际测试: 直接访问 user 页看 title

  // 测试3: 检查页面 RENDER_DATA 里 Glx6768 是否被识别
  console.log('\n=== 测试3: 访问用户页检查RENDER_DATA ===');
  await page.goto('https://www.douyin.com/user/Glx6768', { waitUntil: 'domcontentloaded', timeout: 40000 }).catch(()=>{});
  await page.waitForTimeout(6000);
  const renderData = await page.evaluate(() => {
    const s = document.getElementById('RENDER_DATA') || document.querySelector('script#RENDER_DATA');
    return s ? s.textContent.slice(0, 3000) : '无RENDER_DATA';
  });
  console.log('RENDER_DATA片段:', renderData.slice(0, 500));

  await browser.close();
}

main().catch(e => { console.error('FATAL:', e.message); process.exit(1); });
