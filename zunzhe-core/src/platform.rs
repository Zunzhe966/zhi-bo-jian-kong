use anyhow::{Context, Result, anyhow};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tracing::warn;

use crate::bilibili_auth::{
    browser_cookie_path, cookie_header_from_netscape_public, ensure_anon_cookie,
    ensure_user_cookie_file, read_browser_cookie_file,
};
use crate::model::VideoMeta;
use crate::process::ManagedChild;

/// B 站 WBI 固定置换表（公开算法，非密钥）。
const MIXIN_KEY_TAB: [usize; 64] = [
    46, 47, 18, 2, 53, 8, 23, 32, 15, 50, 10, 31, 58, 3, 45, 35, 27, 43, 5, 49, 33, 9, 42, 19, 29,
    28, 14, 39, 12, 38, 41, 13, 37, 48, 7, 16, 24, 55, 40, 61, 26, 17, 0, 1, 60, 51, 30, 4, 22, 25,
    54, 21, 56, 59, 6, 63, 57, 62, 11, 36, 20, 34, 44, 52,
];

fn mixin_key(img_key: &str, sub_key: &str) -> String {
    let raw = format!("{img_key}{sub_key}");
    let chars: Vec<char> = raw.chars().collect();
    MIXIN_KEY_TAB
        .iter()
        .map(|&i| chars.get(i).copied().unwrap_or('a'))
        .collect()
}

fn md5_hex(text: &str) -> String {
    use md5::{Digest, Md5};
    let mut hasher = Md5::new();
    hasher.update(text.as_bytes());
    let bytes = hasher.finalize();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// 汇总三个兜底源的空结果错误，生成给用户的顶层错误说明。
///
/// 24×30 审计发现：B站源网络/DNS 故障时，旧实现把底层原因吞成“空列表”，
/// 顶层永远归因为“412 风控，请配置 cookie”，误导用户去配 Cookie 而不是
/// 检查网络。这里把每级原因带上来，风控与网络故障分别给出可操作提示。
fn upload_list_failure_message(official: &str, rsshub: &str, ytdlp: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if !official.is_empty() {
        parts.push(official);
    }
    if !rsshub.is_empty() {
        parts.push(rsshub);
    }
    if !ytdlp.is_empty() {
        parts.push(ytdlp);
    }
    let detail = if parts.is_empty() {
        "三个兜底均未返回数据".to_string()
    } else {
        parts.join("；")
    };
    // 判定顺序很有讲究：
    // 1) 网络故障优先——即使同时带“请求失败”和“风控”，根因也是网络；
    // 2) “-403 访问权限不足/需要登录态”单独成一档——此时风控已过，只差登录，
    //    提示用户去配置 B站 cookie 才是唯一正解，不能笼统说“412 风控”；
    // 3) 其余风控类（-352/-799/412/签名）再归为风控档。
    let hint = if detail.contains("解析失败")
        || detail.contains("请求失败")
        || detail.contains("DNS")
        || detail.contains("timed out")
        || detail.contains("Timeout")
        || detail.contains("超时")
        || detail.contains("不可达")
    {
        "若为网络/DNS 故障请检查网络后重试".to_string()
    } else if detail.contains("访问权限不足") || detail.contains("-403") {
        "B站该接口需要登录态，请在桌面端「B站授权」中勾选登录账号后再试".to_string()
    } else if detail.contains("-352")
        || detail.contains("-799")
        || detail.contains("412")
        || detail.contains("风控")
        || detail.contains("403")
        || detail.contains("wbi")
        || detail.contains("签名")
    {
        "B站按 412 风控拦下了这次请求：请在设置页点「B站授权」用浏览器登录一次再重试——带 Cookie 请求就不容易被拦；不想登录就隔几分钟、换个网络再试"
            .to_string()
    } else {
        "可稍后重试；仍失败就在设置页点「B站授权」登录一次".to_string()
    };
    format!("B站投稿列表拉取失败: {detail}（{hint}）")
}

/// 构造 RSSHub 路由 URL 候选列表：配置的自建实例优先，否则依次尝试
/// 公共实例（rsshub.app 官方 + RSS.SB 社区镜像），一个挂了自动换下一个，
/// 避免「RSS 首选」因单点故障直接失效。
fn rsshub_candidates(configured: &str, mid: &str) -> Vec<String> {
    let trimmed = configured.trim().trim_end_matches('/');
    let bases: Vec<String> = if trimmed.is_empty() {
        vec![
            "https://rsshub.app".to_string(),
            "https://rsshub.rss.sb".to_string(),
        ]
    } else {
        vec![trimmed.to_string()]
    };
    bases
        .into_iter()
        .map(|b| format!("{b}/bilibili/user/video/{mid}"))
        .collect()
}

/// JS `encodeURIComponent` 的 Rust 等价实现。
///
/// B站的 `w_rid` 是对**已 URL 编码**的查询串求 MD5，而且真正发出去的请求必须
/// 是同一份编码后的串。`dm_img_*` 这类风控参数的值是 JSON，含 `{` `"` `[` `]`
/// 等字符，不编码服务端会直接判成非法请求。
fn encode_uri_component(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        let b = *byte;
        let unreserved = b.is_ascii_alphanumeric()
            || matches!(b, b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')');
        if unreserved {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// 网页端 `x/space/wbi/arc/search` 一起发出的本地化头（参与 w_rid 签名）。
const X_BILI_LOCALE_JSON: &str =
    "{\"c_locale\":{\"language\":\"zh\",\"script\":\"Hans\"},\"always_translate\":false}";
/// 网页端 `x/space/wbi/arc/search` 一起发出的设备头（参与 w_rid 签名）。
const X_BILI_DEVICE_REQ_JSON: &str =
    "{\"platform\":\"web\",\"device\":\"pc\",\"spmid\":\"333.1387\",\"mobi_app\":\"web_cn\"}";
/// 风险控制参数：鼠标轨迹。网页端由 JS 采集，这里给通用占位值——
/// 服务端主要校验「有没有参与签名」，不校验轨迹内容。
const DM_IMG_LIST: &str = "[{\"x\":0,\"y\":0,\"z\":0,\"timestamp\":1,\"k\":1,\"type\":1}]";
/// 风险控制参数：WebGL 渲染器信息（base64）。
const DM_IMG_STR: &str = "V2ViR0wgMS4wIChPcGVuR0wgRVMgMi4wIENocm9taXVtKQ";
/// 风险控制参数：显卡/渲染器描述（base64）。
const DM_COVER_IMG_STR: &str = "QU5HTEUgKEFwcGxlLCBBTkdMRSBNZXRhbCBSZW5kZXJlcjogQXBwbGUgTTEsIFVuc3BlY2lmaWVkIFZlcnNpb24pR29vZ2xlIEluYy4gKEFwcGxl";
/// 风险控制参数：页面交互采样。
const DM_IMG_INTER: &str = "{\"ds\":[],\"wh\":[0,0,0],\"of\":[0,0,0]}";

/// 用 B 站 WBI 公钥和投稿列表参数生成签名查询串。
///
/// 纯函数便于单测；排序、URL 编码、拼参、`w_rid` 均按官方 WBI 规则执行。
///
/// 参数表必须与网页端 `x/space/wbi/arc/search` 保持一致：B站把整串参与
/// `w_rid` 签名，少一个都会被判成 `code=-352`「风控校验失败」。尤其是
/// `dm_img_list` / `dm_img_str` / `dm_cover_img_str` / `dm_img_inter` 这四个
/// 由网页端 JS 采集的风险控制字段——早期实现漏了它们，官方投稿接口永远
/// 打不通，「列出主播历史」只能退回 RSSHub/yt-dlp 的半截列表，于是官方
/// 46 条只列出 45 条，用户怎么点都补不上那一条。
fn build_wbi_query(
    img_key: &str,
    sub_key: &str,
    mid: &str,
    count: u32,
    page: u32,
    wts: u64,
) -> String {
    let mut params = vec![
        ("mid", mid.to_string()),
        ("ps", count.to_string()),
        ("pn", page.to_string()),
        ("tid", "0".to_string()),
        ("special_type", String::new()),
        ("order", "pubdate".to_string()),
        ("index", "0".to_string()),
        ("keyword", String::new()),
        ("order_avoided", "true".to_string()),
        ("platform", "web".to_string()),
        ("web_location", "333.1387".to_string()),
        ("x-bili-locale-json", X_BILI_LOCALE_JSON.to_string()),
        ("x-bili-device-req-json", X_BILI_DEVICE_REQ_JSON.to_string()),
        ("dm_img_list", DM_IMG_LIST.to_string()),
        ("dm_img_str", DM_IMG_STR.to_string()),
        ("dm_cover_img_str", DM_COVER_IMG_STR.to_string()),
        ("dm_img_inter", DM_IMG_INTER.to_string()),
        ("wts", wts.to_string()),
    ];
    params.sort_by(|a, b| a.0.cmp(b.0));
    let query = params
        .iter()
        .map(|(k, v)| format!("{}={}", encode_uri_component(k), encode_uri_component(v)))
        .collect::<Vec<_>>()
        .join("&");
    let mixin = mixin_key(img_key, sub_key);
    let digest = md5_hex(&format!("{query}{mixin}"));
    format!("{query}&w_rid={digest}")
}

/// 合并两组 Cookie 头：以 `base` 为主，仅追加 `extra` 中 base 没有的键。
///
/// 用于「登录态 Cookie + 匿名设备指纹」叠加：登录态字段（如 SESSDATA）
/// 绝不能被匿名指纹覆盖，缺失的设备字段（buvid3/buvid4/b_lsid）才补进来。
fn merge_cookie_headers(base: &str, extra: &str) -> String {
    let mut seen: Vec<String> = Vec::new();
    let mut pairs: Vec<(String, String)> = Vec::new();
    // 先 base 后 extra：先到先得，同名键只保留第一次出现的值，
    // 因此登录态字段（base）不会被匿名指纹（extra）覆盖。
    for source in [base, extra] {
        for pair in source.split(';') {
            let pair = pair.trim();
            if pair.is_empty() {
                continue;
            }
            let Some((key, value)) = pair.split_once('=') else {
                continue;
            };
            let key = key.trim();
            let value = value.trim();
            if key.is_empty() || value.is_empty() {
                continue;
            }
            let lower = key.to_ascii_lowercase();
            if !seen.contains(&lower) {
                seen.push(lower);
                pairs.push((key.to_string(), value.to_string()));
            }
        }
    }
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("; ")
}

/// Cookie 头里是否含非空的登录态字段（`SESSDATA` / `DedeUserID`）。
///
/// 匿名设备指纹（buvid3/buvid4/b_lsid）不算登录态，不能作为「已授权」的依据。
fn cookie_header_has_login(header: &str) -> bool {
    header.split(';').any(|pair| {
        let Some((key, value)) = pair.split_once('=') else {
            return false;
        };
        !value.trim().is_empty()
            && (key.trim().eq_ignore_ascii_case("SESSDATA")
                || key.trim().eq_ignore_ascii_case("DedeUserID"))
    })
}

/// 判断 B站投稿列表是否应继续翻页。
///
/// `count == 0` 表示“全部历史视频”：只要总投稿数已知且还没取满就继续；
/// 否则以 count 为上限。总投稿数未知时，按“当前页是否已取满”保守继续，
/// 但调用方仍必须设置最大页数护栏，防止接口异常时无限翻页。
fn should_fetch_more(count: u32, fetched: usize, total: Option<u32>, page: u32) -> bool {
    if page >= 50 {
        return false;
    }
    if count > 0 && fetched as u32 >= count {
        return false;
    }
    match total {
        Some(t) => (fetched as u32) < t,
        None => page < 10,
    }
}

/// 解析 yt-dlp `--print "%(id)s\t%(playlist_count)s"` 的 stdout。
///
/// 返回 `(BV 号列表, 播放列表总数)`：
/// - 每行形如 `<bv号>\t<总数>`，`playlist_count` 不可用时 yt-dlp 会打印 `NA`；
/// - 总数取所有行里的最大值（每行都重复同一个 `playlist_count`，取最大对
///   单条异常值也更稳）；全部缺失/不可解析时为 `None`，绝不能瞎猜成 0；
/// - `count > 0`（只取最近 N 条）时按 `count` 截断，与官方路径语义一致。
///
/// 抽成纯函数是为了可测：这个总数直接决定界面会不会弹「列表可能不完整」，
/// 解析错了就是又一次假警报或又一次静默漏条。
fn parse_ytdlp_flat_playlist(text: &str, count: u32) -> (Vec<String>, Option<u32>) {
    let mut total: Option<u32> = None;
    let mut ids: Vec<String> = Vec::new();
    for line in text.lines() {
        let mut parts = line.trim().split('\t');
        let id = parts.next().unwrap_or("").trim();
        if let Some(n) = parts.next().and_then(|s| s.trim().parse::<u32>().ok()) {
            total = Some(total.map_or(n, |cur: u32| cur.max(n)));
        }
        if is_plausible_bvid(id) {
            ids.push(id.to_string());
        }
    }
    if count > 0 {
        ids.truncate(count as usize);
    }
    (ids, total)
}

pub trait Platform: Send + Sync {
    fn name(&self) -> &'static str;

    /// 从博主主页/订阅地址列出已发布视频。
    fn list_videos<'a>(
        &'a self,
        url: &'a str,
        count: u32,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Vec<VideoMeta>>> + Send + 'a>>;

    /// 上一次 `list_videos` 期间平台自己报告的总条数；拿不到就返回 `None`。
    ///
    /// 上层用它把「平台说共 N 条」与「实际列出 M 条」对照：M < N 时如实告诉
    /// 用户差了几条，而不是让列表悄悄少一条。默认实现返回 `None`（RSSHub /
    /// yt-dlp 这类兜底源本身不提供总数）。
    fn expected_total(&self) -> Option<u32> {
        None
    }

    /// 上一次 `list_videos` 期间「权威来源」（平台自己的投稿接口）为什么没给出
    /// 完整列表；没有失败时返回 `None`。
    ///
    /// 兜底源（RSSHub / yt-dlp）经常只能给半截列表，而半截列表和完整列表在界面上
    /// 长得一模一样。把失败原因翻译成用户能照着做的一句话交给上层，才不会出现
    /// 「官方 46 条、界面 45 条，还查不出为什么」。
    fn authoritative_hint(&self) -> Option<String> {
        None
    }
}

/// B站适配器：先解析 space.bilibili.com/<mid>，用 view 接口补全
/// 标题/封面/日期/时长；投稿 ID 走官方 WBI 接口，RSSHub/yt-dlp 兜底。
#[derive(Debug)]
pub struct BilibiliAdapter {
    client: reqwest::Client,
    cookie: String,
    project_dir: PathBuf,
    /// 官方接口报告的投稿总数（`data.page.count`），`list_videos` 期间写入。
    ///
    /// 存在的唯一目的：把「平台说一共有多少条」和「实际列出多少条」摆到一起。
    /// 这两者不等时（例如官方报 46、实取 45），少掉的那条不是被程序吞了，就是
    /// 被 B站自己过滤了（下架 / 仅自己可见 / 审核中 / 联合投稿不在该接口）。
    /// 以前这个差值被静默丢弃，用户只看到「少了 1 条」却永远查不出为什么。
    /// 0 表示未知（RSSHub / yt-dlp 兜底路径拿不到总数）。
    expected_total: std::sync::atomic::AtomicU32,

    /// 官方接口最近一次失败的业务码，0 表示没失败（成功或没调用）。
    /// 负数直接是 B站业务码；`-1000` = HTTP 层失败（412 等），`-1001` = 网络/解析失败。
    /// 上层用它把「官方接口为什么没给出完整列表」翻译成用户能照着做的提示。
    last_official_code: std::sync::atomic::AtomicI64,

    /// 最近一次请求官方接口时，请求里是否真的带上了 B站登录态
    /// （`SESSDATA` / `DedeUserID`）。
    ///
    /// 用来区分两种「-403 访问权限不足」：**没登录**（提示去授权）与
    /// **已经登录但仍被拒**（多为出口 IP 被限流/该接口额外校验，提示别再重复登录）。
    /// 早期把两者混为一谈，用户明明授权过却一直被叫去「登录一次」。
    last_official_had_login: std::sync::atomic::AtomicBool,
}

/// 请求 B站时使用的桌面 Chrome User-Agent。
///
/// 必须是**完整**的 UA。早期这里只写了
/// `Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36`，
/// 缺掉 `(KHTML, like Gecko) Chrome/… Safari/537.36` 这一段——这不是一个合法
/// 的浏览器 UA，B站会直接判定成爬虫并返回 412 风控。后果很隐蔽：官方投稿
/// 接口永远打不通，「列出主播历史」只能退回 RSSHub 的半截列表，于是官方
/// 46 条只列出 45 条，用户怎么点都补不上那一条。
const DESKTOP_CHROME_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36";

/// B站相关请求统一使用的中文语言偏好（真实浏览器都会带，缺了更像爬虫）。
const ZH_ACCEPT_LANGUAGE: &str = "zh-CN,zh;q=0.9,en;q=0.8";

/// B站相关请求统一的默认头：UA + Accept-Language。
fn bilibili_default_headers() -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    if let Ok(v) = reqwest::header::HeaderValue::from_str(DESKTOP_CHROME_UA) {
        headers.insert(reqwest::header::USER_AGENT, v);
    }
    if let Ok(v) = reqwest::header::HeaderValue::from_str(ZH_ACCEPT_LANGUAGE) {
        headers.insert(reqwest::header::ACCEPT_LANGUAGE, v);
    }
    headers
}

/// 构造一个带完整浏览器头的 reqwest 客户端。B站接口对 UA/语言头敏感，
/// 统一从这里出，避免各处再写出残缺的 UA。
fn browser_client(timeout_secs: u64) -> reqwest::Client {
    crate::net::client_builder()
        .default_headers(bilibili_default_headers())
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .build()
        .unwrap_or_default()
}

impl Default for BilibiliAdapter {
    fn default() -> Self {
        Self {
            client: browser_client(20),
            cookie: String::new(),
            project_dir: PathBuf::from("."),
            expected_total: std::sync::atomic::AtomicU32::new(0),
            last_official_code: std::sync::atomic::AtomicI64::new(0),
            last_official_had_login: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

impl BilibiliAdapter {
    /// 记录官方投稿接口最近一次的失败码（负数=B站业务码，`-1000`=HTTP 失败，
    /// `-1001`=网络/解析失败，`0`=成功）。仅在失败时调用；成功路径显式记 0。
    fn note_official_code(&self, code: i64) {
        self.last_official_code
            .store(code, std::sync::atomic::Ordering::Relaxed);
    }

    /// 请求一次 B站官方投稿接口，解析完整元数据并返回可读失败原因。
    ///
    /// 返回 `((videos, total), reason)`：
    /// - `total` 是 `data.page.count`（总投稿数，可能缺失），供分页判断；
    /// - `reason` 非空表示本页确有可解释的失败（如 `-352 风控校验失败` /
    ///   `-403 访问权限不足`），供顶层区分“风控”与“仅差登录”，避免日志
    ///   一律归因为 412 误导用户；空列表表示需要上层换下一组兜底。
    async fn request_upload_ids_reason(
        &self,
        signed_query: &str,
        headers: &reqwest::header::HeaderMap,
        count: u32,
    ) -> ((Vec<VideoMeta>, Option<u32>), String) {
        let signed = format!("https://api.bilibili.com/x/space/wbi/arc/search?{signed_query}");
        let Ok(resp) = self
            .client
            .get(&signed)
            .headers(headers.clone())
            .send()
            .await
        else {
            self.note_official_code(-1001);
            return ((Vec::new(), None), "官方接口请求发送失败".to_string());
        };
        // HTTP 层必须是成功状态，B站 412/5xx 时继续走 RSSHub/yt-dlp，
        // 不能让 HTML 或错误 JSON 被当成“空列表”。
        let status = resp.status();
        if !status.is_success() {
            self.note_official_code(-1000);
            return (
                (Vec::new(), None),
                format!("官方接口返回 HTTP {status}（多为 412 风控）"),
            );
        }
        let Ok(data) = resp.json::<serde_json::Value>().await else {
            self.note_official_code(-1001);
            return (
                (Vec::new(), None),
                "官方接口返回非 JSON（可能被风控页替换）".to_string(),
            );
        };
        let code = data["code"].as_i64().unwrap_or(-1);
        if code != 0 {
            self.note_official_code(code);
            let message = data["message"].as_str().unwrap_or("未知错误");
            let hint = match code {
                -352 => "（风控校验失败：设备指纹或出口 IP 被拦）",
                -403 => "（风控已过但访问权限不足：该空间接口需要登录态，请配置 B站 cookie）",
                -799 => "（请求过于频繁：出口 IP 被限流）",
                _ => "",
            };
            return (
                (Vec::new(), None),
                format!("官方接口业务错误 code={code} {message}{hint}"),
            );
        }
        // 这一页是干净的：清掉可能由上一页留下的失败码，避免成功之后仍报旧错。
        self.note_official_code(0);
        let total = data["data"]["page"]["count"]
            .as_u64()
            .and_then(|n| u32::try_from(n).ok());
        (
            (parse_official_upload_response(&data, count), total),
            String::new(),
        )
    }

    /// 取 B 站 wbi 图片并生成查询签名（w_rid/wts）。失败返回空串，调用方走无签名兜底。
    async fn wbi_query(&self, mid: &str, count: u32, page: u32) -> anyhow::Result<String> {
        // 用统一的浏览器客户端：这里的 nav 请求同属 api.bilibili.com，UA 残缺
        // 一样会被 412 拦。早期这里单写了一个「半截 UA」的客户端，导致就算
        // 投稿接口那侧已修好，签名这一步仍可能先死，整条官方链路退化成兜底源。
        let client = browser_client(20);
        // nav 是 WBI 图片 key 的来源；有配置/浏览器 Cookie 时也必须一并带上，
        // 否则即使用户已授权，这一步仍可能被 B站当成匿名出口并 412，导致
        // 官方投稿接口永远回退到 RSSHub/yt-dlp。
        let cookie = self.effective_cookie().await;
        let mut req = client
            .get("https://api.bilibili.com/x/web-interface/nav")
            .header("Referer", "https://www.bilibili.com");
        if !cookie.is_empty()
            && let Ok(v) = reqwest::header::HeaderValue::from_str(&cookie)
        {
            req = req.header(reqwest::header::COOKIE, v);
        }
        let nav = req.send().await?.json::<serde_json::Value>().await?;
        let img_url = nav["data"]["wbi_img"]["img_url"].as_str().unwrap_or("");
        let sub_url = nav["data"]["wbi_img"]["sub_url"].as_str().unwrap_or("");
        if img_url.is_empty() || sub_url.is_empty() {
            anyhow::bail!("wbi nav 缺少 img_url");
        }
        let img_key = img_url.rsplit('/').next().unwrap_or("").replace(".png", "");
        let sub_key = sub_url.rsplit('/').next().unwrap_or("").replace(".png", "");
        if img_key.is_empty() || sub_key.is_empty() {
            anyhow::bail!("wbi key 为空");
        }
        let wts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Ok(build_wbi_query(&img_key, &sub_key, mid, count, page, wts))
    }

    pub fn with_project_dir(project_dir: PathBuf) -> Self {
        Self {
            project_dir,
            ..Self::default()
        }
    }

    /// 从桌面端「B站授权」导出的 Netscape cookie 文件读取 Cookie 字符串。
    ///
    /// 读取失败（如文件存在但无权限或损坏）时返回错误，而不是静默假装没有 Cookie，
    /// 让真实授权状态和程序缺陷不会混在一起。
    fn browser_cookie_from_project(&self) -> String {
        match read_browser_cookie_file(&self.project_dir) {
            Ok(Some(text)) => cookie_header_from_netscape_public(&text),
            Ok(None) => String::new(),
            Err(e) => {
                tracing::warn!("读取 B站授权 Cookie 失败: {e:#}");
                String::new()
            }
        }
    }

    /// 用户配置 Cookie 写入旧 Python 同路径的 Netscape 文件，并转成请求头。
    ///
    /// 返回空串表示没有可用的用户 Cookie；调用方应继续使用浏览器导出或匿名兜底。
    fn configured_cookie_header(&self) -> String {
        let Some(cookie) = crate::config::Config::load(&self.project_dir)
            .ok()
            .map(|c| c.get_fupan("视频监控", "b站cookie"))
        else {
            return String::new();
        };
        let cookie = cookie.trim();
        if cookie.is_empty() {
            return String::new();
        }
        let Some(path) = ensure_user_cookie_file(&self.project_dir, cookie) else {
            return cookie.to_string();
        };
        let Ok(text) = std::fs::read_to_string(path) else {
            return cookie.to_string();
        };
        let header = cookie_header_from_netscape_public(&text);
        if header.is_empty() {
            cookie.to_string()
        } else {
            header
        }
    }

    /// 用户配置 Cookie 的 Netscape 文件（优先返回旧 Python 同路径文件）。
    ///
    /// 只有文件不存在时才现场生成；文件已存在时优先复用，避免写覆盖旧 Python 产物。
    fn user_cookie_file(&self) -> Option<PathBuf> {
        let cookie = crate::config::Config::load(&self.project_dir)
            .ok()
            .map(|c| c.get_fupan("视频监控", "b站cookie"))
            .unwrap_or_default();
        let cookie = cookie.trim();
        if cookie.is_empty() {
            return None;
        }
        let path = crate::bilibili_auth::user_cookie_path(&self.project_dir);
        if path.exists() {
            return Some(path);
        }
        ensure_user_cookie_file(&self.project_dir, cookie)
    }

    /// 是否配了登录态 Cookie（手动配置的 `b站cookie`，或设置页「B站授权」导入）。
    ///
    /// 只有登录态才可能拿到完整投稿列表；匿名请求基本必被 412 拦。用它决定
    /// 「RSSHub 已返回结果时，要不要再打一次官方接口」——不给没授权的用户
    /// 每个监控轮次都白白多一次风控暴露。
    fn has_login_cookie(&self) -> bool {
        cookie_header_has_login(&self.upload_list_cookie())
    }

    /// 投稿列表专用 Cookie：旧 Python 明确不使用匿名 buvid，只允许账号 Cookie，
    /// 否则会更容易触发 412。这里遵循同一优先级。
    fn upload_list_cookie(&self) -> String {
        let configured = self.configured_cookie_header();
        if !configured.is_empty() {
            return configured;
        }
        let browser = self.browser_cookie_from_project();
        if !browser.is_empty() {
            return browser;
        }
        // 没有账号 Cookie 时，桌面端导出的浏览器 Cookie 里通常至少会有
        // bili_ticket/buvid 等风控校验字段；这里补一次同步读取，避免上面
        // `browser_cookie_from_project` 的日志路径把可恢复文件当成空状态。
        match read_browser_cookie_file(&self.project_dir) {
            Ok(Some(text)) => {
                let header = cookie_header_from_netscape_public(&text);
                if header.is_empty() {
                    tracing::warn!("B站授权 Cookie 文件存在但没有可导出字段");
                }
                header
            }
            Ok(None) => String::new(),
            Err(e) => {
                tracing::warn!("读取 B站授权 Cookie 失败: {e:#}");
                String::new()
            }
        }
    }

    /// 在投稿列表 Cookie 之上合并匿名设备指纹（buvid3/buvid4/b_lsid）。
    ///
    /// - 先取账号/浏览器 Cookie（含 SESSDATA 等登录态）；
    /// - 再补匿名设备指纹，仅追加缺失字段，绝不覆盖登录态字段；
    /// - 设备指纹获取失败时退回原 Cookie，不影响原有行为。
    async fn upload_list_cookie_with_fingerprint(&self) -> String {
        let base = self.upload_list_cookie();
        let Some(anon) = ensure_anon_cookie(&self.project_dir).await else {
            tracing::debug!(
                "B站投稿列表 Cookie（无匿名指纹）: 含登录态={} 字段数={}",
                cookie_header_has_login(&base),
                base.split(';').filter(|s| !s.trim().is_empty()).count()
            );
            return base;
        };
        let Ok(text) = std::fs::read_to_string(anon) else {
            return base;
        };
        let fingerprint = cookie_header_from_netscape_public(&text);
        if fingerprint.is_empty() {
            return base;
        }
        let merged = merge_cookie_headers(&base, &fingerprint);
        tracing::debug!(
            "B站投稿列表 Cookie: 含登录态={} 字段数={}",
            cookie_header_has_login(&merged),
            merged.split(';').filter(|s| !s.trim().is_empty()).count()
        );
        merged
    }

    pub fn with_cookie_and_project_dir(cookie: impl Into<String>, project_dir: PathBuf) -> Self {
        Self {
            cookie: cookie.into(),
            project_dir,
            ..Self::default()
        }
    }

    async fn effective_cookie(&self) -> String {
        if !self.cookie.is_empty() {
            self.cookie.clone()
        } else if let Some(cookie) = self.user_cookie_file() {
            let Ok(text) = std::fs::read_to_string(cookie) else {
                return self.browser_cookie_from_project();
            };
            cookie_header_from_netscape_public(&text)
        } else if let Some(anon) = ensure_anon_cookie(&self.project_dir).await {
            let Ok(text) = std::fs::read_to_string(anon) else {
                return self.browser_cookie_from_project();
            };
            cookie_header_from_netscape_public(&text)
        } else {
            self.browser_cookie_from_project()
        }
    }

    fn mid_from_url(url: &str) -> Option<String> {
        let trimmed = url.trim();
        // 纯数字直接当 UID（用户在「批量下载」里只填了数字的情况）。
        if !trimmed.is_empty() && trimmed.chars().all(|c| c.is_ascii_digit()) {
            return Some(trimmed.to_string());
        }
        // space.bilibili.com/数字 与 m.bilibili.com/space/数字 都要认。
        let re = regex::Regex::new(r"(?:space\.bilibili\.com|/space)/(\d+)").ok()?;
        re.captures(trimmed)
            .and_then(|caps| caps.get(1))
            .map(|m| m.as_str().to_string())
    }

    async fn view(&self, bvid: &str) -> Option<VideoMeta> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::REFERER,
            reqwest::header::HeaderValue::from_static("https://www.bilibili.com"),
        );
        let cookie = self.effective_cookie().await;
        if !cookie.is_empty()
            && let Ok(v) = reqwest::header::HeaderValue::from_str(&cookie)
        {
            headers.insert(reqwest::header::COOKIE, v);
        }
        let url = format!("https://api.bilibili.com/x/web-interface/view?bvid={bvid}");
        let resp = match self.client.get(&url).headers(headers).send().await {
            Ok(r) => r,
            Err(_) => return None,
        };
        let resp = match resp.error_for_status() {
            Ok(r) => r,
            Err(_) => return None,
        };
        let data: serde_json::Value = match resp.json().await {
            Ok(d) => d,
            Err(_) => return None,
        };
        if data.get("code").and_then(|c| c.as_i64()) != Some(0) {
            return None;
        }
        let d = &data["data"];
        if d.get("title")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .is_empty()
        {
            return None;
        }
        let ts = d["pubdate"].as_i64().unwrap_or(0);
        Some(VideoMeta {
            id: bvid.to_string(),
            platform: "bilibili".to_string(),
            url: format!("https://www.bilibili.com/video/{bvid}"),
            title: d["title"].as_str().unwrap_or(bvid).to_string(),
            cover: d["pic"]
                .as_str()
                .unwrap_or("")
                .replace("http://", "https://"),
            published_at: if ts > 0 {
                unix_to_date(ts)
            } else {
                String::new()
            },
            duration: d["duration"].as_i64().unwrap_or(0),
        })
    }
}

impl Platform for BilibiliAdapter {
    fn name(&self) -> &'static str {
        "bilibili"
    }

    fn expected_total(&self) -> Option<u32> {
        match self
            .expected_total
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            0 => None,
            n => Some(n),
        }
    }

    /// 把「官方接口这次为什么没给出完整列表」翻译成用户能照着做的一句话。
    ///
    /// 关键区分（顺序即严重度，越靠前越需要用户动手）：
    /// - `-403` 访问权限不足：风控已过，**只差登录态**——这就是「官方 46 条、
    ///   界面 45 条」在当前机器上的真实原因，用户点一次「B站授权」即可补齐；
    /// - `-352` 风控校验失败：设备指纹或出口 IP 被拦，换网络/稍后重试或授权；
    /// - `-799` 频率限制：出口 IP 被限流，只能等；
    /// - `-1000` HTTP 层失败（412 等）、`-1001` 网络/签名失败：偏环境问题。
    ///
    /// 返回 `None` 表示官方接口这次没失败（成功，或压根没调用）。
    fn authoritative_hint(&self) -> Option<String> {
        let code = self
            .last_official_code
            .load(std::sync::atomic::Ordering::Relaxed);
        match code {
            0 => None,
            -403 => Some(if self
                .last_official_had_login
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                "已检测到 B站 登录态，但官方接口这次仍拒绝访问（多为出口 IP 被限流，\
或该接口对本次请求额外校验）：稍后重试即可；本次列表来自兜底源，条目通常仍然齐全，\
只是取数慢一些，不需要再去登录一次"
                    .to_string()
            } else {
                "B站官方接口已放行但要求登录态：请到设置页点「B站授权」用浏览器登录一次，\
之后就能拿到该主播的完整投稿列表（当前列表来自兜底源，可能少几条）"
                    .to_string()
            }),
            -352 => Some(
                "B站风控拦下了官方接口这次请求（设备指纹或出口 IP 被判定异常）：\
可稍后重试、或换一个网络出口，也可以到设置页点「B站授权」登录一次降低被误判概率"
                    .to_string(),
            ),
            -799 => Some(
                "B站提示请求过于频繁（出口 IP 被限流）：稍等几分钟再点「列出历史」即可"
                    .to_string(),
            ),
            -1000 => Some(
                "B站官方接口返回了风控页（HTTP 412 等）：稍后重试，或到设置页点「B站授权」\
登录一次（当前列表来自兜底源，可能少几条）"
                    .to_string(),
            ),
            -1001 => Some(
                "B站官方接口这次没能访问成功（网络或签名失败）：请检查网络后重试；\
当前列表来自兜底源，可能少几条"
                    .to_string(),
            ),
            other => Some(format!(
                "B站官方接口报错 code={other}，本次列表来自兜底源，可能少几条：\
稍后重试，或到设置页点「B站授权」登录一次"
            )),
        }
    }

    fn list_videos<'a>(
        &'a self,
        url: &'a str,
        count: u32,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Vec<VideoMeta>>> + Send + 'a>> {
        Box::pin(async move {
            // 每次列历史都重置总数与官方失败码，避免把上一次的残留值当成这一次的答案。
            self.expected_total
                .store(0, std::sync::atomic::Ordering::Relaxed);
            self.last_official_code
                .store(0, std::sync::atomic::Ordering::Relaxed);
            self.last_official_had_login
                .store(false, std::sync::atomic::Ordering::Relaxed);
            let mid = Self::mid_from_url(url).with_context(|| {
                format!(
                    "这个地址不是 B站UP主主页，认不出是哪个 UP主：{url}。\
请改用「space.bilibili.com/数字」——在 B站打开该 UP主主页后复制地址栏；\
或改用该 UP主的任意视频链接 / b23.tv 分享短链（会自动识别 UP主）"
                )
            })?;
            // RSSHub 先试：它不直接打 B站接口，是三途里最不容易触发风控的。
            // 三个兜底都只返回“空列表”，但空的原因可能是 412 风控，也可能是
            // 网络/DNS 故障。把每级的真实原因带上来，顶层报错才不至于把
            // “api.bilibili.com 解析失败”误导成“请配置 cookie”。
            let (rsshub_ids, rsshub_err) = self.upload_ids_rsshub(&mid, count).await;
            if rsshub_ids.is_empty() {
                tracing::warn!("B站 RSSHub 未返回投稿 (0 条) ({rsshub_err})，回退官方接口");
            } else {
                tracing::debug!("B站 RSSHub 返回 {} 条", rsshub_ids.len());
            }

            // 官方 wbi/arc 接口是唯一能给出「总投稿数」（data.page.count）和完整
            // 投稿列表的源。要不要在 RSSHub 之外再打一次，取决于三件事：
            //   - RSSHub 没结果 → 必须试（否则只剩 yt-dlp 一条路）；
            //   - count == 0，即用户在界面上点「列出历史」要完整列表 → 必须试。
            //     只有官方接口能给出完整列表和总数；「RSSHub 有返回就跳过官方」
            //     等于默认接受「界面比官方少几条，而且不解释」。实测正是这条
            //     捷径造成「官方 46 条、界面 45 条、用户查不出为什么」。
            //   - RSSHub 有结果但用户已「B站授权」→ 也要试。早期实现在这里
            //     「RSSHub 有返回就跳过官方」，导致用户授权了也永远只能看到
            //     RSSHub 给的半截列表（官方 46 条 → 界面只列 34 条），授权形同虚设。
            // 只有「按需取最近 N 条」这种轻量请求（例如监控轮次只取最近一屏）
            // 才允许在 RSSHub 够用时省掉这次请求。
            let try_official =
                rsshub_ids.is_empty() || count == 0 || self.has_login_cookie();
            let (official_videos, official_err) = if try_official {
                self.upload_ids_official(&mid, count).await
            } else {
                tracing::debug!("B站按需取最近 {count} 条且 RSSHub 已返回结果，跳过官方接口");
                (Vec::new(), String::new())
            };
            if official_videos.is_empty() {
                if try_official {
                    tracing::warn!("B站官方投稿接口返回 0 条 ({official_err})");
                }
            } else {
                tracing::debug!("B站官方投稿接口返回 {} 条", official_videos.len());
            }

            // 合并两源：官方列表自带完整元数据（标题/封面/时长/日期）且按发布时间
            // 倒序，作为主体；RSSHub 里官方没给到的 BV（例如联合投稿）再用 view
            // 补全，尽量一条不漏。
            let mut seen: std::collections::HashSet<String> =
                official_videos.iter().map(|v| v.id.clone()).collect();
            let mut missing_bvids: Vec<String> = rsshub_ids
                .into_iter()
                .filter(|id| seen.insert(id.clone()))
                .collect();

            let mut ytdlp_err = String::new();
            if official_videos.is_empty() && missing_bvids.is_empty() {
                let (next, err) = self.upload_ids_ytdlp(&mid, count).await;
                missing_bvids = next;
                ytdlp_err = err.clone();
                if missing_bvids.is_empty() {
                    tracing::warn!("B站 yt-dlp 兜底也未返回投稿 (0 条) ({err})");
                } else {
                    tracing::debug!("B站 yt-dlp 兜底返回 {} 条", missing_bvids.len());
                }
            }
            if official_videos.is_empty() && missing_bvids.is_empty() {
                return Err(anyhow!(upload_list_failure_message(
                    &official_err,
                    &rsshub_err,
                    &ytdlp_err
                )));
            }

            // 官方 vlist 已带标题/封面/时长/日期，直接作为结果主体；
            // RSSHub/yt-dlp 补出来的 BV 号才需要并发调用 view 补全，
            // 避免把占位伪装成成功。
            let mut out = official_videos;
            if !missing_bvids.is_empty() {
                let results = futures_util::future::join_all(
                    missing_bvids.iter().map(|bvid| self.view(bvid)),
                )
                .await;
                // view 补全可能个别失败（412/超时），失败的那条会被 flatten 丢掉。
                // 以前是静默的——用户只看到「少一条」却查不出原因，这里至少留下日志。
                let filled = results.iter().filter(|r| r.is_some()).count();
                if filled < missing_bvids.len() {
                    tracing::warn!(
                        "B站 view 补全失败 {} 条（共 {} 条），这些条目本次不会出现在列表里",
                        missing_bvids.len() - filled,
                        missing_bvids.len()
                    );
                }
                out.extend(results.into_iter().flatten());
            }
            if out.is_empty() {
                return Err(anyhow!("B站视频信息补全失败，请配置 B站 cookie 或稍后重试"));
            }
            if count > 0 {
                out.truncate(count as usize);
            }
            Ok(out)
        })
    }
}

impl BilibiliAdapter {
    /// 返回 `(videos, 失败原因)`。失败原因用于顶层区分风控与网络故障，
    /// 空串表示“无具体原因可描述”。
    async fn upload_ids_official(&self, mid: &str, count: u32) -> (Vec<VideoMeta>, String) {
        // 旧 Python 的投稿列表接口明确不使用匿名 buvid：那会更容易触发 412。
        // 这里在用户配置/浏览器授权 Cookie 之上，再补匿名设备指纹
        // （buvid3/buvid4/b_lsid），但不覆盖已有的登录态字段。
        //
        // 24× 实测：space/wbi/arc/search 的风控是三级递进——
        //   匿名无指纹            → HTTP 412 / -799 硬拦截
        //   仅 buvid3             → -352 风控校验失败
        //   buvid3+buvid4+b_lsid  → -403 访问权限不足（风控已过，仅差登录）
        // 补齐指纹能让错误提示准确区分“风控”与“需登录”，不再误导用户。
        let cookie = self.upload_list_cookie_with_fingerprint().await;
        // 记下这次到底有没有带上登录态：同为 -403，原因完全不同，提示也必须不同。
        self.last_official_had_login.store(
            cookie_header_has_login(&cookie),
            std::sync::atomic::Ordering::Relaxed,
        );
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::REFERER,
            reqwest::header::HeaderValue::from_str(&format!(
                "https://space.bilibili.com/{mid}/video"
            ))
            .unwrap_or_else(|_| {
                reqwest::header::HeaderValue::from_static("https://www.bilibili.com")
            }),
        );
        if let Ok(v) = reqwest::header::HeaderValue::from_str(&cookie) {
            headers.insert(reqwest::header::COOKIE, v);
        }

        // 官方接口优先使用 WBI 签名。签名（nav）本身也可能被 B站 412/限流，
        // 一次失败不代表接口不可用：重试一次再回退 RSSHub/yt-dlp，避免
        // 偶发网络抖动让官方接口被静默跳过。
        let mut reason = String::new();
        let mut out = Vec::new();
        let mut page = 1u32;
        let mut total = None;
        // 官方网页端每页就是 40 条（`ps=40`）；跟着它走最稳，取 50 反而可能
        // 被服务端当异常参数。count=0 表示“全部历史视频”，循环直到 total 读完
        // 或 50 页护栏。
        // 分页失败时把已收集的视频交给上层兜底判断，不能把“只拿到半截”
        // 当作完整成功。
        let page_size = if count == 0 { 40 } else { count };
        loop {
            let signed = match self.wbi_query(mid, page_size, page).await {
                Ok(q) => q,
                Err(e) => {
                    self.note_official_code(-1001);
                    if reason.is_empty() {
                        reason = format!("官方接口签名失败: {e:#}");
                    }
                    break;
                }
            };
            let ((videos, page_total), page_reason) = self
                .request_upload_ids_reason(&signed, &headers, page_size)
                .await;
            if total.is_none() {
                total = page_total;
                // 官方报告的总数一拿到就记下来，供上层对比「实取条数」。
                // 差值不是被程序吞了，就是被 B站自己过滤了——用户有权知道。
                if let Some(t) = total {
                    self.expected_total
                        .store(t, std::sync::atomic::Ordering::Relaxed);
                }
            }
            if videos.is_empty() {
                if reason.is_empty() {
                    reason = if page_reason.is_empty() {
                        "官方 wbi 接口未返回 BV（可能是 412 风控或接口变更）".to_string()
                    } else {
                        page_reason
                    };
                }
                break;
            }
            out.extend(videos);
            if !should_fetch_more(count, out.len(), total, page) {
                break;
            }
            page += 1;
        }
        if out.is_empty() {
            return (Vec::new(), reason);
        }
        (out, String::new())
    }

    async fn upload_ids_rsshub(&self, mid: &str, count: u32) -> (Vec<String>, String) {
        let configured = crate::config::Config::load(&self.project_dir)
            .ok()
            .map(|c| c.get_main("网络", "RSSHub 地址"))
            .unwrap_or_default();
        let candidates = rsshub_candidates(&configured, mid);
        let mut failures = Vec::new();
        for rss in &candidates {
            // 必须给每个实例一个硬上限：RSSHub 公共实例被打到黑洞（丢包、不响应）
            // 时，裸请求会一直挂着，整条「列主播历史」就永远回不来——上层再放宽
            // 超时也没用，因为根本没有可等的终态。20 秒 × 2 个实例 = 最多 40 秒。
            match self
                .client
                .get(rss)
                .timeout(Duration::from_secs(20))
                .send()
                .await
            {
                Ok(resp) => {
                    let status = resp.status();
                    if !status.is_success() {
                        failures.push(format!("{rss} → HTTP {status}"));
                        continue;
                    }
                    match resp.text().await {
                        Ok(text) if text.starts_with('<') => {
                            let ids = extract_bvids(&text);
                            if ids.is_empty() {
                                failures.push(format!("{rss} → 返回 HTML 但未解析出 BV 号"));
                                continue;
                            }
                            let limit = if count == 0 {
                                usize::MAX
                            } else {
                                count as usize
                            };
                            return (ids.into_iter().take(limit).collect(), String::new());
                        }
                        Ok(_) => {
                            failures.push(format!("{rss} → 返回非 HTML 内容"));
                            continue;
                        }
                        Err(e) => {
                            failures.push(format!("{rss} → 响应读取失败: {e:#}"));
                            continue;
                        }
                    }
                }
                Err(e) => {
                    failures.push(format!("{rss} → 请求失败: {e:#}"));
                }
            }
        }
        (
            Vec::new(),
            format!("RSSHub 全部实例失败: {}", failures.join("；")),
        )
    }

    /// 把 Cookie 文件复制到 `<run>/ytdlp_cookies/` 下再交给 yt-dlp。
    ///
    /// yt-dlp 会把自己维护的 cookie jar 写回 `--cookies` 指向的文件。若直接
    /// 指向「B站授权」导出的那份，一次匿名 buvid 刷新就会把用户登录得到的
    /// SESSDATA 覆盖掉，表现是「设置页明明授权过，列历史还是匿名」且无声无息。
    /// 复制失败时退回原路径：宁可冒被覆盖的风险，也不能让这次请求没 Cookie。
    fn ytdlp_cookie_copy(&self, src: &std::path::Path) -> std::path::PathBuf {
        let Some(name) = src.file_name() else {
            return src.to_path_buf();
        };
        let dir = crate::paths::run_dir(&self.project_dir).join("ytdlp_cookies");
        if std::fs::create_dir_all(&dir).is_err() {
            return src.to_path_buf();
        }
        let dst = dir.join(name);
        match std::fs::copy(src, &dst) {
            Ok(_) => dst,
            Err(e) => {
                tracing::warn!("复制 yt-dlp Cookie 副本失败（继续使用原文件）: {e:#}");
                src.to_path_buf()
            }
        }
    }

    async fn upload_ids_ytdlp(&self, mid: &str, count: u32) -> (Vec<String>, String) {
        // 与旧 Python 一致：优先用户配置 Cookie，其次桌面端浏览器授权 Cookie，
        // 再补匿名 buvid 文件；全部没有时才裸请求一次。Cookie 一律转成
        // `yt-dlp --cookies` 文件，不用易被编码/换行截断的 `--add-header`。
        // B站会间歇性 412：同一 Cookie 文件失败后立即重试一次，再换下一组。
        let candidate = self
            .project_venv_ytdlp()
            .unwrap_or_else(ytdlp_path_fallback);
        let user_cookies = self.user_cookie_file();
        let browser_cookies = browser_cookie_path(&self.project_dir);
        let anon = crate::bilibili_auth::ensure_anon_cookie(&self.project_dir).await;
        // 顺序：用户配置 -> 浏览器授权 -> 匿名 buvid；None 表示“该组不存在”。
        let mut cookie_options: Vec<Option<PathBuf>> = vec![
            user_cookies,
            Some(browser_cookies).filter(|p| p.exists()),
            anon,
        ];
        // 无任何 Cookie 文件时也试一次裸请求，避免“配置为空但网络可匿名”时
        // 明明能取到列表却提前返回失败。
        if cookie_options.is_empty() {
            cookie_options.push(None);
        }
        let mut last_error = String::new();
        // 每个 Cookie 组最多尝试 2 次；单次 yt-dlp 执行 30 秒超时，由
        // ManagedChild 统一回收进程组，不会让 412/网络卡顿无限拖住监控轮次。
        // 两次尝试之间加退避，避免风控/限流时立即连打同一出口加重 412。
        for cookie_file in cookie_options {
            // yt-dlp 会把自己维护的 cookie jar **写回** `--cookies` 指向的文件
            // （文件头会变成 "generated by yt-dlp. Do not edit."）。若直接指向
            // 「B站授权」导出的那份，一次请求失败或一次匿名 buvid 刷新就可能把
            // 用户登录得到的 SESSDATA 冲掉——于是「设置页明明授权过，列历史却
            // 依然匿名」，且完全无声。这里统一只把**副本**交给 yt-dlp。
            let cookie_file = cookie_file.map(|p| self.ytdlp_cookie_copy(&p));
            let label = cookie_file
                .as_deref()
                .map(|p| {
                    p.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string()
                })
                .unwrap_or_else(|| "none".to_string());
            for attempt in 0..2 {
                let mut cmd = Command::new(&candidate);
                cmd.kill_on_drop(true);
                if count > 0 {
                    cmd.args(["--flat-playlist", "--playlist-end"])
                        .arg(count.to_string());
                } else {
                    cmd.arg("--flat-playlist");
                }
                // 让 yt-dlp 顺带报出播放列表总数（`playlist_count`，实测这台机器
                // 上就是 46，与官方 `page.count` 一致）。这是官方接口打不通时
                // 唯一能拿到的权威总数——有了它才敢说「列表是全的」，而不是
                // 一边明明列全了 46 条、一边给用户弹「本次列表可能不完整」的假警报。
                cmd.args(["--print", "%(id)s\t%(playlist_count)s"])
                    .arg(format!("https://space.bilibili.com/{mid}/video"));
                // 出站分流：按目标 URL 决定直连或走用户配置的出站代理。
                cmd.args(
                    crate::net::policy()
                        .ytdlp_proxy_args(&format!("https://space.bilibili.com/{mid}/video")),
                );
                if let Some(cookie_file) = &cookie_file {
                    cmd.args(["--cookies", cookie_file.display().to_string().as_str()]);
                }
                cmd.stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped());
                let Ok(child) = ManagedChild::spawn(&mut cmd) else {
                    break;
                };
                let Ok(out) = child.wait_with_output(Duration::from_secs(30)).await else {
                    last_error = "yt-dlp 兜底超时（30 秒）".to_string();
                    break;
                };
                let text = String::from_utf8_lossy(&out.stdout);
                let (ids, total) = parse_ytdlp_flat_playlist(&text, count);
                if !ids.is_empty() {
                    // 官方接口没给出总数时用兜底源的总数补上，让「总数 vs 实取」
                    // 的比对在任何路径下都成立。compare_exchange(0 → t) 保证不会
                    // 覆盖官方给出的、更权威的那个总数。
                    if count == 0
                        && let Some(t) = total
                        && t > 0
                    {
                        let _ = self.expected_total.compare_exchange(
                            0,
                            t,
                            std::sync::atomic::Ordering::Relaxed,
                            std::sync::atomic::Ordering::Relaxed,
                        );
                    }
                    // 之前尝试可能被 412 挡掉，但只要本次 stdout 有完整 BV 列表
                    // 就按成功处理；必须清空旧错误，否则顶层日志会把成功误报成失败。
                    return (ids, String::new());
                }
                last_error = String::from_utf8_lossy(&out.stderr)
                    .chars()
                    .take(300)
                    .collect::<String>();
                warn!(
                    "yt-dlp 未返回 BV 号 (cookie={label}, attempt={}): {}",
                    attempt + 1,
                    last_error
                );
                if attempt == 0 {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
        warn!("yt-dlp 兜底全部失败: {last_error}");
        (Vec::new(), last_error)
    }

    /// 用 Electron 浏览器内核从 B站空间页提取投稿 BV 号。
    ///
    /// B站 `arc/search` 接口、RSSHub 和 yt-dlp 都可能因为匿名出口/公共实例
    /// 限制而失败；真实 Chromium 页面会跑完前端风控逻辑，是最接近桌面浏览器
    /// 的取数方式。这里只用于“取列表”，不触发下载，因此不会占用带宽写文件。
    /// 目前调度轮次不调用（冷启动 20-40 秒会撞 50 秒护栏），保留给未来
    /// 后台任务/手动诊断使用。
    #[allow(dead_code)]
    async fn upload_ids_electron(&self, mid: &str, count: u32) -> Vec<String> {
        let cookie_path = crate::bilibili_auth::browser_cookie_path(&self.project_dir);
        let cookie = match std::fs::read_to_string(&cookie_path) {
            Ok(cookie) => cookie,
            Err(e) => {
                tracing::warn!(
                    "跳过 B站 Electron 列表提取：读取 Cookie 失败 path={} err={e:#}",
                    cookie_path.display()
                );
                return Vec::new();
            }
        };
        if cookie.trim().is_empty() {
            tracing::warn!(
                "跳过 B站 Electron 列表提取：Cookie 文件为空 path={}",
                cookie_path.display()
            );
            return Vec::new();
        }
        let url = format!("https://space.bilibili.com/{mid}/video");
        let script = r#"(function(){{
  var out={{ok:false,bvids:[]}};
  try{{
    var text=document.body ? document.body.innerText : '';
    var html=document.documentElement ? document.documentElement.innerHTML : '';
    var seen={{}};
    var re=/BV[0-9A-Za-z]{{10}}/g;
    (text+'\n'+html).replace(re, function(v){{ if(!seen[v]){{seen[v]=1; out.bvids.push(v);}} return v; }});
    out.ok=out.bvids.length>0;
    if(!out.ok && window.__INITIAL_STATE__){{
      var st=window.__INITIAL_STATE__;
      var list=(st.videoList||st.space||{{}}).list;
      if(Array.isArray(list)){{
        for(var i=0;i<list.length;i++){{ if(list[i].bvid && !seen[list[i].bvid]){{ seen[list[i].bvid]=1; out.bvids.push(list[i].bvid); }} }}
      }}
    }}
    out.ok=out.bvids.length>0;
  }}catch(e){{ out.error=String(e && e.message ? e.message : e); }}
  return JSON.stringify(out);
}})()"#
            .to_string();
        let result = match crate::bilibili_electron::run_bilibili_script(
            &self.project_dir,
            &script,
            &[("FUPAN_TARGET_URL", url)],
            std::time::Duration::from_secs(45),
        )
        .await
        {
            Ok(result) => result,
            Err(e) => {
                tracing::warn!("B站 Electron 投稿列表提取失败: {e:#}");
                return Vec::new();
            }
        };
        let value = result.value;
        let Some(bvids) = value.get("bvids").and_then(|v| v.as_array()) else {
            return Vec::new();
        };
        bvids
            .iter()
            .filter_map(|v| v.as_str())
            .filter(|s| is_plausible_bvid(s))
            .take(count as usize)
            .map(ToOwned::to_owned)
            .collect()
    }

    fn project_venv_ytdlp(&self) -> Option<PathBuf> {
        // 当前平台优先：Windows venv 是 Scripts/yt-dlp.exe，Unix 是 bin/yt-dlp。
        // 再回退另一平台路径，兼容镜像/迁移目录；PATH 兜底由调用方拼平台后缀。
        let candidates = if cfg!(windows) {
            [
                self.project_dir
                    .join(".venv")
                    .join("Scripts")
                    .join("yt-dlp.exe"),
                self.project_dir.join(".venv").join("bin").join("yt-dlp"),
            ]
        } else {
            [
                self.project_dir.join(".venv").join("bin").join("yt-dlp"),
                self.project_dir
                    .join(".venv")
                    .join("Scripts")
                    .join("yt-dlp.exe"),
            ]
        };
        candidates.into_iter().find(|p| p.exists())
    }
}

/// 无项目内 yt-dlp 时回退到 PATH：Windows 必须用 `.exe`，Unix 直接裸名。
pub fn ytdlp_path_fallback() -> PathBuf {
    PathBuf::from(if cfg!(windows) {
        "yt-dlp.exe"
    } else {
        "yt-dlp"
    })
}

/// B站 BV 号是 10 位字母数字且以 BV1 开头；用于过滤 RSS/yt-dlp 输出里的脏文本。
fn is_plausible_bvid(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 12
        && text.starts_with("BV1")
        && bytes[3..].iter().all(|b| b.is_ascii_alphanumeric())
}

/// 从 B站官方 `arc/search` 成功 JSON 中提取 BV 列表。
///
/// `code != 0`、缺 `data.list.vlist` 或数组中无 BV 都视为“没有可用结果”，
/// 由上层继续 RSSHub/yt-dlp 兜底，不把风控/空响应伪装成成功。
fn parse_official_upload_response(data: &serde_json::Value, count: u32) -> Vec<VideoMeta> {
    parse_official_upload_response_in_offset(data, count, *chrono::Local::now().offset())
}

/// 与 `parse_official_upload_response` 同语义，但日期按显式时区转换；
/// 生产用本机时区，测试用固定偏移，避免结果随宿主机时区漂移。
fn parse_official_upload_response_in_offset(
    data: &serde_json::Value,
    count: u32,
    offset: chrono::FixedOffset,
) -> Vec<VideoMeta> {
    if data.get("code").and_then(|c| c.as_i64()) != Some(0) {
        return Vec::new();
    }
    let Some(rows) = data["data"]["list"]["vlist"].as_array() else {
        return Vec::new();
    };
    rows.iter()
        .filter_map(|r| {
            let bvid = r.get("bvid").and_then(|b| b.as_str())?;
            let ts = r.get("pubdate").and_then(|t| t.as_i64()).unwrap_or(0);
            Some(VideoMeta {
                id: bvid.to_string(),
                platform: "bilibili".to_string(),
                url: format!("https://www.bilibili.com/video/{bvid}"),
                title: r
                    .get("title")
                    .and_then(|t| t.as_str())
                    .unwrap_or(bvid)
                    .to_string(),
                cover: r
                    .get("pic")
                    .and_then(|p| p.as_str())
                    .unwrap_or("")
                    .replace("http://", "https://"),
                published_at: if ts > 0 {
                    unix_to_date_in_offset(ts, offset)
                } else {
                    String::new()
                },
                duration: r.get("length").and_then(|l| l.as_i64()).unwrap_or(0),
            })
        })
        .take(if count == 0 {
            rows.len()
        } else {
            count as usize
        })
        .collect()
}

/// 从 RSS/HTML 文本提取 BV 号（保持顺序、去重）。
pub fn extract_bvids(text: &str) -> Vec<String> {
    let re = regex::Regex::new(r"BV[0-9A-Za-z]{10}").unwrap();
    let mut out = Vec::new();
    for cap in re.find_iter(text) {
        let s = cap.as_str().to_string();
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

fn unix_to_date(ts: i64) -> String {
    let offset = *chrono::Local::now().offset();
    unix_to_date_in_offset(ts, offset)
}

/// 按显式时区把 Unix 时间戳转成 `YYYY-MM-DD`。
///
/// 生产语义沿用旧 Python `time.localtime`（本机时区）；单测用固定偏移，
/// 避免测试结果随 CI/容器默认时区漂移。
fn unix_to_date_in_offset(ts: i64, offset: chrono::FixedOffset) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .unwrap_or_else(|| chrono::DateTime::from_timestamp(0, 0).unwrap_or_default())
        .with_timezone(&offset)
        .format("%Y-%m-%d")
        .to_string()
}

/// B站分享短链域名：这些不含 `space.bilibili.com`，若直接交给通用分支会被当网页抓。
fn is_bilibili_short_link(url: &str) -> bool {
    ["b23.tv", "bili2233.cn", "bili22.cn", "acg.tv"]
        .iter()
        .any(|d| url.contains(d))
}

/// 跟随跳转拿到最终 URL（短链 → 真实地址）。失败返回 None，调用方退回原地址。
async fn follow_redirect(url: &str) -> Option<String> {
    let client = browser_client(15);
    let resp = client.get(url).send().await.ok()?;
    Some(resp.url().to_string())
}

/// 视频 BV 号 → UP 主 mid（官方 view 接口）。
async fn bilibili_owner_mid(bvid: &str) -> Option<String> {
    let client = crate::net::client_builder()
        .timeout(Duration::from_secs(15))
        .build()
        .ok()?;
    let resp = client
        .get(format!(
            "https://api.bilibili.com/x/web-interface/view?bvid={bvid}"
        ))
        .header(
            reqwest::header::REFERER,
            reqwest::header::HeaderValue::from_static("https://www.bilibili.com"),
        )
        .send()
        .await
        .ok()?;
    let data: serde_json::Value = resp.json().await.ok()?;
    if data.get("code").and_then(|c| c.as_i64()) != Some(0) {
        return None;
    }
    data["data"]["owner"]["mid"]
        .as_i64()
        .map(|m| m.to_string())
}

/// 把用户在各种入口粘贴的 B站链接归一化成 `space.bilibili.com/{mid}`：
///   - 空间地址（space.bilibili.com/数字、m.bilibili.com/space/数字）原样可用；
///   - `b23.tv` 等分享短链：跟随跳转拿真实地址，视频链接再经 view 接口取 UP 主 mid；
///   - 纯数字：直接当 UID。
/// 非 B站输入原样返回，交给 RssAdapter 处理其它平台/RSS 源。
pub async fn normalize_blogger_input(input: &str) -> String {
    let input = input.trim();
    if input.is_empty() {
        return String::new();
    }
    // 纯数字 → 当 UID。
    if input.chars().all(|c| c.is_ascii_digit()) {
        return format!("https://space.bilibili.com/{input}");
    }
    let mut target = input.to_string();
    if is_bilibili_short_link(input) {
        if let Some(final_url) = follow_redirect(input).await {
            target = final_url;
        }
    }
    // 视频链接（含短链跳到的视频页）→ 取 UP 主 mid，换成空间地址。
    if target.contains("/video/") || target.contains("bvid=") {
        if let Some(bv) = extract_bvids(&target).into_iter().next() {
            if let Some(mid) = bilibili_owner_mid(&bv).await {
                return format!("https://space.bilibili.com/{mid}");
            }
        }
    }
    // 空间地址归一成规范形态：只保留 space.bilibili.com/{mid}。
    //
    // 用户在 B站复制的主页地址往往带 `?spm_id_from=333.337.0.0` 这类跟踪参数；
    // 若原样透传，它会成为视频库的 source_url 主键，于是同一个人出现两条 key
    // （`space.bilibili.com/601214690` 和 `space.bilibili.com/601214690?spm_id_from=…`），
    // 按 source_url 分组的投稿列表和已下载标记就会各算各的、数量对不上。
    if let Some(mid) = BilibiliAdapter::mid_from_url(&target) {
        return format!("https://space.bilibili.com/{mid}");
    }
    target
}

/// RSS/Atom 适配器：解析标题/链接/封面/日期，返回统一 VideoMeta。
#[derive(Debug)]
pub struct RssAdapter {
    client: reqwest::Client,
}

impl Default for RssAdapter {
    fn default() -> Self {
        Self {
            client: browser_client(20),
        }
    }
}

impl RssAdapter {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Platform for RssAdapter {
    fn name(&self) -> &'static str {
        "rss"
    }

    fn list_videos<'a>(
        &'a self,
        url: &'a str,
        count: u32,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<VideoMeta>>> + Send + 'a>> {
        Box::pin(async move {
            let resp = self.client.get(url).send().await?;
            // 非成功状态或非 XML/HTML 正文都是“没拿到有效订阅”，必须明确失败。
            // 若把空列表当成正常基线，用户会永远收不到该博主的新视频通知。
            if !resp.status().is_success() {
                return Err(anyhow!(
                    "RSS 源请求失败（HTTP {}）: {url}",
                    resp.status().as_u16()
                ));
            }
            let text = resp.text().await?;
            let trimmed = text.trim_start();
            if !trimmed.starts_with('<') {
                return Err(anyhow!("RSS 源未返回 XML/HTML 内容: {url}"));
            }
            Ok(parse_rss(&text)?.into_iter().take(count as usize).collect())
        })
    }
}

const ATOM_NS: &str = "http://www.w3.org/2005/Atom";
const MRSS_NS: &str = "http://search.yahoo.com/mrss/";

/// RSS 2.0 / Atom 解析。
///
/// 使用只读 XML DOM 而不是字符串切分，才能正确识别 XML 命名空间（如
/// `<atom:link>`、`<media:thumbnail>`）和 CDATA 文本；这两类在真实订阅源里很常见，
/// 旧 Python 依赖 `xml.etree.ElementTree`，Rust 侧也必须对齐同样的解析语义。
pub fn parse_rss(text: &str) -> Result<Vec<VideoMeta>> {
    // roxmltree 默认禁止 DTD：一旦订阅源里出现 `<!DOCTYPE>` 就直接报
    // 「XML with DTD detected」。但部分 RSSHub 实例或前置代理会在 XML 里带 DTD
    // 声明，必须允许才能解析。roxmltree 是只读、不联网的 DOM 解析器，开 allow_dtd
    // 只是允许解析 DTD 子集，不会读取/加载外部实体，不存在 XXE 注入风险。
    let opt = roxmltree::ParsingOptions {
        allow_dtd: true,
        ..Default::default()
    };
    let doc = roxmltree::Document::parse_with_options(text, opt).map_err(|e| {
        let lowered = text.trim_start().to_ascii_lowercase();
        if lowered.starts_with("<!doctype html") || text.contains("<html") {
            anyhow!(
                "RSS/Atom 解析失败：返回内容像是网页（可能 RSSHub 被限流或代理拦截），不是合法订阅源"
            )
        } else {
            anyhow!("RSS/Atom XML 解析失败: {e}")
        }
    })?;

    let mut rss = Vec::new();
    for item in doc
        .descendants()
        .filter(|n| matches_local_name(*n, "item", None))
    {
        let title = child_text(item, "title");
        let link = child_text(item, "link");
        if link.is_empty() {
            continue;
        }
        let guid = child_text(item, "guid");
        let cover = media_cover(item)
            .or_else(|| element_attr(item, "enclosure", "url"))
            .unwrap_or_default();
        let published_at = normalize_rss_date(Some(child_text(item, "pubdate")));
        rss.push(VideoMeta {
            id: if guid.is_empty() { link.clone() } else { guid },
            platform: "rss".to_string(),
            url: link,
            title,
            cover,
            published_at,
            duration: 0,
        });
    }
    if !rss.is_empty() {
        return Ok(rss);
    }

    let mut atom = Vec::new();
    for entry in doc
        .descendants()
        .filter(|n| matches_local_name(*n, "entry", Some(ATOM_NS)))
    {
        let title = namespaced_child_text(entry, ATOM_NS, "title");
        let link = atom_link(entry);
        if link.is_empty() {
            continue;
        }
        let guid = namespaced_child_text(entry, ATOM_NS, "id");
        let cover = media_cover(entry).unwrap_or_default();
        let published_at = {
            let published = namespaced_child_text(entry, ATOM_NS, "published");
            if published.is_empty() {
                namespaced_child_text(entry, ATOM_NS, "updated")
            } else {
                published
            }
        };
        atom.push(VideoMeta {
            id: if guid.is_empty() { link.clone() } else { guid },
            platform: "rss".to_string(),
            url: link,
            title,
            cover,
            published_at: normalize_rss_date(Some(published_at)),
            duration: 0,
        });
    }
    Ok(atom)
}

fn child_text<'a, 'input>(node: roxmltree::Node<'a, 'input>, name: &str) -> String {
    node.children()
        .filter(|n| n.is_element() && matches_local_name(*n, name, None))
        .find_map(|n| {
            let text = n.text().unwrap_or("").trim();
            (!text.is_empty()).then(|| text.to_string())
        })
        .unwrap_or_default()
}

fn matches_local_name<'a, 'input>(
    node: roxmltree::Node<'a, 'input>,
    name: &str,
    allowed_namespace: Option<&str>,
) -> bool {
    if !node.is_element() || node.tag_name().name() != name {
        return false;
    }
    match allowed_namespace {
        Some(ns) => {
            node.tag_name().namespace() == Some(ns) || node.tag_name().namespace().is_none()
        }
        None => true,
    }
}

fn namespaced_child_text<'a, 'input>(
    node: roxmltree::Node<'a, 'input>,
    namespace: &str,
    name: &str,
) -> String {
    node.children()
        .filter(|n| {
            n.is_element()
                && (n.tag_name().namespace() == Some(namespace)
                    || n.tag_name().namespace().is_none())
                && n.has_tag_name(name)
        })
        .find_map(|n| {
            let text = n.text().unwrap_or("").trim();
            (!text.is_empty()).then(|| text.to_string())
        })
        .unwrap_or_default()
}

fn element_attr<'a, 'input>(
    node: roxmltree::Node<'a, 'input>,
    name: &str,
    attr: &str,
) -> Option<String> {
    node.children()
        .filter(|n| n.is_element() && n.has_tag_name(name))
        .find_map(|n| {
            n.attribute(attr)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned)
        })
}

fn media_cover<'a, 'input>(node: roxmltree::Node<'a, 'input>) -> Option<String> {
    ["thumbnail", "content"].into_iter().find_map(|name| {
        node.children()
            .filter(|n| {
                n.is_element() && n.tag_name().namespace() == Some(MRSS_NS) && n.has_tag_name(name)
            })
            .find_map(|n| {
                n.attribute("url")
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(ToOwned::to_owned)
            })
    })
}

fn atom_link<'a, 'input>(node: roxmltree::Node<'a, 'input>) -> String {
    node.children()
        .find(|n| {
            n.is_element() && n.tag_name().namespace() == Some(ATOM_NS) && n.has_tag_name("link")
        })
        .and_then(|n| n.attribute("href"))
        .unwrap_or("")
        .trim()
        .to_string()
}

/// 把 RSS 2.0 / Atom 常见日期规范成 `YYYY-MM-DD`，失败时回退 `YYYY-MM-DD` 前缀。
/// 与旧 Python `video_monitor.py` 的 `_rss_date` 语义保持一致。
fn normalize_rss_date(raw: Option<String>) -> String {
    let raw = raw.unwrap_or_default();
    let raw = raw.trim();
    if raw.is_empty() {
        return String::new();
    }
    // RFC 2822 / RFC 1123 日期（RSS 2.0 常用）。
    let rfc = chrono::DateTime::parse_from_rfc2822(raw);
    if let Ok(dt) = rfc {
        return dt.format("%Y-%m-%d").to_string();
    }
    // ISO 8601（Atom 常用），含 `Z` 或时区偏移。
    let iso = chrono::DateTime::parse_from_rfc3339(raw);
    if let Ok(dt) = iso {
        return dt.format("%Y-%m-%d").to_string();
    }
    // 兼容无时区的基础日期时间。
    let plain = chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S")
        .map(|dt| dt.format("%Y-%m-%d").to_string());
    if let Ok(date) = plain {
        return date;
    }
    // 已无法识别时，保留前 10 位；连日期前缀都没有则返回空串。
    let date = raw.chars().take(10).collect::<String>();
    if date.len() == 10 && date.chars().nth(4) == Some('-') && date.chars().nth(7) == Some('-') {
        date
    } else {
        String::new()
    }
}

/// 抖音适配器：双通道。
///
/// 1. **首选 · Python 桥纯 HTTP（免登录）**：`fupan/douyin_post_list` 用项目自带
///    a_bogus 签名调 `aweme/v1/web/aweme/post`，匿名 ttwid 即可取列表，
///    失败自动用「抖音授权」cookie 兜底重试（2026-09-25 实测两在监账号可用）。
/// 2. **兜底 · Electron 隐藏浏览器**：拦截 `/web/aweme/post` 接口响应，
///    抗风控能力最强，但要求先做「抖音授权」扫码登录。
///
/// 抖音短视频监控自 1.7.0 起不再强制授权；授权只作为兜底通道存在。
#[derive(Debug)]
pub struct DouyinAdapter {
    project_dir: PathBuf,
}

impl DouyinAdapter {
    pub fn new(project_dir: PathBuf) -> Self {
        Self { project_dir }
    }

    /// 抖音登录态 Cookie 的 Netscape 文件路径（由 douyin-cookie.cjs 导出）。
    fn cookie_path(&self) -> PathBuf {
        crate::paths::run_dir(&self.project_dir).join("douyin_browser_cookies.txt")
    }

    /// 从抖音用户主页 URL 提取 sec_uid（`MS4wLjABAAAA` 开头的加密 UID）。
    /// 支持 `douyin.com/user/<sec_uid>` 长链；短链 `v.douyin.com/xxx` 需先解析，
    /// 这里先只识别长链，短链返回 None 并提示用户换成完整主页链接。
    fn sec_uid_from_url(url: &str) -> Option<String> {
        if let Some(rest) = url.split("douyin.com/user/").nth(1) {
            let uid = rest.split(['/', '?', '&']).next().unwrap_or("").trim();
            if uid.starts_with("MS4wLjABAAAA") {
                return Some(uid.to_string());
            }
        }
        None
    }

    /// 把毫秒时间戳转成 `YYYY-MM-DD HH:MM:SS`。
    fn ts_to_date(ts_ms: i64) -> String {
        if ts_ms <= 0 {
            return String::new();
        }
        chrono::DateTime::from_timestamp(ts_ms / 1000, 0)
            .map(|dt| {
                dt.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string()
            })
            .unwrap_or_default()
    }

    /// 首选通道：Python 桥纯 HTTP 列表提取（免登录，a_bogus 签名）。
    /// 输出契约见 `fupan/douyin_post_list.py`：{ok, videos:[...]} / {ok:false, error}。
    /// 桥把失败透成 error 字段，这里转成 Err 交给上层分层/记失败——
    /// 绝不能把失败伪装成空列表（监控坏了用户也看不到）。
    async fn list_via_python_bridge(&self, sec_uid: &str, count: u32) -> Result<Vec<VideoMeta>> {
        let python = crate::service::python_bin(&self.project_dir);
        if !crate::service::python_binary_available(&python) {
            anyhow::bail!("找不到可用的 Python 环境: {}", python.display());
        }
        let mut cmd = Command::new(&python);
        cmd.kill_on_drop(true)
            .arg("-m")
            .arg("fupan.douyin_post_list")
            .arg("--sec-uid")
            .arg(sec_uid)
            .arg("--count")
            .arg(count.max(1).to_string())
            .current_dir(&self.project_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = ManagedChild::spawn(&mut cmd).context("启动抖音列表桥接失败")?;
        let output = match child.wait_with_output(Duration::from_secs(40)).await {
            Ok(o) => o,
            Err(crate::process::ChildError::Io(e)) => {
                return Err(e).context("读取抖音列表桥接输出失败")
            }
            Err(crate::process::ChildError::Timeout) => {
                anyhow::bail!("抖音列表桥接超时（40 秒）")
            }
        };
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!(
                "抖音列表桥接退出码 {}: {}",
                output.status,
                stderr.chars().take(300).collect::<String>()
            );
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let value: serde_json::Value = serde_json::from_str(stdout.trim()).with_context(|| {
            format!(
                "抖音列表桥接输出非 JSON: {}",
                stdout.chars().take(200).collect::<String>()
            )
        })?;
        parse_douyin_bridge_videos(&value)
    }
}

/// 解析 `fupan/douyin_post_list` 的列表契约（纯函数，便于锁契约）。
/// 失败契约（ok:false 或无 videos）一律 Err——失败不许伪装成空列表。
fn parse_douyin_bridge_videos(value: &serde_json::Value) -> Result<Vec<VideoMeta>> {
    if !value.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        let err = value
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("未知错误");
        anyhow::bail!("{err}");
    }
    let items = value
        .get("videos")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut videos = Vec::with_capacity(items.len());
    for item in items {
        let id = item
            .get("aweme_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if id.is_empty() {
            continue;
        }
        let create_secs = item.get("create_time").and_then(|v| v.as_i64()).unwrap_or(0);
        videos.push(VideoMeta {
            id: id.to_string(),
            platform: "douyin".to_string(),
            url: item
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            title: item
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            cover: item
                .get("cover")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            published_at: DouyinAdapter::ts_to_date(create_secs * 1000),
            duration: item.get("duration").and_then(|v| v.as_i64()).unwrap_or(0),
        });
    }
    if videos.is_empty() {
        anyhow::bail!("抖音列表桥接返回空列表");
    }
    Ok(videos)
}

impl Platform for DouyinAdapter {
    fn name(&self) -> &'static str {
        "douyin"
    }

    fn list_videos<'a>(
        &'a self,
        url: &'a str,
        count: u32,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<VideoMeta>>> + Send + 'a>> {
        Box::pin(async move {
            let sec_uid = Self::sec_uid_from_url(url).ok_or_else(|| {
                anyhow!(
                    "抖音视频源需要「完整主页链接」（douyin.com/user/MS4wLjABAAAA... 开头），\
                     短链或抖音号无法直接识别，请在抖音 App 打开博主主页 → 分享 → 复制链接"
                )
            })?;
            // 首选：Python 桥纯 HTTP（免登录）。失败不静默——带上原因回退授权通道。
            let bridge_err = match self.list_via_python_bridge(&sec_uid, count).await {
                Ok(videos) => return Ok(videos),
                Err(e) => e,
            };
            tracing::warn!("抖音免登录列表通道失败，回退 Electron 授权通道: {bridge_err:#}");
            // 兜底：Electron 隐藏浏览器（需要「抖音授权」cookie）。
            let cookie_path = self.cookie_path();
            if !cookie_path.exists() {
                return Err(anyhow!(
                    "抖音视频列表获取失败: {bridge_err:#}；且尚未「抖音授权」，无浏览器兜底通道"
                ));
            }
            let target = format!("https://www.douyin.com/user/{sec_uid}");
            // Electron runner 自己读取 douyin_browser_cookies.txt 并注入；
            // payload 只需要目标 URL（FUPAN_SCRIPT_FILE 存 URL，与 B站脚本约定一致）。
            let result = crate::bilibili_electron::run_electron_script(
                &self.project_dir,
                &target,
                &[],
                std::time::Duration::from_secs(90),
                "douyin-video-list.cjs",
            )
            .await
            .with_context(|| format!("抖音 Electron 列表提取失败: {target}"))?;

            let value = result.value;
            let items = value
                .get("items")
                .and_then(|v| v.as_array())
                .ok_or_else(|| {
                    anyhow!(
                        "抖音列表提取返回异常: {}",
                        value
                            .get("error")
                            .and_then(|e| e.as_str())
                            .unwrap_or("无 items 字段")
                    )
                })?;

            let mut videos = Vec::with_capacity(items.len());
            for item in items {
                let id = item.get("id").and_then(|v| v.as_str()).unwrap_or_default();
                if id.is_empty() {
                    continue;
                }
                let title = item
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let url = item
                    .get("url")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&format!("https://www.douyin.com/video/{id}"))
                    .to_string();
                let cover = item
                    .get("cover")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let created_at = item.get("created_at").and_then(|v| v.as_i64()).unwrap_or(0);
                let duration = item.get("duration").and_then(|v| v.as_i64()).unwrap_or(0);
                videos.push(VideoMeta {
                    id: id.to_string(),
                    platform: "douyin".to_string(),
                    url,
                    title,
                    cover,
                    published_at: Self::ts_to_date(created_at),
                    duration,
                });
            }
            if videos.is_empty() {
                return Err(anyhow!(
                    "抖音列表提取成功但为空（博主可能暂无作品或触发风控）"
                ));
            }
            Ok(videos.into_iter().take(count as usize).collect())
        })
    }
}

// 带项目根的平台注册表：yt-dlp 兜底按项目 .venv 查找，跨目录运行也可用。

/// 从抖音主页 URL 解析抖音号（unique_id）——供 monitor_add 把直播监控一并挂上。
///
/// 走 iesdouyin 公开 user/info 接口：免登录、免签名、免浏览器
/// （2026-09-25 实测两个在监 sec_uid 均正确返回 unique_id）。
/// 旧方案（patchright headless Chrome 拦截 profile/other 接口）已被抖音
/// JSVM 验证页干掉——页面不再自发请求该接口，脚本恒超时，已弃用。
pub async fn douyin_unique_id_from_url(
    url: &str,
    _project_dir: &std::path::Path,
) -> anyhow::Result<serde_json::Value> {
    let sec_uid = url
        .split("/user/")
        .nth(1)
        .and_then(|s| s.split('?').next())
        .unwrap_or("")
        .trim()
        .to_string();
    if sec_uid.is_empty() || !sec_uid.starts_with("MS4wLjAB") {
        anyhow::bail!("抖音主页链接缺少有效 sec_uid");
    }
    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (iPhone; CPU iPhone OS 16_6 like Mac OS X) AppleWebKit/605.1.15")
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .context("构建 HTTP 客户端失败")?;
    let api = format!("https://www.iesdouyin.com/web/api/v2/user/info/?sec_uid={sec_uid}");
    let resp = client
        .get(&api)
        .send()
        .await
        .context("请求抖音用户信息接口失败")?;
    let value: serde_json::Value = resp
        .json()
        .await
        .context("抖音用户信息接口返回非 JSON（可能被风控）")?;
    let (unique_id, nickname) = parse_iesdouyin_user_info(&value)?;
    Ok(serde_json::json!({
        "ok": true,
        "unique_id": unique_id,
        "nickname": nickname,
    }))
}

/// 解析 iesdouyin user/info 响应，抽出（unique_id, nickname）。纯函数便于测试。
fn parse_iesdouyin_user_info(value: &serde_json::Value) -> anyhow::Result<(String, String)> {
    let (_, unique_id, nickname) = parse_iesdouyin_user_info_full(value)?;
    if unique_id.is_empty() || unique_id == "0" {
        anyhow::bail!("抖音用户信息接口未返回有效 unique_id（用户不存在或接口变更）");
    }
    Ok((unique_id, nickname))
}

/// 判断输入是否为裸抖音号（如 Glx6768、xiguahaochi96）：
/// 不是 URL、不含路径/空白、不是纯数字（纯数字按 B站 room/space ID 处理）。
pub fn looks_like_douyin_id(input: &str) -> bool {
    let v = input.trim();
    if v.is_empty() || v.len() > 64 {
        return false;
    }
    if v.contains("://") || v.contains('/') || v.contains(char::is_whitespace) {
        return false;
    }
    if v.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    v.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// 抖音号（unique_id）→ (sec_uid, 昵称)。
/// iesdouyin 公开 user/info 接口直接吃 unique_id：免登录、免签名、免浏览器
/// （2026-09-26 实测 Glx6768→关立新、xiguahaochi96→侯翠翠 均命中 sec_uid）。
/// 这取代了已弃用的 patchright Chrome 反查（被风控、150 秒超时把前端拖死）。
pub async fn douyin_user_info_by_douyin_id(douyin_id: &str) -> anyhow::Result<(String, String)> {
    let id = douyin_id.trim();
    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (iPhone; CPU iPhone OS 16_6 like Mac OS X) AppleWebKit/605.1.15")
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .context("构建 HTTP 客户端失败")?;
    let api = format!("https://www.iesdouyin.com/web/api/v2/user/info/?unique_id={id}");
    let resp = client
        .get(&api)
        .send()
        .await
        .context("请求抖音用户信息接口失败")?;
    let value: serde_json::Value = resp
        .json()
        .await
        .context("抖音用户信息接口返回非 JSON（可能被风控）")?;
    let (sec_uid, _, nickname) = parse_iesdouyin_user_info_full(&value)?;
    if sec_uid.is_empty() {
        anyhow::bail!("抖音号不存在或接口未返回 sec_uid: {id}");
    }
    Ok((sec_uid, nickname))
}

/// 裸抖音号 → (直播间 URL, 主页 URL, 昵称)。添加流程的统一解析入口：
/// 直播走 live.douyin.com/{抖音号}（免登录检测），短视频走主页 sec_uid。
pub async fn resolve_douyin_id_to_urls(
    douyin_id: &str,
) -> anyhow::Result<(String, String, String)> {
    let id = douyin_id.trim();
    let (sec_uid, nickname) = douyin_user_info_by_douyin_id(id).await?;
    Ok((
        format!("https://live.douyin.com/{id}"),
        format!("https://www.douyin.com/user/{sec_uid}"),
        nickname,
    ))
}

/// 解析 iesdouyin user/info 响应，抽出（sec_uid, unique_id, nickname）。纯函数便于测试。
fn parse_iesdouyin_user_info_full(
    value: &serde_json::Value,
) -> anyhow::Result<(String, String, String)> {
    let status_code = value
        .get("status_code")
        .and_then(|c| c.as_i64())
        .unwrap_or(-1);
    if status_code != 0 {
        anyhow::bail!("抖音用户信息接口返回错误 status_code={status_code}");
    }
    let info = value
        .get("user_info")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let sec_uid = info
        .get("sec_uid")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let unique_id = info
        .get("unique_id")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let nickname = info
        .get("nickname")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    if sec_uid.is_empty() && (unique_id.is_empty() || unique_id == "0") {
        anyhow::bail!("抖音用户信息接口未返回有效用户（用户不存在或接口变更）");
    }
    Ok((sec_uid, unique_id, nickname))
}

/// 按名字搜索主播（P4 起点：B 站搜索接口基础版；抖音 sec_uid 搜索后续完善）。
///
/// 返回结构：`{ok, candidates: [{platform, name, url, mid?, room_id?, sign?, hint?}]}`
/// - B 站：`api.bilibili.com/x/web-interface/search/type?search_type=bili_user`
///   风控（-352/412）时返回明确错误而不是静默空结果。
/// - 抖音：当前返回提示条目，等 P4 接入 sec_uid 搜索。
pub async fn search_anchor(
    name: &str,
    _project_dir: &Path,
    cfg: &crate::config::Config,
) -> anyhow::Result<serde_json::Value> {
    use serde_json::json;
    let client = reqwest::Client::new();
    let keyword = reqwest::Url::parse(&format!(
        "https://api.bilibili.com/x/web-interface/search/type?search_type=bili_user&keyword={name}"
    ))?;
    let mut req = client
        .get(keyword)
        .header(
            reqwest::header::USER_AGENT,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36",
        )
        .header(reqwest::header::REFERER, "https://search.bilibili.com/");
    let cookie = cfg.get_main("Cookie", "b站cookie");
    if !cookie.is_empty() {
        if let Ok(v) = reqwest::header::HeaderValue::from_str(&cookie) {
            req = req.header(reqwest::header::COOKIE, v);
        }
    }
    let resp: serde_json::Value = req.send().await?.json().await?;
    let code = resp.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
    if code != 0 {
        let msg = resp
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("未知错误")
            .to_string();
        anyhow::bail!(
            "B站搜索被风控（code={code}，{msg}）。可稍后再试或直接提供主页 URL 给 monitor_add"
        );
    }
    let mut candidates = Vec::new();
    if let Some(users) = resp.pointer("/data/result").and_then(|v| v.as_array()) {
        for u in users.iter().take(10) {
            let uname = u.get("uname").and_then(|v| v.as_str()).unwrap_or_default();
            let mid = u.get("mid").and_then(|v| v.as_i64()).unwrap_or(0);
            if mid == 0 {
                continue;
            }
            candidates.push(json!({
                "platform": "bilibili",
                "name": uname,
                "url": format!("https://space.bilibili.com/{mid}"),
                "mid": mid,
                "room_id": u.get("room_id").and_then(|v| v.as_i64()).unwrap_or(0),
                "sign": u.get("sign").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            }));
        }
    }
    // 抖音：带登录态 cookie 尝试 discover/search，无 cookie 或风控时给明确提示。
    let dy = DouyinAdapter::new(_project_dir.to_path_buf());
    let cookie_file = dy.cookie_path();
    if cookie_file.exists() {
        let keyword_url = reqwest::Url::parse(&format!(
            "https://www.douyin.com/aweme/v1/web/discover/search/?keyword={name}&search_channel=aweme_user&search_type=user"
        ))?;
        let dy_req = client
            .get(keyword_url)
            .header(
                reqwest::header::USER_AGENT,
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36",
            )
            .header(reqwest::header::REFERER, "https://www.douyin.com/");
        let dy_resp: serde_json::Value = dy_req.send().await?.json().await?;
        let dy_code = dy_resp
            .get("status_code")
            .and_then(|v| v.as_i64())
            .unwrap_or(-1);
        if dy_code == 0 {
            let mut dy_count = 0usize;
            collect_douyin_users(&dy_resp, &mut candidates, &mut dy_count);
            if dy_count == 0 {
                candidates.push(json!({
                    "platform": "douyin",
                    "name": name,
                    "url": "",
                    "hint": "抖音搜索无结果；可直接粘贴抖音主页链接给 monitor_add",
                }));
            }
        } else {
            candidates.push(json!({
                "platform": "douyin",
                "name": name,
                "url": "",
                "hint": "抖音搜索被风控（status_code={dy_code}）。可先在桌面端完成「抖音授权」后重试，或直接粘贴抖音主页链接给 monitor_add",
            }));
        }
    } else {
        candidates.push(json!({
            "platform": "douyin",
            "name": name,
            "url": "",
            "hint": "抖音未授权（缺 douyin_browser_cookies.txt）。先在桌面端「抖音授权」扫码，或直接粘贴抖音主页链接给 monitor_add",
        }));
    }
    Ok(json!({"ok": true, "count": candidates.len(), "candidates": candidates}))
}

/// 从抖音搜索响应中宽松收集用户条目（字段结构因接口版本而异，递归扫描）。
fn collect_douyin_users(
    value: &serde_json::Value,
    out: &mut Vec<serde_json::Value>,
    count: &mut usize,
) {
    if *count >= 10 {
        return;
    }
    match value {
        serde_json::Value::Object(map) => {
            if let Some(sec_uid) = map.get("sec_uid").and_then(|v| v.as_str())
                && let Some(nickname) = map.get("nickname").and_then(|v| v.as_str())
                && !sec_uid.is_empty()
            {
                out.push(serde_json::json!({
                    "platform": "douyin",
                    "name": nickname,
                    "url": format!("https://www.douyin.com/user/{sec_uid}"),
                    "sec_uid": sec_uid,
                    "signature": map.get("signature").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                }));
                *count += 1;
            }
            for v in map.values() {
                collect_douyin_users(v, out, count);
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                collect_douyin_users(v, out, count);
            }
        }
        _ => {}
    }
}

pub fn resolve_with_project_dir(url: &str, project_dir: &Path) -> Box<dyn Platform> {
    if url.contains("bilibili.com") || url.contains("b23.tv") {
        let cookie = crate::config::Config::load(project_dir)
            .ok()
            .map(|c| c.get_fupan("视频监控", "b站cookie"))
            .unwrap_or_default();
        Box::new(BilibiliAdapter::with_cookie_and_project_dir(
            cookie,
            project_dir.to_path_buf(),
        ))
    } else if url.contains("douyin.com") {
        Box::new(DouyinAdapter::new(project_dir.to_path_buf()))
    } else {
        Box::new(RssAdapter::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn douyin_bridge_videos_parse_real_contract() {
        // 契约形态与 fupan/douyin_post_list.py 输出一致（2026-09-25 实测样本精简）
        let v = serde_json::json!({
            "ok": true,
            "videos": [{
                "aweme_id": "7676401627256207988",
                "title": "不是 浙江怎么是这样的啊 #丽水",
                "create_time": 1787301532,
                "duration": 256,
                "url": "https://www.douyin.com/video/7676401627256207988",
                "download_url": "https://v26-web.douyinvod.com/abc",
                "cover": ""
            }]
        });
        let videos = parse_douyin_bridge_videos(&v).unwrap();
        assert_eq!(videos.len(), 1);
        assert_eq!(videos[0].id, "7676401627256207988");
        assert_eq!(videos[0].platform, "douyin");
        assert_eq!(videos[0].duration, 256);
        assert!(!videos[0].published_at.is_empty());
    }

    #[test]
    fn douyin_bridge_failure_contract_is_error_not_empty() {
        // 失败契约必须转成 Err：风控/签名失效时监控源要记失败，
        // 绝不能当成「博主没发新视频」
        let v = serde_json::json!({"ok": false, "error": "抖音视频列表获取失败: 接口返回空视频列表"});
        let err = parse_douyin_bridge_videos(&v).unwrap_err();
        assert!(err.to_string().contains("抖音视频列表获取失败"));
        let v2 = serde_json::json!({"ok": true, "videos": []});
        assert!(parse_douyin_bridge_videos(&v2).is_err());
    }

    #[test]
    fn iesdouyin_user_info_parses_unique_id() {
        // 2026-09-25 实测响应结构（关立新 / 侯翠翠两个在监 sec_uid 均为此形态）
        let v = serde_json::json!({
            "status_code": 0,
            "user_info": {"nickname": "侯翠翠", "unique_id": "xiguahaochi96", "short_id": "0"}
        });        let (uid, nick) = parse_iesdouyin_user_info(&v).unwrap();
        assert_eq!(uid, "xiguahaochi96");
        assert_eq!(nick, "侯翠翠");
    }

    #[test]
    fn iesdouyin_user_info_rejects_error_status() {
        let v = serde_json::json!({"status_code": 8, "user_info": {}});
        assert!(parse_iesdouyin_user_info(&v).is_err());
    }

    #[test]
    fn iesdouyin_user_info_rejects_empty_unique_id() {
        // 用户未设抖音号 / 接口变更 / 用户不存在：必须报错而不是挂上 live.douyin.com/ 空地址
        let v =
            serde_json::json!({"status_code": 0, "user_info": {"nickname": "x", "unique_id": ""}});
        assert!(parse_iesdouyin_user_info(&v).is_err());
        let v2 = serde_json::json!({"status_code": 0, "user_info": {"nickname": "x"}});
        assert!(parse_iesdouyin_user_info(&v2).is_err());
    }

    #[test]
    fn iesdouyin_user_info_full_parses_sec_uid() {
        // 2026-09-26 实测 unique_id 查询响应（Glx6768 → 关立新）
        let v = serde_json::json!({
            "status_code": 0,
            "user_info": {
                "nickname": "关立新（早上7:00直播）",
                "unique_id": "Glx6768",
                "sec_uid": "MS4wLjABAAAAfRq5hAWoSxwYGwDEI-F3glR1hf7mdi87_S2y1bhL7OjOUkc_ETYzRq6QwGZ_qf2m"
            }
        });
        let (sec_uid, unique_id, nickname) = parse_iesdouyin_user_info_full(&v).unwrap();
        assert!(sec_uid.starts_with("MS4wLjAB"));
        assert_eq!(unique_id, "Glx6768");
        assert!(nickname.contains("关立新"));
        // 用户不存在（user_info 为空）：full 解析必须报错，不许挂空主页
        let empty = serde_json::json!({"status_code": 0, "user_info": {}});
        assert!(parse_iesdouyin_user_info_full(&empty).is_err());
    }

    #[test]
    fn looks_like_douyin_id_only_matches_bare_ids() {
        assert!(looks_like_douyin_id("Glx6768"));
        assert!(looks_like_douyin_id("xiguahaochi96"));
        assert!(looks_like_douyin_id("a.b_c-d"));
        // URL / 路径 / 纯数字（B站 ID）/ 空白 / 空串 都不是裸抖音号
        assert!(!looks_like_douyin_id("https://live.douyin.com/Glx6768"));
        assert!(!looks_like_douyin_id("live.douyin.com/Glx6768"));
        assert!(!looks_like_douyin_id("24931109"));
        assert!(!looks_like_douyin_id("张三 李四"));
        assert!(!looks_like_douyin_id(""));
    }

    #[test]
    fn merge_cookie_headers_keeps_login_state_and_appends_fingerprint() {
        // 登录态字段（SESSDATA）必须保留；匿名指纹里缺失的 buvid 才补入。
        let base = "SESSDATA=abc123; bili_jct=def456";
        let extra = "buvid3=B3; buvid4=B4; b_lsid=LSID_1";
        let merged = merge_cookie_headers(base, extra);
        assert!(merged.contains("SESSDATA=abc123"));
        assert!(merged.contains("bili_jct=def456"));
        assert!(merged.contains("buvid3=B3"));
        assert!(merged.contains("buvid4=B4"));
        assert!(merged.contains("b_lsid=LSID_1"));
    }

    #[test]
    fn merge_cookie_headers_never_overrides_base_values() {
        // 同名键冲突时以 base（登录态）为准，绝不被匿名指纹覆盖。
        let base = "buvid3=REAL_LOGIN_BUVID";
        let extra = "buvid3=ANON_BUVID; buvid4=B4";
        let merged = merge_cookie_headers(base, extra);
        assert!(merged.contains("buvid3=REAL_LOGIN_BUVID"));
        assert!(!merged.contains("ANON_BUVID"));
        assert!(merged.contains("buvid4=B4"));
    }

    #[test]
    fn merge_cookie_headers_handles_empty_sides() {
        assert_eq!(merge_cookie_headers("", ""), "");
        assert_eq!(merge_cookie_headers("A=1", ""), "A=1");
        assert_eq!(merge_cookie_headers("", "B=2"), "B=2");
        // 空片段与无等号片段被忽略，不产生残缺项。
        assert_eq!(merge_cookie_headers("A=1; ; jUNK", ""), "A=1");
    }

    #[test]
    fn failure_message_distinguishes_login_required_from_risk_control() {
        // -403 访问权限不足：风控已过、仅差登录，提示必须指向“勾选登录账号”。
        let login_msg = upload_list_failure_message(
            "官方接口业务错误 code=-403 访问权限不足（风控已过但访问权限不足：该空间接口需要登录态，请配置 B站 cookie）",
            "",
            "",
        );
        assert!(login_msg.contains("勾选登录账号"), "实际: {login_msg}");

        // -352 风控：仍归为风控档，提示配置 cookie。
        let risk_msg = upload_list_failure_message("-352 风控校验失败", "", "");
        assert!(risk_msg.contains("412 风控"), "实际: {risk_msg}");
        assert!(!risk_msg.contains("勾选登录账号"));

        // 网络故障优先于一切：即使同时含风控字样也提示检查网络。
        let net_msg =
            upload_list_failure_message("官方接口请求发送失败", "RSSHub 请求失败: 连接超时", "");
        assert!(net_msg.contains("检查网络"), "实际: {net_msg}");
    }

    #[test]
    fn rsshub_url_can_use_configured_local_instance() {
        // 空配置 → 公共实例候选（官方 + 社区镜像）
        assert_eq!(
            rsshub_candidates("", "946974"),
            vec![
                "https://rsshub.app/bilibili/user/video/946974".to_string(),
                "https://rsshub.rss.sb/bilibili/user/video/946974".to_string(),
            ]
        );
        // 配置自建实例 → 只用一个，容忍末尾斜杠
        assert_eq!(
            rsshub_candidates("http://127.0.0.1:1200/", "946974"),
            vec!["http://127.0.0.1:1200/bilibili/user/video/946974".to_string()]
        );
    }

    #[test]
    fn extracts_bvids_ordered_unique() {
        let text = "BV1abcDEFghi BV1abcDEFghi BV1xyz123456";
        assert_eq!(extract_bvids(text), vec!["BV1abcDEFghi", "BV1xyz123456"]);
    }

    #[test]
    fn douyin_sec_uid_extraction() {
        // 完整主页长链可提取加密 sec_uid
        let url = "https://www.douyin.com/user/MS4wLjABAAAAabc123def456";
        assert_eq!(
            DouyinAdapter::sec_uid_from_url(url).as_deref(),
            Some("MS4wLjABAAAAabc123def456")
        );
        // 带查询参数/尾斜杠也能正确截断
        let url2 = "https://www.douyin.com/user/MS4wLjABAAAAxyz?from_tab_name=main";
        assert_eq!(
            DouyinAdapter::sec_uid_from_url(url2).as_deref(),
            Some("MS4wLjABAAAAxyz")
        );
        // 非加密 UID（抖音号/昵称）无法识别，返回 None 提示换完整链接
        assert!(
            DouyinAdapter::sec_uid_from_url("https://www.douyin.com/user/xiguahochi96").is_none()
        );
        // 非 user 路径不误判
        assert!(DouyinAdapter::sec_uid_from_url("https://www.douyin.com/video/123").is_none());
    }

    #[test]
    fn parses_rss_items() {
        let xml = r#"<rss xmlns:media="http://search.yahoo.com/mrss/"><channel>
          <item><title>测试一</title><link>https://x.test/1</link><guid>g1</guid>
          <media:thumbnail url="https://x.test/1.jpg"/></item>
          <item><title>测试二</title><link>https://x.test/2</link><guid>g2</guid></item>
        </channel></rss>"#;
        let items = parse_rss(xml).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].title, "测试一");
        assert_eq!(items[0].cover, "https://x.test/1.jpg");
        assert_eq!(items[1].url, "https://x.test/2");
    }

    #[test]
    fn parses_atom_entries() {
        let xml = r#"<feed xmlns="http://www.w3.org/2005/Atom"
          xmlns:media="http://search.yahoo.com/mrss/">
          <entry><title>Atom 视频</title><id>urn:1</id>
          <link href="https://x.test/1"/>
          <media:thumbnail url="https://x.test/1.jpg"/>
          <published>2026-08-12T10:00:00+08:00</published></entry>
        </feed>"#;
        let items = parse_rss(xml).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Atom 视频");
        assert_eq!(items[0].url, "https://x.test/1");
        assert_eq!(items[0].cover, "https://x.test/1.jpg");
        assert_eq!(items[0].published_at, "2026-08-12");
    }

    #[test]
    fn parses_cdata_titles_and_rss_default_namespace() {
        let xml = r#"<rss version="2.0" xmlns="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
          <channel>
            <item>
              <title><![CDATA[一个 <带标签> 的标题]]></title>
              <link>https://x.test/cdata</link>
            </item>
          </channel>
        </rss>"#;
        let items = parse_rss(xml).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "一个 <带标签> 的标题");
        assert_eq!(items[0].url, "https://x.test/cdata");
    }

    #[test]
    fn parses_feed_with_doctype_declaration() {
        // 回归：roxmltree 默认禁止 DTD，带 `<!DOCTYPE>` 的订阅源（部分 RSSHub
        // 实例/前置代理会加）之前会报「XML with DTD detected」。开启 allow_dtd 后
        // 必须能正常解析。
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE feed [
  <!ENTITY x "y">
]>
<feed xmlns="http://www.w3.org/2005/Atom">
  <entry><title>带 DTD 的 Atom</title><id>urn:dt-1</id>
  <link href="https://x.test/dt"/></entry>
</feed>"#;
        let items = parse_rss(xml).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "带 DTD 的 Atom");
        assert_eq!(items[0].url, "https://x.test/dt");
    }

    #[test]
    fn rss_html_error_page_is_reported_clearly() {
        // RSSHub 被限流/代理拦截时常返回 `<!DOCTYPE html>` 网页。开启 allow_dtd 后
        // 这种网页会被解析器判定为非法 XML，应给出可读错误而非把 DOCTYPE 当致命错。
        let html = r#"<!DOCTYPE html><html><head><meta charset="utf-8"><title>403</title></head>
<body>RSSHub 访问受限</body></html>"#;
        let err = parse_rss(html).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("网页") || msg.contains("订阅源"),
            "应给出可读错误，实际: {msg}"
        );
    }

    #[test]
    fn malformed_rss_is_an_error_not_empty_baseline() {
        assert!(parse_rss("<rss><item><title>未闭合").is_err());
    }

    #[test]
    fn rss_link_is_preferred_over_empty_atom_link() {
        let xml = r#"<rss xmlns:atom="http://www.w3.org/2005/Atom"><channel>
          <item>
            <title>RSS 标题</title>
            <atom:link href=""/>
            <link>https://x.test/rss</link>
          </item>
        </channel></rss>"#;
        let items = parse_rss(xml).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].url, "https://x.test/rss");
    }

    #[test]
    fn normalizes_rss_and_atom_dates() {
        assert_eq!(
            normalize_rss_date(Some("Wed, 12 Aug 2026 10:00:00 +0800".to_string())),
            "2026-08-12"
        );
        assert_eq!(
            normalize_rss_date(Some("2026-08-12T10:00:00+08:00".to_string())),
            "2026-08-12"
        );
        assert_eq!(
            normalize_rss_date(Some("2026-08-12T10:00:00".to_string())),
            "2026-08-12"
        );
        assert_eq!(normalize_rss_date(Some("not a date".to_string())), "");
        assert_eq!(normalize_rss_date(None), "");
    }

    #[test]
    fn mid_parsed_from_space_url() {
        assert_eq!(
            BilibiliAdapter::mid_from_url("https://space.bilibili.com/946974/video"),
            Some("946974".to_string())
        );
        // m.bilibili.com/space/数字 也要认。
        assert_eq!(
            BilibiliAdapter::mid_from_url("https://m.bilibili.com/space/1458143131"),
            Some("1458143131".to_string())
        );
        // 纯数字直接当 UID。
        assert_eq!(
            BilibiliAdapter::mid_from_url("1458143131"),
            Some("1458143131".to_string())
        );
        assert_eq!(BilibiliAdapter::mid_from_url("https://example.com/x"), None);
    }

    #[tokio::test]
    async fn normalize_blogger_input_handles_uid_and_passthrough() {
        // 纯数字 → 空间地址；非短链/非视频输入原样返回（不触发网络）。
        assert_eq!(
            normalize_blogger_input("1458143131").await,
            "https://space.bilibili.com/1458143131"
        );
        assert_eq!(
            normalize_blogger_input("  https://rsshub.app/bilibili/user/video/1  ").await,
            "https://rsshub.app/bilibili/user/video/1"
        );
        assert_eq!(
            normalize_blogger_input("https://space.bilibili.com/946974").await,
            "https://space.bilibili.com/946974"
        );
    }

    #[tokio::test]
    async fn normalize_blogger_input_strips_tracking_query_on_space_url() {
        // B站主页地址常带 ?spm_id_from= 这类跟踪参数。若不归一，它会成为
        // 视频库的 source_url 主键，同一个人就出现两条 key，投稿列表和
        // 已下载标记各算各的——这正是「官方 46 条、界面只列 45 条」的成因之一。
        assert_eq!(
            normalize_blogger_input("https://space.bilibili.com/601214690?spm_id_from=333.337.0.0")
                .await,
            "https://space.bilibili.com/601214690"
        );
        assert_eq!(
            normalize_blogger_input("https://space.bilibili.com/601214690/video#/all").await,
            "https://space.bilibili.com/601214690"
        );
        assert_eq!(
            normalize_blogger_input("https://m.bilibili.com/space/601214690").await,
            "https://space.bilibili.com/601214690"
        );
    }

    #[test]
    fn unix_ts_converts_date() {
        let east8 = chrono::FixedOffset::east_opt(8 * 3600).unwrap();
        assert_eq!(unix_to_date_in_offset(1786464000, east8), "2026-08-12");
        // 同一时刻在 UTC 是前一天，证明函数真的按传入时区而不是宿主机默认。
        let utc = chrono::FixedOffset::east_opt(0).unwrap();
        assert_eq!(unix_to_date_in_offset(1786464000, utc), "2026-08-11");
    }

    #[test]
    fn wbi_query_matches_web_client_parameter_set() {
        // 参数表必须与网页端 `x/space/wbi/arc/search` 一致：B站把整串参与
        // w_rid 签名，缺一个都会被判成 code=-352「风控校验失败」。
        // 特别是 dm_img_* 这四个风险控制字段，早期实现漏了它们，官方接口
        // 永远打不通，「列出主播历史」只能退回半截列表（官方 46 → 45）。
        let query = build_wbi_query(
            "1234567890abcdefghijklmnopqrstuvwxyz",
            "ABCDEFGHIJKLMNOPQRSTUVWXYZ01",
            "946974",
            40,
            1,
            1786550400,
        );
        for key in [
            "dm_cover_img_str=",
            "dm_img_inter=",
            "dm_img_list=",
            "dm_img_str=",
            "index=0",
            "keyword=",
            "mid=946974",
            "order=pubdate",
            "order_avoided=true",
            "platform=web",
            "pn=1",
            "ps=40",
            "special_type=",
            "tid=0",
            "web_location=333.1387",
            "wts=1786550400",
            "x-bili-device-req-json=",
            "x-bili-locale-json=",
        ] {
            assert!(query.contains(key), "缺少 {key}：{query}");
        }
        assert!(query.contains("&w_rid="), "{query}");
        // JSON 值必须整体 encodeURIComponent 后再签名，否则请求本身非法。
        assert!(query.contains("%7B%22platform%22"), "JSON 未编码：{query}");
        assert!(!query.contains('{'), "不应残留未编码的大括号：{query}");
        // 参数按 key 升序拼接（末尾的 w_rid 是签名结果，不参与排序）。
        let keys: Vec<&str> = query
            .split('&')
            .filter_map(|kv| kv.split('=').next())
            .filter(|k| *k != "w_rid")
            .collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted, "参数必须按 key 升序");
        // 固定输入 → 固定签名，防止编码/拼接规则被改坏。
        assert_eq!(
            query,
            "dm_cover_img_str=QU5HTEUgKEFwcGxlLCBBTkdMRSBNZXRhbCBSZW5kZXJlcjogQXBwbGUgTTEsIFVuc3BlY2lmaWVkIFZlcnNpb24pR29vZ2xlIEluYy4gKEFwcGxl&dm_img_inter=%7B%22ds%22%3A%5B%5D%2C%22wh%22%3A%5B0%2C0%2C0%5D%2C%22of%22%3A%5B0%2C0%2C0%5D%7D&dm_img_list=%5B%7B%22x%22%3A0%2C%22y%22%3A0%2C%22z%22%3A0%2C%22timestamp%22%3A1%2C%22k%22%3A1%2C%22type%22%3A1%7D%5D&dm_img_str=V2ViR0wgMS4wIChPcGVuR0wgRVMgMi4wIENocm9taXVtKQ&index=0&keyword=&mid=946974&order=pubdate&order_avoided=true&platform=web&pn=1&ps=40&special_type=&tid=0&web_location=333.1387&wts=1786550400&x-bili-device-req-json=%7B%22platform%22%3A%22web%22%2C%22device%22%3A%22pc%22%2C%22spmid%22%3A%22333.1387%22%2C%22mobi_app%22%3A%22web_cn%22%7D&x-bili-locale-json=%7B%22c_locale%22%3A%7B%22language%22%3A%22zh%22%2C%22script%22%3A%22Hans%22%7D%2C%22always_translate%22%3Afalse%7D&w_rid=4c4e9ed6108a92589e6e26d9810558c4"
        );
    }

    #[test]
    fn encode_uri_component_matches_javascript() {
        // 与 JS encodeURIComponent 的保留字符集一致（含 `!~*'()` 不编码）。
        assert_eq!(encode_uri_component("a-b_c.d!e~f*g'h(i)"), "a-b_c.d!e~f*g'h(i)");
        assert_eq!(encode_uri_component("{\"a\":1}"), "%7B%22a%22%3A1%7D");
        assert_eq!(encode_uri_component("中"), "%E4%B8%AD");
        assert_eq!(encode_uri_component("a b"), "a%20b");
        assert_eq!(encode_uri_component("/+="), "%2F%2B%3D");
    }

    #[test]
    fn cookie_header_has_login_only_counts_account_fields() {
        // 登录态要认；匿名设备指纹不能当登录态，否则会误判成「已授权」，
        // 让 RSSHub 有结果时还去多打一次官方接口。
        assert!(cookie_header_has_login("buvid3=x; SESSDATA=abc; b_lsid=y"));
        assert!(cookie_header_has_login("DedeUserID=123"));
        assert!(!cookie_header_has_login("buvid3=x; buvid4=y; b_lsid=z"));
        assert!(!cookie_header_has_login("SESSDATA="));
        assert!(!cookie_header_has_login("SESSDATA=   "));
        assert!(!cookie_header_has_login(""));
    }

    #[test]
    fn wbi_query_accepts_page_number() {
        let query = build_wbi_query(
            "1234567890abcdefghijklmnopqrstuvwxyz",
            "ABCDEFGHIJKLMNOPQRSTUVWXYZ01",
            "946974",
            30,
            2,
            1786550400,
        );
        assert!(query.contains("pn=2"), "{query}");
        assert!(query.contains("ps=30"), "{query}");
    }

    #[test]
    fn parses_ytdlp_flat_playlist_ids_and_total() {
        // 正常：每行都带同一个 playlist_count（实测这台机器上就是 46）。
        let text = "BV1abcdefghi\t46\nBV1abcdefghj\t46\nBV1abcdefghk\t46\n";
        let (ids, total) = parse_ytdlp_flat_playlist(text, 0);
        assert_eq!(ids, vec!["BV1abcdefghi", "BV1abcdefghj", "BV1abcdefghk"]);
        assert_eq!(total, Some(46));

        // count > 0（只取最近 N 条）时按 count 截断，但总数仍是完整总数。
        let (ids, total) = parse_ytdlp_flat_playlist(text, 2);
        assert_eq!(ids.len(), 2);
        assert_eq!(total, Some(46));

        // playlist_count 不可用（yt-dlp 打印 NA）→ 总数未知。
        // 绝不能当成 0：那会比对出「共 0 条、列出 2 条」这种荒谬结论。
        let (ids, total) = parse_ytdlp_flat_playlist("BV1abcdefghi\tNA\nBV1abcdefghj\tNA\n", 0);
        assert_eq!(ids.len(), 2);
        assert_eq!(total, None);

        // 老版本 yt-dlp 只打印裸 id 时也要能解析，总数保持未知。
        let (ids, total) = parse_ytdlp_flat_playlist("BV1abcdefghi\nBV1abcdefghj\n", 0);
        assert_eq!(ids.len(), 2);
        assert_eq!(total, None);

        // 空输出：既无 id 也无总数，调用方据此换下一组兜底。
        let (ids, total) = parse_ytdlp_flat_playlist("", 0);
        assert!(ids.is_empty());
        assert_eq!(total, None);
    }

    #[test]
    fn should_fetch_more_respects_count_total_and_page_guard() {
        // 指定数量：取满即停。
        assert!(!should_fetch_more(30, 30, Some(80), 1));
        assert!(should_fetch_more(30, 29, Some(80), 1));
        // count=0 表示全部：只要还有更多就继续，直到总投稿数读完。
        assert!(should_fetch_more(0, 30, Some(80), 1));
        assert!(!should_fetch_more(0, 80, Some(80), 2));
        // 总投稿数未知：最多翻到 50 页护栏。
        assert!(should_fetch_more(0, 100, None, 9));
        assert!(!should_fetch_more(0, 100, None, 10));
    }

    #[test]
    fn official_upload_response_requires_code_zero_and_bvids() {
        let ok = serde_json::json!({
            "code": 0,
            "data": {"list": {"vlist": [
                {"bvid": "BV1xx411c7mD", "title": "标题甲", "pic": "http://x.test/1.jpg", "pubdate": 1786464000, "length": 61},
                {"bvid": "BV1yy411c7mD", "title": "标题乙"},
                {"title": "无 bvid 的行应被忽略"}
            ]}}
        });
        let east8 = chrono::FixedOffset::east_opt(8 * 3600).unwrap();
        let parsed = parse_official_upload_response_in_offset(&ok, 10, east8);
        assert_eq!(parsed.len(), 2, "应忽略无 bvid 的行");
        assert_eq!(parsed[0].id, "BV1xx411c7mD");
        assert_eq!(parsed[0].title, "标题甲");
        assert_eq!(parsed[0].cover, "https://x.test/1.jpg");
        assert_eq!(parsed[0].published_at, "2026-08-12");
        assert_eq!(parsed[0].duration, 61);
        assert_eq!(parsed[1].id, "BV1yy411c7mD");
        assert_eq!(parsed[1].title, "标题乙");
        for bad in [
            serde_json::json!({"code": -412}),
            serde_json::json!({"code": 0, "data": {}}),
            serde_json::json!({"code": 0, "data": {"list": {"vlist": []}}}),
        ] {
            assert!(
                parse_official_upload_response(&bad, 10).is_empty(),
                "无有效 vlist 时应返回空，而不是伪装成成功"
            );
        }
    }

    #[test]
    fn upload_failure_message_distinguishes_network_from_risk_control() {
        // 网络/DNS 故障：必须提示查网络，不能误导成“请配置 cookie”。
        let net = upload_list_failure_message(
            "官方接口签名失败: reqwest error resolving api.bilibili.com: nodename nor servname provided",
            "RSSHub 请求失败: connection timed out",
            "yt-dlp 兜底超时（30 秒）",
        );
        assert!(
            net.contains("解析失败") || net.contains("请求失败"),
            "{net}"
        );
        assert!(net.contains("检查网络"), "{net}");
        assert!(!net.contains("请配置 B站 cookie"), "{net}");

        // 风控：提示去「B站授权」登录（比笼统的“配置 cookie”更可操作）。
        let risk = upload_list_failure_message(
            "官方 wbi 接口未返回 BV（可能是 412 风控或接口变更）",
            "RSSHub 返回 HTTP 403",
            "yt-dlp 未返回 BV 号",
        );
        assert!(risk.contains("412"), "{risk}");
        assert!(risk.contains("B站授权"), "{risk}");

        // 完全无原因：给出通用提示，不崩溃。
        let empty = upload_list_failure_message("", "", "");
        assert!(empty.contains("三个兜底均未返回数据"), "{empty}");
    }

    #[test]
    fn authoritative_hint_translates_official_failure_codes() {
        // 成功：官方接口没失败，不该给用户任何提示。
        let ok = BilibiliAdapter::default();
        assert_eq!(ok.authoritative_hint(), None);

        // -403 是「官方 46 条、界面 45 条」在这台机器上的真实原因：
        // 风控已过、只差登录态 → 提示必须明确指向「B站授权」这个动作，
        // 否则用户看到「少一条」也无从下手。
        let login = BilibiliAdapter::default();
        login.note_official_code(-403);
        let hint = login.authoritative_hint().expect("-403 必须给出提示");
        assert!(hint.contains("B站授权"), "{hint}");

        // 已经带上登录态却仍被 -403：绝不能再叫用户「去登录一次」——
        // 「我明明登录过」正是这样被反复误导出来的。
        let logged_in = BilibiliAdapter::default();
        logged_in.note_official_code(-403);
        logged_in
            .last_official_had_login
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let logged_hint = logged_in.authoritative_hint().unwrap();
        assert_ne!(logged_hint, hint);
        assert!(!logged_hint.contains("请到设置页"), "{logged_hint}");
        assert!(logged_hint.contains("限流"), "{logged_hint}");

        // -352 / -799 / HTTP 层 / 网络层各自给出不同的话，不能被混成一句，
        // 否则用户按错的建议去操作（例如明明是限流却去登录）。
        let risk = BilibiliAdapter::default();
        risk.note_official_code(-352);
        let risk_hint = risk.authoritative_hint().unwrap();
        assert!(risk_hint.contains("风控"), "{risk_hint}");
        assert_ne!(risk_hint, hint);

        let limited = BilibiliAdapter::default();
        limited.note_official_code(-799);
        assert!(limited.authoritative_hint().unwrap().contains("频繁"));

        let http = BilibiliAdapter::default();
        http.note_official_code(-1000);
        assert!(http.authoritative_hint().unwrap().contains("412"));

        let net = BilibiliAdapter::default();
        net.note_official_code(-1001);
        assert!(net.authoritative_hint().unwrap().contains("网络"));

        // 未收录的业务码也要带上码本身和可行动作，不能返回空串。
        let other = BilibiliAdapter::default();
        other.note_official_code(-509);
        let other_hint = other.authoritative_hint().unwrap();
        assert!(other_hint.contains("-509"), "{other_hint}");

        // 成功一次之后旧失败码必须被清掉，否则界面上会一直挂着过期提示。
        other.note_official_code(0);
        assert_eq!(other.authoritative_hint(), None);
    }

    #[test]
    fn venv_ytdlp_prefers_current_platform_and_falls_back() {
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-ytdlp-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let unix = dir.join(".venv").join("bin").join("yt-dlp");
        let windows = dir.join(".venv").join("Scripts").join("yt-dlp.exe");
        let adapter = BilibiliAdapter::with_cookie_and_project_dir("", dir.clone());

        std::fs::create_dir_all(unix.parent().unwrap()).unwrap();
        std::fs::create_dir_all(windows.parent().unwrap()).unwrap();

        // 只放本平台文件时，应选中它而不是另一平台路径。
        if cfg!(windows) {
            std::fs::write(&windows, "").unwrap();
        } else {
            std::fs::write(&unix, "").unwrap();
        }
        assert_eq!(
            adapter.project_venv_ytdlp().unwrap(),
            if cfg!(windows) {
                windows.clone()
            } else {
                unix.clone()
            }
        );

        // 两个都在时仍优先本平台路径。
        if cfg!(windows) {
            std::fs::write(&unix, "").unwrap();
        } else {
            std::fs::write(&windows, "").unwrap();
        }
        assert_eq!(
            adapter.project_venv_ytdlp().unwrap(),
            if cfg!(windows) { windows } else { unix }
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ytdlp_path_fallback_uses_platform_suffix() {
        let fallback = ytdlp_path_fallback();
        assert_eq!(
            fallback.to_string_lossy(),
            if cfg!(windows) {
                "yt-dlp.exe"
            } else {
                "yt-dlp"
            }
        );
    }
}
