#!/usr/bin/env node
/**
 * 抖音历史视频批量回捞 (带登录cookie支持)
 *
 * 原理:
 *  1. aweme/post 接口对访客态返回空body, 必须带登录态(sessionid)才能拿视频数据
 *  2. 用无头浏览器建立签名上下文, 页面内fetch带cookie重放, 拿到真实aweme_list
 *  3. 支持 --cookie 从抖音网页复制完整cookie(含sessionid)注入
 *
 * 用法:
 *   node douyin_history.js <sec_uid>                          # 访客态尝试
 *   node douyin_history.js <sec_uid> --cookie "<完整cookie>"   # 带登录cookie
 *   node douyin_history.js <sec_uid> --cookie-file cookies.txt # 从文件读cookie
 *   node douyin_history.js <sec_uid> --max N                  # 最多抓N条(默认全部)
 *
 * cookie获取: 浏览器登录抖音→F12→Network→复制任意douyin请求的Cookie请求头
 */
const { chromium } = require('../rsshub/node_modules/patchright');
const fs = require('fs');
const CHROME = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const UA = 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/151.0.0.0 Safari/537.36';

function parseArgs(argv) {
  const a = { secUid: argv[0], cookie: null, max: Infinity };
  const ci = argv.indexOf('--cookie'); if (ci > 0) a.cookie = argv[ci + 1];
  const cf = argv.indexOf('--cookie-file'); if (cf > 0) a.cookie = fs.readFileSync(argv[cf + 1], 'utf8').trim();
  const mi = argv.indexOf('--max'); if (mi > 0) a.max = parseInt(argv[mi + 1]);
  return a;
}

async function main() {
  const a = parseArgs(process.argv.slice(2));
  if (!a.secUid) { console.error('用法: node douyin_history.js <sec_uid> [--cookie <cookie>] [--cookie-file f] [--max N]'); process.exit(1); }
  if (a.cookie) console.log('已注入登录cookie (长度:' + a.cookie.length + ')');
  else console.log('⚠️ 未提供cookie(访客态), aweme/post可能返回空。建议 --cookie 注入登录态。');

  const browser = await chromium.launch({ executablePath: CHROME, headless: true, args: ['--no-sandbox','--disable-blink-features=AutomationControlled'] });
  const ctx = await browser.newContext({ userAgent: UA, locale: 'zh-CN' });
  if (a.cookie) { try { await ctx.addCookies(a.cookie.split(';').filter(Boolean).map(c => { const i = c.indexOf('='); return { name: c.slice(0,i).trim(), value: c.slice(i+1).trim(), url: 'https://www.douyin.com' }; })); } catch(e) { console.log('cookie注入警告:', e.message); } }
  const page = await ctx.newPage();

  const allVideos = [];
  let cursor = '0', hasMore = true;
  let fetched = 0;

  // 拦截页面真实aweme/post响应
  page.on('response', async (res) => {
    if (!res.url().includes('/aweme/post')) return;
    try {
      const body = await res.text();
      if (body.length < 100) { console.log(`[页面post] body=0 (访客态被拒或需登录)`); return; }
      const j = JSON.parse(body);
      const list = j.aweme_list || [];
      list.forEach(v => {
        if (allVideos.length >= a.max) return;
        allVideos.push({ id: v.aweme_id, desc: v.desc, create_time: v.create_time, play: v.video?.play_addr?.url_list?.[0] || '', cover: v.video?.cover?.url_list?.[0] || '', duration: v.duration });
      });
      if (j.max_cursor) cursor = j.max_cursor;
      hasMore = !!j.has_more;
      console.log(`[页面post] 本批${list.length}条 | 累计${allVideos.length} | has_more=${hasMore}`);
    } catch(e) {}
  });

  console.log(`打开用户主页 ${a.secUid.slice(0,25)}...`);
  await page.goto(`https://www.douyin.com/user/${a.secUid}`, { waitUntil: 'domcontentloaded', timeout: 45000 }).catch(e=>console.log('goto:', e.message));
  await page.waitForTimeout(6000);
  // 滚动触发懒加载
  for (let i = 0; i < 8 && allVideos.length < a.max; i++) {
    await page.evaluate(() => window.scrollBy(0, 2500)).catch(()=>{});
    await page.waitForTimeout(2500);
  }
  await page.waitForTimeout(3000);

  // 若页面只拿到1-2批且has_more, 用页面内fetch翻页(带签名上下文+cookie)
  while (hasMore && allVideos.length < a.max && allVideos.length > 0) {
    console.log(`翻页 fetch cursor=${cursor} ...`);
    const res = await page.evaluate(async ({ sec, cur }) => {
      const url = `https://www.douyin.com/aweme/v1/web/aweme/post/?device_platform=webapp&aid=6383&channel=channel_pc_web&sec_user_id=${sec}&max_cursor=${cur}&locate_query=false&show_live_replay_strategy=1&need_time_list=1&count=18&publish_video_strategy_type=2&from_user_page=1&update_version_code=170400&pc_client_type=1&pc_libra_divert=Mac&support_h265=1&support_dash=0&version_code=170400&version_name=17.4.0&cookie_enabled=true&platform=PC&downlink=10&effective_type=4g&round_trip_time=300`;
      const r = await fetch(url, { headers: { 'referer': `https://www.douyin.com/user/${sec}`, 'accept': 'application/json' } });
      const t = await r.text();
      return { status: r.status, len: t.length, text: t };
    }, { sec: a.secUid, cur: cursor });
    console.log(`  翻页 status=${res.status} len=${res.len}`);
    if (res.len < 100) { console.log('  翻页被拒(需登录), 停止'); break; }
    try {
      const j = JSON.parse(res.text);
      const list = j.aweme_list || [];
      list.forEach(v => { if (allVideos.length < a.max) allVideos.push({ id: v.aweme_id, desc: v.desc, create_time: v.create_time, play: v.video?.play_addr?.url_list?.[0] || '', cover: v.video?.cover?.url_list?.[0] || '', duration: v.duration }); });
      cursor = j.max_cursor || cursor;
      hasMore = !!j.has_more;
      console.log(`  本批${list.length}条 | 累计${allVideos.length} | has_more=${hasMore}`);
    } catch(e) { console.log('  翻页解析失败'); break; }
  }

  console.log(`\n=== 抓取到视频总数: ${allVideos.length} ===`);
  allVideos.slice(0, 20).forEach(v => {
    console.log(`ID:${v.id} | ${new Date(v.create_time*1000).toISOString().slice(0,10)} | ${(v.desc||'').slice(0,28)} | ${v.duration ? Math.round(v.duration/1000)+'s' : ''}`);
  });
  const out = `/tmp/dy_history_${Date.now()}.json`;
  fs.writeFileSync(out, JSON.stringify(allVideos, null, 2));
  console.log(`\n已保存 ${out}`);
  await browser.close();
}
main().catch(e => { console.error('FATAL:', e.message); process.exit(1); });
