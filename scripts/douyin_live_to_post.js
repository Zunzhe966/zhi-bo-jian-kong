#!/usr/bin/env node
/**
 * 完整链路: 抖音号 → live页提取sec_uid → 浏览器fetch post接口拿历史视频
 * 还原备份软件"只靠主播ID+平台"的原理
 *
 * 用法: node douyin_live_to_post.js <抖音号>
 */
const { chromium } = require('../rsshub/node_modules/patchright');
const https = require('https');
const CHROME = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const UA = 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/151.0.0.0 Safari/537.36';

function fetchHtml(url) {
  return new Promise((resolve, reject) => {
    const req = https.get(url, { headers: { 'User-Agent': UA, 'Accept-Language': 'zh-CN,zh;q=0.9' }, timeout: 20000 }, res => {
      if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) return fetchHtml(res.headers.location).then(resolve, reject);
      let d = ''; res.on('data', c => d += c); res.on('end', () => resolve(d));
    });
    req.on('error', reject); req.on('timeout', () => { req.destroy(); reject(new Error('timeout')); });
  });
}

async function main() {
  const douyinId = process.argv[2] || 'Glx6768';
  console.log('目标抖音号:', douyinId);

  // 1. 从 live 页提取 sec_uid
  console.log('\n[1/3] 访问 live.douyin.com/' + douyinId + ' 提取 sec_uid ...');
  const html = await fetchHtml('https://live.douyin.com/' + douyinId);
  const secs = [...new Set(html.match(/MS4wLjAB[A-Za-z0-9_-]+/g) || [])];
  if (secs.length === 0) { console.log('❌ live页未提取到sec_uid'); return; }
  const secUid = secs[0];
  console.log('✅ 提取到 sec_uid:', secUid);

  // 2. 用浏览器fetch post接口(带签名)
  console.log('\n[2/3] 启动浏览器, 用 sec_uid 查历史视频 ...');
  const browser = await chromium.launch({ executablePath: CHROME, headless: true, args: ['--no-sandbox','--disable-blink-features=AutomationControlled'] });
  const ctx = await browser.newContext({ userAgent: UA, locale: 'zh-CN' });
  const page = await ctx.newPage();
  await page.goto('https://www.douyin.com/', { waitUntil: 'domcontentloaded', timeout: 40000 }).catch(()=>{});
  await page.waitForTimeout(3000);

  const baseParams = { device_platform:'webapp', aid:'6383', channel:'channel_pc_web', update_version_code:'170400', pc_client_type:'1', pc_libra_divert:'Mac', support_h265:'1', support_dash:'0', version_code:'170400', version_name:'17.4.0', cookie_enabled:'true', platform:'PC', downlink:'10', effective_type:'4g', round_trip_time:'300' };

  // 3. 用户资料 + post
  console.log('\n[3/3] 拉取用户资料 + 历史视频列表 ...');
  try {
    const prof = await page.evaluate(async ({p, sec}) => {
      const qs = new URLSearchParams({...p, sec_user_id: sec, publish_video_strategy_type:'2', source:'channel_pc_web'}).toString();
      const r = await fetch('https://www.douyin.com/aweme/v1/web/user/profile/other/?'+qs, { headers:{ 'referer':'https://www.douyin.com/user/'+sec, 'accept':'application/json' }});
      return await r.json();
    }, { p: baseParams, sec: secUid });
    const u = prof.user || {};
    console.log('资料 status:', prof.status_code, prof.status_msg||'');
    console.log('昵称:', u.nickname, '| 抖音号:', u.unique_id);
    console.log('作品数:', u.aweme_count, '| 粉丝:', u.follower_count);
  } catch(e) { console.log('资料接口失败:', e.message.slice(0,60)); }

  try {
    const post = await page.evaluate(async ({p, sec}) => {
      const qs = new URLSearchParams({...p, sec_user_id: sec, max_cursor:'0', count:'20', publish_video_strategy_type:'2'}).toString();
      const r = await fetch('https://www.douyin.com/aweme/v1/web/aweme/post/?'+qs, { headers:{ 'referer':'https://www.douyin.com/user/'+sec, 'accept':'application/json' }});
      return { status: r.status, body: await r.text() };
    }, { p: baseParams, sec: secUid });
    console.log('post接口 HTTP:', post.status, '| bodyLen:', post.body.length);
    if (post.body.length > 10) {
      try {
        const d = JSON.parse(post.body);
        console.log('status_code:', d.status_code, d.status_msg||'');
        const list = d.aweme_list || [];
        console.log('本页视频数:', list.length, '| has_more:', d.has_more);
        list.slice(0,8).forEach(a => {
          console.log('  VIDEO ' + a.aweme_id + ' | ' + (a.desc||'').slice(0,28) + ' | ' + new Date(a.create_time*1000).toISOString().slice(0,10));
        });
      } catch(e) { console.log('post响应非JSON, head:', post.body.slice(0,150)); }
    }
  } catch(e) { console.log('post接口异常:', e.message.slice(0,60)); }

  await browser.close();
}
main().catch(e => { console.error('FATAL:', e.message); process.exit(1); });
