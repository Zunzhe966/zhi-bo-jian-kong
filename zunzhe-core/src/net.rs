//! 出站网络分流：学代理软件的"规则分流"思想。
//!
//! 监控目标既有国内站点（B站/抖音/快手/国内通知服务），也可能有国外站点
//! （YouTube 等）。国内直连最快最稳；国外站点必须走用户本机代理。
//! 一刀切"全走代理"会在代理软件退出后拖垮全部监控（已实测：代理端口失效
//! 导致 B站源连续失败 42 次）；一刀切"全直连"则让国外源永远不可达。
//!
//! 因此这里按域名分流：
//! - `直连域名` 清单内的域名**永远直连**；
//! - 其余域名在用户配置了 `出站代理` 时走该代理，未配置时直连。
//!
//! 配置位于主 config.ini 的 `[网络]` 节：
//! - `出站代理 = http://127.0.0.1:7890`（留空 = 全部直连）
//! - `直连域名 = bilibili.com,hdslb.com,...`（逗号分隔，追加到内置清单）
//!
//! daemon 进程启动时已剥离代理环境变量（service.rs/zunzhe-daemon.sh），
//! 所以"未配置出站代理"就是干净的直连状态，不会意外继承环境里的代理。

use crate::config::Config;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

/// 内置直连域名后缀：国内站点与国内通知服务。
/// 用户可通过 config.ini `[网络] 直连域名` 追加。
const DEFAULT_DIRECT_DOMAINS: &[&str] = &[
    // 本机回环：本地 RSSHub 等自建服务绝不能走出站代理
    "localhost",
    "127.0.0.1",
    "::1",
    "bilibili.com",
    "hdslb.com",
    "biliapi.net",
    "biliapi.com",
    "b23.tv",
    "bilibili.tv",
    "douyin.com",
    "douyinpic.com",
    "kuaishou.com",
    "ftqq.com",
    "pushplus.plus",
];

/// 出站策略：代理地址 + 直连域名清单。
#[derive(Debug, Clone)]
pub struct OutboundPolicy {
    /// 用户配置的出站代理（如 `http://127.0.0.1:7890`）；None = 未配置。
    proxy: Option<String>,
    /// 直连域名后缀清单（小写）。
    direct_domains: Vec<String>,
}

impl OutboundPolicy {
    /// 从项目配置读取策略；读取失败时退回"全直连"默认值。
    pub fn load(project_dir: &Path) -> Self {
        let cfg = Config::load(project_dir).ok();
        let proxy = cfg
            .as_ref()
            .map(|c| c.get_main("网络", "出站代理"))
            .unwrap_or_default();
        let proxy = {
            let p = proxy.trim().to_string();
            (!p.is_empty()).then_some(p)
        };
        let mut direct_domains: Vec<String> = DEFAULT_DIRECT_DOMAINS
            .iter()
            .map(|s| s.to_string())
            .collect();
        if let Some(cfg) = cfg.as_ref() {
            for d in cfg.get_main("网络", "直连域名").split(',') {
                let d = d.trim().to_ascii_lowercase();
                if !d.is_empty() {
                    direct_domains.push(d);
                }
            }
        }
        Self {
            proxy,
            direct_domains,
        }
    }

    /// 判断主机是否应直连（域名后缀匹配）。
    pub fn is_direct(&self, host: &str) -> bool {
        let host = host.to_ascii_lowercase();
        self.direct_domains
            .iter()
            .any(|d| host == *d || host.ends_with(&format!(".{d}")))
    }

    /// 测试专用构造器：跨模块单测需要构造指定代理/直连清单的策略。
    #[cfg(test)]
    pub(crate) fn for_test(proxy: Option<&str>, direct_domains: Vec<String>) -> Self {
        Self {
            proxy: proxy.map(str::to_string),
            direct_domains,
        }
    }

    /// 构建带分流规则的 reqwest ClientBuilder。
    ///
    /// 未配置出站代理时返回干净的 builder（daemon 环境无代理变量，天然直连）；
    /// 配置了代理时用 `Proxy::custom` 按目标域名逐请求分流。
    pub fn reqwest_builder(&self) -> reqwest::ClientBuilder {
        let builder = reqwest::Client::builder();
        match &self.proxy {
            None => builder,
            Some(addr) => match addr.parse::<reqwest::Url>() {
                Ok(proxy_url) => {
                    let directs = self.direct_domains.clone();
                    builder.proxy(reqwest::Proxy::custom(move |url| {
                        let host = url.host_str().unwrap_or("");
                        let direct = directs.iter().any(|d| {
                            let h = host.to_ascii_lowercase();
                            h == *d || h.ends_with(&format!(".{d}"))
                        });
                        if direct {
                            None // 直连域名不走代理
                        } else {
                            Some(proxy_url.clone())
                        }
                    }))
                }
                Err(_) => {
                    tracing::warn!("出站代理配置无效: {addr}，忽略代理全直连");
                    builder
                }
            },
        }
    }

    /// yt-dlp 的代理参数：按目标 URL 分流。
    /// - 未配置代理：空（daemon 环境本身无代理变量）；
    /// - 直连域名：显式 `--proxy ""` 强制禁代理（防外部环境残留）；
    /// - 其他域名且配置了代理：`--proxy <addr>`。
    pub fn ytdlp_proxy_args(&self, url: &str) -> Vec<String> {
        let Some(proxy) = &self.proxy else {
            return Vec::new();
        };
        let host = host_of(url);
        let direct = host.as_deref().map(|h| self.is_direct(h)).unwrap_or(true); // 解析不出域名的保守直连
        if direct {
            vec!["--proxy".to_string(), String::new()]
        } else {
            vec!["--proxy".to_string(), proxy.clone()]
        }
    }

    /// 该 URL 是否必须经过出站代理才能访问（配置了代理且域名不在直连清单）。
    /// 未配置代理时恒为 false（全直连状态，不存在“代理没开”的问题）。
    pub fn needs_proxy(&self, url: &str) -> bool {
        if self.proxy.is_none() {
            return false;
        }
        match host_of(url) {
            Some(host) => !self.is_direct(&host),
            None => false, // 解析不出域名的保守视为直连
        }
    }

    /// 探测本机出站代理端口是否可连（1 秒超时 TCP 连接）。
    ///
    /// 代理软件没开时端口是 Connection refused——这与“节点不通”不同：
    /// 前者应该跳过依赖代理的源并明确提示用户开代理，而不是把源当成
    /// 故障累计失败冷却。返回 None 表示未配置代理（无需探测）。
    pub async fn proxy_alive(&self) -> Option<bool> {
        let addr = self.proxy.as_deref()?;
        let url = reqwest::Url::parse(addr).ok()?;
        let host = url.host_str()?.to_string();
        let port = url.port_or_known_default()?;
        // 拒绝连接是瞬时的；但对黑洞地址 connect 可能挂很久，必须加 1 秒护栏。
        let attempt = tokio::time::timeout(
            Duration::from_secs(1),
            tokio::net::TcpStream::connect((host.as_str(), port)),
        )
        .await;
        Some(match attempt {
            Ok(Ok(_)) => true,
            // 连接失败或探测超时都按“代理未运行”处理。
            _ => false,
        })
    }
}

fn host_of(url: &str) -> Option<String> {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|s| s.to_string()))
}

static POLICY: OnceLock<OutboundPolicy> = OnceLock::new();

/// 进程启动时初始化全局策略（cli.rs 入口调用一次）。
/// 之后配置变更需要重启 daemon 才生效（网络策略低频变更，可接受）。
pub fn init_with_config(project_dir: &Path) {
    let _ = POLICY.set(OutboundPolicy::load(project_dir));
}

/// 取全局策略；未初始化（如部分单元测试路径）时退回全直连默认值。
pub fn policy() -> &'static OutboundPolicy {
    POLICY.get_or_init(|| OutboundPolicy {
        proxy: None,
        direct_domains: DEFAULT_DIRECT_DOMAINS
            .iter()
            .map(|s| s.to_string())
            .collect(),
    })
}

/// 统一的 reqwest ClientBuilder 出口：替换散落各处的
/// `reqwest::Client::builder()`，保证所有出站请求都经过同一套分流规则。
pub fn client_builder() -> reqwest::ClientBuilder {
    policy().reqwest_builder()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_is_direct_only() {
        let p = OutboundPolicy {
            proxy: None,
            direct_domains: DEFAULT_DIRECT_DOMAINS
                .iter()
                .map(|s| s.to_string())
                .collect(),
        };
        assert!(p.is_direct("api.bilibili.com"));
        assert!(
            p.ytdlp_proxy_args("https://www.bilibili.com/video/BV1")
                .is_empty()
        );
    }

    #[test]
    fn proxied_policy_splits_by_domain() {
        let p = OutboundPolicy {
            proxy: Some("http://127.0.0.1:7890".to_string()),
            direct_domains: vec!["bilibili.com".to_string()],
        };
        // 国内站点显式禁代理
        assert_eq!(
            p.ytdlp_proxy_args("https://www.bilibili.com/video/BV1"),
            vec!["--proxy".to_string(), String::new()]
        );
        // 国外站点走代理
        assert_eq!(
            p.ytdlp_proxy_args("https://www.youtube.com/watch?v=x"),
            vec!["--proxy".to_string(), "http://127.0.0.1:7890".to_string()]
        );
        // 解析失败的 URL 保守直连（显式禁代理）
        assert_eq!(
            p.ytdlp_proxy_args("not a url"),
            vec!["--proxy".to_string(), String::new()]
        );
        assert!(p.is_direct("API.Bilibili.COM")); // 大小写不敏感
    }

    #[test]
    fn loopback_never_goes_through_proxy() {
        // 配了出站代理时，本地 RSSHub（127.0.0.1:1200）也必须直连
        let p = OutboundPolicy {
            proxy: Some("http://127.0.0.1:7891".to_string()),
            direct_domains: DEFAULT_DIRECT_DOMAINS
                .iter()
                .map(|s| s.to_string())
                .collect(),
        };
        assert!(p.is_direct("127.0.0.1"));
        assert!(p.is_direct("localhost"));
        assert_eq!(
            p.ytdlp_proxy_args("http://127.0.0.1:1200/douyin/user/MS4wLjABAAAAxx"),
            vec!["--proxy".to_string(), String::new()]
        );
    }
}
