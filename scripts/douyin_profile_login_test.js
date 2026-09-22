// 测试: 用系统Chrome真实登录态profile访问抖音, 看能否拿到登录态并解析抖音号
const { chromium } = require('../rsshub/node_modules/patchright');
const CHROME = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const USER_DATA = '/Users/zhangxuetao/Library/Application Support/Google/Chrome';

async function main() {
  const douyinId = process.argv[2] || 'Glx6768';
  console.log('目标抖音号:', douyinId);

  // 用持久化上下文加载真实Chrome profile(带登录态)
  const ctx = await chromium.launchPersistentContext(USER_DATA, {
    executablePath: CHROME,
    headless: false,   // 持久化profile在headless下可能受限, 用有头
    channel: 'chrome',
    args: ['--no-first-run', '--no-default-browser-check']
  }).catch(async e => {
    console.log('持久化启动失败(可能Chrome占用):', e.message.slice(0,80));
    process.exit(1);
  });

  // 取已打开的页面或新建
  let page = ctx.pages()[0] || await ctx.newPage();

  // 访问抖音首页检查登录态
  console.log('\n检查抖音登录态...');
  await page.goto('https://www.douyin.com/', { waitUntil: 'domcontentloaded', timeout: 45000 }).catch(()=>{});
  await page.waitForTimeout(4000);
  const cookies = await ctx.cookies('https://www.douyin.com');
  console.log('抖音cookies:', cookies.map(c=>c.name).join(', ') || '(无)');
  const hasSession = cookies.some(c => ['sessionid','sessionid_ss','sid_guard','passport_csrf_token'].includes(c.name));
  console.log('登录态(sessionid):', hasSession ? '✅ 已登录' : '❌ 未登录/访客');

  // 尝试访问 /user/{抖音号}
  console.log('\n访问用户页 /user/' + douyinId + ' ...');
  let profileUrl = '';
  page.on('response', async (resp) => {
    const url = resp.url();
    if (url.includes('profile') && !profileUrl) {
      profileUrl = url;
      try {
        const txt = await resp.text();
        if (txt.includes('MS4wLjAB')) {
          const secs = txt.match(/MS4wLjAB[A-Za-z0-9_-]+/g) || [];
          const d = JSON.parse(txt);
          console.log('\n[profile接口] status_code:', d.status_code, d.status_msg||'');
          console.log('  sec_uid:', secs[0] || '无');
          console.log('  昵称:', d.user?.nickname || '无');
          console.log('  unique_id:', d.user?.unique_id || '无');
          console.log('  ===> 命中sec_uid候选:', secs);
        } else {
          console.log('[profile接口] 返回无sec_uid, head:', txt.slice(0,120));
        }
      } catch(e){ console.log('[profile解析失败]', e.message.slice(0,50)); }
    }
  });

  await page.goto('https://www.douyin.com/user/' + douyinId, { waitUntil: 'domcontentloaded', timeout: 45000 }).catch(()=>{});
  await page.waitForTimeout(8000);
  console.log('\n当前URL:', page.url().slice(0,100));

  await ctx.close();
}

main().catch(e => { console.error('FATAL:', e.message); process.exit(1); });
