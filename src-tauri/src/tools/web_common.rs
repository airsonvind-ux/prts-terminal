//! tools::web_common —— 网络域（M5）共享守卫与纯函数
//!
//! web_search / web_fetch 共用的离线可测逻辑，全部为纯函数：
//! - URL 解析与协议守卫（仅 http/https）
//! - SSRF 防护：IP 字面量分类 + 域名解析复查，并把解析结果固定到连接（解析失败按私有保守拒）
//! - 字符安全截断（UTF-8 char 边界）
//! - Markdown 链接 absolutize 后处理（相对链接 join base；非 http/https
//!   协议（javascript:/data:/mailto: 等）剥成纯文本——readabilityrs 的
//!   markdown 输出不做链接重写与协议过滤（crate 安全说明明示
//!   sanitize_content 不作用于 markdown_content），本函数补齐该缺口）
//!
//! 测试注入点：host 校验以 resolver 闭包形态注入（生产 = 系统 DNS 全量
//! 解析；测试 = 罐头数据），实现"IP 字面量全类表驱动 + 域名用例 mock"的
//! 离线确定性测试，不触碰真实网络。

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs};
use std::sync::OnceLock;

use regex::Regex;

// ═══════════════════════════════════════════════════════
// URL 解析与协议守卫
// ═══════════════════════════════════════════════════════

/// URL 守卫拒绝原因（消息文案由消费工具组装）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UrlReject {
    /// 不是可解析的 URL（无 scheme 等）
    Invalid,
    /// scheme 非 http/https（值 = 实际 scheme，空 = 无 scheme）
    UnsupportedScheme(String),
}

/// 解析 URL 并守卫协议：仅 http/https 放行（返回解析后的 Url）。
/// `reqwest::Url` 即 `url::Url` 的再导出（不为此引入新依赖）。
pub fn parse_http_url(raw: &str) -> Result<reqwest::Url, UrlReject> {
    let url = reqwest::Url::parse(raw.trim()).map_err(|_| UrlReject::Invalid)?;
    match url.scheme() {
        "http" | "https" => Ok(url),
        other => Err(UrlReject::UnsupportedScheme(other.to_string())),
    }
}

// ═══════════════════════════════════════════════════════
// SSRF 防护：IP 字面量分类
// ═══════════════════════════════════════════════════════

/// 判断 IP 是否属于"不可访问"的私有/本机/链路本地/保留范围。
///
/// 覆盖（对齐 M5 定稿清单 + 防御补强）：
/// - IPv4 回环 127.0.0.0/8、未指定 0.0.0.0/8、RFC1918（10/8、172.16/12、
///   192.168/16）、链路本地 169.254/16、CGNAT 100.64/10、基准测试
///   198.18/15、文档/保留段（192.0.0/24、192.0.2/24、198.51.100/24、
///   203.0.113/24、192.88.99/24）、组播/保留 224.0.0.0/3
/// - IPv6 回环 ::1、未指定 ::、链路本地 fe80::/10、ULA fc00::/7、
///   组播 ff00::/8、文档段 2001:db8::/32
/// - IPv4 映射 IPv6（::ffff:a.b.c.d）按其内嵌 v4 地址复查（否则
///   127.0.0.1 经 ::ffff:127.0.0.1 形态可绕过回环判定）
/// - 其余地址一律放行（公网）
pub fn is_private_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_private_ipv4(*v4),
        IpAddr::V6(v6) => {
            // 先处理 v4 映射形态（含 IPv4 兼容的压缩书写）
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_private_ipv4(v4);
            }
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_unicast_link_local()
                || v6.is_unique_local()
                || v6.is_multicast()
                || (v6.segments()[0] == 0x2001 && v6.segments()[1] == 0x0db8)
        }
    }
}

fn is_private_ipv4(v4: Ipv4Addr) -> bool {
    let o = v4.octets();
    match o[0] {
        127 => true,                       // 回环 127.0.0.0/8
        0 => true,                         // 未指定 0.0.0.0/8（含 0.0.0.0）
        10 => true,                        // RFC1918 10.0.0.0/8
        100 => (64..=127).contains(&o[1]), // CGNAT 100.64.0.0/10
        172 => (16..=31).contains(&o[1]),  // RFC1918 172.16.0.0/12
        192 => match o[1] {
            168 => true,                 // RFC1918 192.168.0.0/16
            0 => o[2] == 0 || o[2] == 2, // 保留 192.0.0.0/24 / 文档 192.0.2.0/24
            88 => o[2] == 99,            // 6to4 中继 192.88.99.0/24
            _ => false,
        },
        198 => (18..=19).contains(&o[1]) || (o[1] == 51 && o[2] == 100), // 基准测试 / TEST-NET-2
        169 => o[1] == 254,                                              // 链路本地 169.254.0.0/16
        203 => o[1] == 0 && o[2] == 113, // TEST-NET-3 203.0.113.0/24
        224..=239 => true,               // 组播 224.0.0.0/4
        240..=255 => true,               // 保留 240.0.0.0/4（含广播）
        _ => false,
    }
}

// ═══════════════════════════════════════════════════════
// SSRF 防护：域名解析复查
// ═══════════════════════════════════════════════════════

/// 解析主机名的全部 IP（生产默认 resolver；port 仅作 getaddrinfo 占位）。
/// 使用系统解析器返回 A + AAAA 全量结果。
pub fn resolve_host_all(host: &str) -> io::Result<Vec<IpAddr>> {
    let addrs: Vec<SocketAddr> = (host, 0).to_socket_addrs()?.collect();
    Ok(addrs.into_iter().map(|sa| sa.ip()).collect())
}

/// host 解析拒绝原因（仅用于内部判定与测试）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostReject {
    Private,
    ResolveFailed,
    Empty,
}

/// 解析主机并返回一个可连接的 IP；域名会做全量私有段复查。
///
/// 返回的 IP 供 `web_fetch` 固定到客户端（防 DNS 重绑定）：调用方必须用
/// 同一结果建立连接，不能在连接阶段再次解析。
pub fn resolve_public_ip(
    host: &str,
    allowlist: &[String],
    resolve: impl Fn(&str) -> io::Result<Vec<IpAddr>>,
) -> Result<IpAddr, HostReject> {
    let allowed = allowlist.iter().any(|h| h == host);
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = bare.parse::<IpAddr>() {
        if allowed || !is_private_ip(&ip) {
            return Ok(ip);
        }
        return Err(HostReject::Private);
    }

    let ips = resolve(host).map_err(|_| HostReject::ResolveFailed)?;
    if ips.is_empty() {
        return Err(HostReject::Empty);
    }
    if !allowed && ips.iter().any(is_private_ip) {
        return Err(HostReject::Private);
    }
    // 优先 IPv4，避免仅有 AAAA 时在无 IPv6 出口的环境直接失败。
    ips.iter()
        .find(|ip| ip.is_ipv4())
        .or_else(|| ips.first())
        .copied()
        .ok_or(HostReject::Empty)
}

/// 测试用布尔包装：等价于 [`resolve_public_ip`] 是否失败。
#[cfg(test)]
pub fn host_is_blocked(
    host: &str,
    allowlist: &[String],
    resolve: impl Fn(&str) -> io::Result<Vec<IpAddr>>,
) -> bool {
    resolve_public_ip(host, allowlist, resolve).is_err()
}

// ═══════════════════════════════════════════════════════
// 字符安全截断
// ═══════════════════════════════════════════════════════

/// 截断到字符上限（UTF-8 char 边界安全，中文按字计）。
pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

// ═══════════════════════════════════════════════════════
// Markdown 链接 absolutize（清洗后处理）
// ═══════════════════════════════════════════════════════

fn link_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // 链接/图片语法：`[文本](目标)` / `![alt](目标)`；目标允许一层
        // 平衡括号（维基类 URL 含括号），不含空白与标题后缀
        Regex::new(
            r"(?P<pre>!?\[[^\]\n]*\])\((?P<url>(?:[^()\s\n]|\([^()\n]*\))*)(?:\s+[^)\n]*)?\)",
        )
        .expect("链接正则静态合法")
    })
}

/// 把 markdown 输出中的链接目标绝对化，并做协议白名单过滤：
/// - 相对目标基于 base join 为绝对 http/https；
/// - 已是 http/https 的保持原样（origin 换站不影响——仅文本输出无请求）；
/// - `javascript:` / `data:` / `mailto:` / 空 / join 失败等一律**剥成纯文本**
///   （链接语法去除、保留可见文本；图片整体丢弃——非内容性装饰）；
/// - 输出不含任何不可达/危险协议的目标，供 LLM 安全消费。
pub fn absolutize_markdown_links(md: &str, base: &reqwest::Url) -> String {
    let re = link_re();
    re.replace_all(md, |caps: &regex::Captures<'_>| {
        let pre = &caps["pre"];
        let target = &caps["url"];
        if target.is_empty() {
            return pre.to_string(); // 无目标：保留原文（多为占位符）
        }
        // 绝对 or 相对统一走 Url 解析/join
        let absolute = reqwest::Url::parse(target)
            .or_else(|_| base.join(target))
            .ok();
        if let Some(u) = absolute {
            if matches!(u.scheme(), "http" | "https") {
                return format!("{}({})", pre, u);
            }
        }
        // 危险协议/不可解析：链接剥语法留文本；图片整体丢弃（alt 非正文）
        if pre.starts_with('!') {
            String::new()
        } else {
            pre.trim_start_matches('[')
                .trim_end_matches(']')
                .to_string()
        }
    })
    .into_owned()
}

// ═══════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_http_url ──

    #[test]
    fn url_guard_accepts_http_https() {
        let url = parse_http_url("https://example.com/a?b=1").unwrap();
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.host_str(), Some("example.com"));
        assert!(parse_http_url("http://127.0.0.1:8080/x").is_ok());
        // 首尾空白容忍（LLM 常带多余空格）
        assert!(parse_http_url("  https://example.com/  ").is_ok());
    }

    #[test]
    fn url_guard_rejects_other_schemes_and_garbage() {
        assert_eq!(
            parse_http_url("ftp://example.com/f"),
            Err(UrlReject::UnsupportedScheme("ftp".into()))
        );
        assert_eq!(
            parse_http_url("javascript:alert(1)"),
            Err(UrlReject::UnsupportedScheme("javascript".into()))
        );
        assert_eq!(
            parse_http_url("file:///C:/x.txt"),
            Err(UrlReject::UnsupportedScheme("file".into()))
        );
        assert_eq!(parse_http_url("不是链接"), Err(UrlReject::Invalid));
        assert_eq!(parse_http_url(""), Err(UrlReject::Invalid));
    }

    // ── is_private_ip：IP 字面量全类表驱动 ──

    fn ip(s: &str) -> IpAddr {
        s.parse().expect("测试 IP 字面量必须合法")
    }

    #[test]
    fn private_ip_table() {
        let private = [
            // IPv4 回环 /8（127.0.0.0~127.255.255.255）
            "127.0.0.1",
            "127.255.255.255",
            "127.8.9.10",
            // 未指定
            "0.0.0.0",
            // RFC1918
            "10.0.0.1",
            "10.255.255.255",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "192.168.255.254",
            // 链路本地
            "169.254.169.254",
            "169.254.0.1",
            // CGNAT / 基准测试 / 文档与保留段
            "100.64.0.1",
            "100.127.255.255",
            "198.18.0.1",
            "198.19.255.255",
            "192.0.0.1",
            "192.0.2.1",
            "198.51.100.1",
            "203.0.113.9",
            "192.88.99.1",
            // 组播 / 保留
            "224.0.0.1",
            "239.255.255.255",
            "240.0.0.1",
            "255.255.255.255",
            // IPv6 回环 / 未指定
            "::1",
            "0:0:0:0:0:0:0:1",
            "::",
            // IPv6 链路本地 fe80::/10（fe80~febf）
            "fe80::1",
            "febf::abcd",
            // IPv6 组播 / 文档段
            "ff02::1",
            "2001:db8::1",
            // IPv6 ULA fc00::/7（fc00~fdff）
            "fc00::1",
            "fd12:3456:789a::1",
            "fdff::ffff",
            // v4 映射 v6 内嵌私有（绕过形态）
            "::ffff:127.0.0.1",
            "::ffff:10.1.2.3",
            "::ffff:192.168.0.1",
            "::ffff:169.254.1.1",
            "::ffff:172.20.0.1",
            "::ffff:0.0.0.0",
        ];
        for s in private {
            assert!(is_private_ip(&ip(s)), "应判定私有: {s}");
        }
    }

    #[test]
    fn public_ip_table() {
        let public = [
            // 公网 v4
            "8.8.8.8",
            "1.1.1.1",
            "114.114.114.114",
            "93.184.216.34",
            // RFC1918 边界外
            "172.15.0.1",
            "172.32.0.1",
            "172.32.0.0",
            // 公网 v6
            "2001:4860:4860::8888",
            "2606:4700:4700::1111",
            "2400:3200::1",
            // v4 映射 v6 内嵌公网
            "::ffff:8.8.8.8",
            "::ffff:1.1.1.1",
        ];
        for s in public {
            assert!(!is_private_ip(&ip(s)), "应判定公网: {s}");
        }
    }

    #[test]
    fn public_ip_boundary_cases() {
        // RFC1918 172.16/12 精确边界：172.16.0.0 私有 / 172.31.255.255 私有 /
        // 172.32.0.0 与 172.15.255.255 公网
        assert!(is_private_ip(&ip("172.16.0.0")));
        assert!(is_private_ip(&ip("172.31.255.255")));
        assert!(!is_private_ip(&ip("172.32.0.0")));
        assert!(!is_private_ip(&ip("172.15.255.255")));
        // 192.168/16 边界
        assert!(is_private_ip(&ip("192.168.0.0")));
        assert!(!is_private_ip(&ip("192.169.0.1")));
        // 169.254/16 边界
        assert!(is_private_ip(&ip("169.254.255.255")));
        assert!(!is_private_ip(&ip("169.255.0.1")));
        // IPv6 链路本地 /10 边界：fe80:: 私有，fe7f:: 前 10 位不命中（fe7f::/16 之外）
        assert!(is_private_ip(&ip("fe80::")));
        assert!(!is_private_ip(&ip("fe7f::1")));
        // 新增非公网段边界
        assert!(is_private_ip(&ip("100.64.0.0")));
        assert!(!is_private_ip(&ip("100.63.255.255")));
        assert!(!is_private_ip(&ip("100.128.0.0")));
        assert!(is_private_ip(&ip("198.18.0.0")));
        assert!(!is_private_ip(&ip("198.17.255.255")));
        assert!(!is_private_ip(&ip("198.20.0.0")));
        assert!(is_private_ip(&ip("203.0.113.0")));
        assert!(!is_private_ip(&ip("203.0.112.255")));
        assert!(!is_private_ip(&ip("203.0.114.0")));
    }

    // ── host_is_blocked：白名单 + 字面量 + 域名 mock ──

    /// mock resolver：域名 → 罐头 IP 表；未知域名 = 解析失败
    fn mock_resolve(host: &str) -> io::Result<Vec<IpAddr>> {
        match host {
            "pub.example.com" => Ok(vec![ip("93.184.216.34")]),
            "mixed.example.com" => Ok(vec![ip("93.184.216.34"), ip("10.0.0.7")]),
            "localhost" => Ok(vec![ip("127.0.0.1")]),
            "ipv6-only.example.com" => Ok(vec![ip("2001:4860:4860::8888")]),
            "v6-ula.example.com" => Ok(vec![ip("fd00::1")]),
            "empty.example.com" => Ok(vec![]),
            _ => Err(io::Error::new(io::ErrorKind::NotFound, "NXDOMAIN")),
        }
    }

    #[test]
    fn host_guard_ip_literal_path() {
        let empty: Vec<String> = vec![];
        // 字面量私有 → 拒
        assert!(host_is_blocked("127.0.0.1", &empty, mock_resolve));
        assert!(host_is_blocked("10.0.0.1", &empty, mock_resolve));
        assert!(host_is_blocked("::1", &empty, mock_resolve));
        assert!(host_is_blocked("::ffff:127.0.0.1", &empty, mock_resolve));
        // 字面量公网 → 放
        assert!(!host_is_blocked("8.8.8.8", &empty, mock_resolve));
        // 字面量直接分类，不该调用 resolver（域名 mock 不认识的 IP 串也会过）
        assert!(!host_is_blocked("1.2.3.4", &empty, mock_resolve));
    }

    #[test]
    fn host_guard_dns_resolution_path() {
        let empty: Vec<String> = vec![];
        // 全公网 → 放
        assert!(!host_is_blocked("pub.example.com", &empty, mock_resolve));
        assert!(!host_is_blocked(
            "ipv6-only.example.com",
            &empty,
            mock_resolve
        ));
        // 任一私有（部分 A/AAAA 指向内网）→ 拒
        assert!(host_is_blocked("mixed.example.com", &empty, mock_resolve));
        // localhost 经 DNS 解析命中回环 → 拒
        assert!(host_is_blocked("localhost", &empty, mock_resolve));
        // v6 ULA → 拒
        assert!(host_is_blocked("v6-ula.example.com", &empty, mock_resolve));
    }

    #[test]
    fn host_guard_conservative_on_resolution_failure_or_empty() {
        let empty: Vec<String> = vec![];
        // 解析失败 → 按私有保守拒
        assert!(host_is_blocked("nxdomain.invalid", &empty, mock_resolve));
        // 解析成功但空结果 → 同样保守拒
        assert!(host_is_blocked("empty.example.com", &empty, mock_resolve));
    }

    #[test]
    fn host_guard_allowlist_bypass() {
        let allow_loopback = vec!["127.0.0.1".to_string()];
        // 白名单精确命中 → 直放（测试注入点）
        assert!(!host_is_blocked("127.0.0.1", &allow_loopback, mock_resolve));
        // 白名单外的私有字面量照拒
        assert!(host_is_blocked("10.0.0.1", &allow_loopback, mock_resolve));
        // 白名单只精确匹配：同值的 localhost 域名不走豁免（DNS 仍拒）
        assert!(host_is_blocked("localhost", &allow_loopback, mock_resolve));
    }

    #[test]
    fn resolve_public_ip_returns_pinned_public_address() {
        let empty: Vec<String> = vec![];
        assert_eq!(
            resolve_public_ip("pub.example.com", &empty, mock_resolve).unwrap(),
            ip("93.184.216.34")
        );
        assert_eq!(
            resolve_public_ip("mixed.example.com", &empty, mock_resolve),
            Err(HostReject::Private)
        );
        assert_eq!(
            resolve_public_ip("empty.example.com", &empty, mock_resolve),
            Err(HostReject::Empty)
        );
        assert_eq!(
            resolve_public_ip("nxdomain.invalid", &empty, mock_resolve),
            Err(HostReject::ResolveFailed)
        );
    }

    // ── truncate_chars ──

    #[test]
    fn truncate_respects_char_boundary() {
        assert_eq!(truncate_chars("hello", 10), "hello");
        // 中文按字计，不按字节
        let s = "明日方舟网络层实现任务";
        assert_eq!(truncate_chars(s, 5), "明日方舟网");
        // 截断不超过上限
        assert!(truncate_chars(s, 5).chars().count() <= 5);
        // 空输入
        assert_eq!(truncate_chars("", 5), "");
        // 多字节字符边界不 panic（截断值落在多字节序列中间仍安全）
        let wide = "网络层".repeat(20);
        let t = truncate_chars(&wide, 17);
        assert_eq!(t.chars().count(), 17);
    }

    // ── absolutize_markdown_links ──

    fn base(url: &str) -> reqwest::Url {
        reqwest::Url::parse(url).unwrap()
    }

    #[test]
    fn absolutize_resolves_relative_links() {
        let md = "正文见[天赋系统](/docs/talent)与[相对路径](guide/intro)。";
        let out = absolutize_markdown_links(md, &base("https://prts.wiki/news/1"));
        assert!(
            out.contains("](https://prts.wiki/docs/talent)"),
            "绝对路径应 join 到站根: {out}"
        );
        assert!(
            out.contains("](https://prts.wiki/news/guide/intro)"),
            "相对路径应基于页面 join: {out}"
        );
        assert!(out.contains("正文见"), "链接文本保留: {out}");
    }

    #[test]
    fn absolutize_keeps_absolute_http_links() {
        let md = "参考[官方站](https://ak.hypergryph.com/)内容。";
        let out = absolutize_markdown_links(md, &base("https://prts.wiki/"));
        assert!(out.contains("](https://ak.hypergryph.com/)"), "got: {out}");
    }

    #[test]
    fn absolutize_strips_dangerous_schemes_to_plain_text() {
        let md =
            "点击[这里](javascript:alert(1))、[甲](data:text/html,<script>)与[乙](mailto:a@b.c)。";
        let out = absolutize_markdown_links(md, &base("https://prts.wiki/"));
        assert!(!out.contains("javascript:"), "got: {out}");
        assert!(!out.contains("data:text"), "got: {out}");
        assert!(!out.contains("mailto:"), "got: {out}");
        assert!(out.contains("点击这里、甲与乙。"), "剥语法留文本: {out}");
    }

    #[test]
    fn absolutize_drops_unsafe_images_keeps_http_images() {
        // data: 图（追踪像素等）→ 整体丢弃；http(s) 图保留
        let md = "![头像](data:image/png;base64,AAAA) 后文 ![logo](https://prts.wiki/logo.png)";
        let out = absolutize_markdown_links(md, &base("https://prts.wiki/"));
        assert!(!out.contains("data:"), "got: {out}");
        assert!(
            out.contains("![logo](https://prts.wiki/logo.png)"),
            "got: {out}"
        );
        // javascript: 图 → 丢弃（含 alt 不回退为文本）
        let out = absolutize_markdown_links("![x](javascript:void 0)", &base("https://prts.wiki/"));
        assert_eq!(out.trim(), "");
    }

    #[test]
    fn absolutize_handles_wiki_parenthesized_urls() {
        // 目标 URL 含一层平衡括号（维基风格）应整体保留
        let md = "[霜星](https://zh.wikipedia.org/wiki/%E9%9C%9C%E6%98%9F_(%E6%98%8E%E6%97%A5%E6%96%B9%E8%88%9F))";
        let out = absolutize_markdown_links(md, &base("https://prts.wiki/"));
        assert!(
            out.contains("](https://zh.wikipedia.org/wiki/%E9%9C%9C%E6%98%9F_(%E6%98%8E%E6%97%A5%E6%96%B9%E8%88%9F))"),
            "括号 URL 不应被截断: {out}"
        );
    }

    #[test]
    fn absolutize_keeps_placeholder_text_with_empty_target() {
        // 空目标 = 作者占位符（如 [待补链接](  )）：不猜不拆，整段保留原文；
        // 非法目标的剥除语义已由 dangerous_schemes 用例覆盖（javascript:/data:/mailto:）
        let md = "占位[待补链接](  )结束";
        let out = absolutize_markdown_links(md, &base("https://prts.wiki/"));
        assert_eq!(out.trim(), "占位[待补链接]结束");
    }
}
