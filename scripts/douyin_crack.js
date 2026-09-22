#!/usr/bin/env node
/**
 * 抖音信息破解工具
 * 原理：用系统 Chrome 驱动（patchright），真实浏览器自动生成签名+设备指纹，
 *       绕过抖音 web 接口的 a_bogus/X-Bogus 签名校验，抓取用户资料和视频列表。
 *
 * 用法：
 *   node douyin_crack.js <sec_uid>                # 抓取指定 sec_uid 的用户资料+视频列表
 *   node douyin_crack.js --url <抖音URL>          # 从分享链接/主页链接自动解析 sec_uid
 *
 * 例：
 *   node douyin_crack.js MS4wLjABAAAAhNKwKfRQ5VW9W77-So7v9NPgridm61yK2wlSMrnaAFGto9nWCaGNpkoNqvFlACFN
 */
const { chromium } = require('../rsshub/node_modules/patchright');
const fs = require('fs');

const CHROME = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const UA = 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/151.0.0.0 Safari/537.36';

// 从URL提取sec_uid（支持 www.douyin.com/user/{sec_uid}、v.douyin.com短链等）
function extractSecUidFromUrl(url) {
  // 主页链接 /user/{sec_uid}
  const m = url.match(/douyin\.com\/user\/([A-Za-z0-9_-]+)/);
  if (m && m[1].startsWith('MS4wLjAB')) return m[1];
  return null;
}

async function fetchInPage(page, path, params, referer) {
  const qs = new URLSearchParams(params).toString();
  const resp = await page.evaluate(async ({ path, qs, referer }) => {
    const r = await fetch(path + '?' + qs, {
      headers: { 'referer': referer, 'accept': 'application/json' }
    });
    return await r.text();
  }, { path, qs, referer });
  return JSON.parse(resp);
}

async function main() {
  const args = process.argv.slice(2);
  let secUid = null;
  if (args[0] === '--url') {
    const url = args[1];
    secUid = extractSecUidFromUrl(url);
    if (!secUid) {
      console.error('无法从URL提取sec_uid:', url);
      console.error('请使用 www.douyin.com/user/{sec_uid} 格式的链接');
      process.exit(1);
    }
    console.log('从URL提取到sec_uid:', secUid);
  } else {
    secUid = args[0];
    if (!secUid) { console.error('用法: node douyin_crack.js <sec_uid>'); process.exit(1); }
  }

  const browser = await chromium.launch({
    executablePath: CHROME, headless: true,
    args: ['--no-sandbox', '--disable-blink-features=AutomationControlled']
  });
  const ctx = await browser.newContext({ userAgent: UA, locale: 'zh-CN' });
  const page = await ctx.newPage();

  // 先访问抖音首页建立会话(cookie/ttwid)，让后续fetch带签名上下文
  console.log('建立会话...');
  await page.goto('https://www.douyin.com/', { waitUntil: 'domcontentloaded', timeout: 40000 }).catch(()=>{});
  await page.waitForTimeout(3000);

  // 1. 抓用户资料
  console.log('\n=== 用户资料 ===');
  const baseParams = {
    device_platform: 'webapp', aid: '6383', channel: 'channel_pc_web',
    update_version_code: '170400', pc_client_type: '1', pc_libra_divert: 'Mac',
    support_h265: '1', support_dash: '0', version_code: '170400', version_name: '17.4.0',
    cookie_enabled: 'true', platform: 'PC', downlink: '10', effective_type: '4g', round_trip_time: '300'
  };
  const profile = await fetchInPage(page,
    'https://www.douyin.com/aweme/v1/web/user/profile/other/',
    { ...baseParams, sec_user_id: secUid, publish_video_strategy_type: '2', source: 'channel_pc_web', personal_center_strategy: '1' },
    `https://www.douyin.com/user/${secUid}`
  );
  const u = profile.user || {};
  console.log('状态:', profile.status_code, profile.status_msg || '');
  console.log('昵称:', u.nickname);
  console.log('抖音号(unique_id):', u.unique_id);
  console.log('sec_uid:', u.sec_uid);
  console.log('签名:', (u.signature || '').slice(0, 60));
  console.log('作品数:', u.aweme_count, '| 粉丝数:', u.follower_count, '| 关注数:', u.following_count);

  // 2. 抓视频列表（可能需额外参数，尽力尝试）
  console.log('\n=== 视频列表(尝试) ===');
  try {
    const post = await fetchInPage(page,
      'https://www.douyin.com/aweme/v1/web/aweme/post/',
      { ...baseParams, sec_user_id: secUid, max_cursor: '0', count: '18', publish_video_strategy_type: '2' },
      `https://www.douyin.com/user/${secUid}`
    );
    const list = post.aweme_list || [];
    console.log('本页视频数:', list.length, '| has_more:', post.has_more);
    list.slice(0, 5).forEach(a => {
      console.log(`  ID:${a.aweme_id} | 标题:${(a.desc || '').slice(0, 25)} | 时间:${new Date(a.create_time*1000).toISOString().slice(0,10)}`);
    });
    fs.writeFileSync('/tmp/dy_crack_post.json', JSON.stringify(post, null, 2));
  } catch (e) {
    console.log('视频列表接口受限:', e.message);
    console.log('提示: 视频列表需额外 msToken 参数，建议改用"访问用户页拦截页面加载"方式');
  }

  await browser.close();
  console.log('\n完成。');
}

main().catch(e => { console.error('FATAL:', e.message); process.exit(1); });
