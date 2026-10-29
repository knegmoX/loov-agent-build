//! 本机代理检测：直连隧道节点被网络屏蔽时，让 xray 经员工自己的代理（Clash / v2rayN / 公司代理）
//! 去连节点。xray 子进程不读系统代理，所以要我们读出来写进它的配置（`sockopt.dialerProxy`）。
//! 经代理的链路仍是 Reality 端到端加密，代理只看到发往伪装站点的 TLS。

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamKind {
    Http,
    Socks,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upstream {
    pub kind: UpstreamKind,
    pub host: String,
    pub port: u16,
    pub user: Option<String>,
    pub pass: Option<String>,
    /// 从哪读到的（给诊断看）。
    pub source: &'static str,
}

impl fmt::Display for Upstream {
    /// 不含口令，给界面和日志用。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let scheme = match self.kind {
            UpstreamKind::Http => "http",
            UpstreamKind::Socks => "socks5",
        };
        write!(f, "{scheme}://{}:{}", self.host, self.port)
    }
}

/// 按「环境变量 → 系统设置」的顺序找第一个可用的代理；指向 Loov 自己的不算。
pub fn detect() -> Option<Upstream> {
    let found = from_env().or_else(from_system);
    found.filter(|u| !points_to_self(u))
}

/// 指向 Loov 自己：本地路由的端口，或「代理 / 全局」模式下 Loov 接管系统代理用的 sys-in 端口
/// （不跳过的话会把自己当成上游，节点连接绕回自己）。
fn points_to_self(u: &Upstream) -> bool {
    let local = matches!(u.host.as_str(), "127.0.0.1" | "localhost" | "::1");
    local && (u.port == crate::proxy::http_client::get_proxy_port() || super::netmode::is_own_port(u.port))
}

fn from_env() -> Option<Upstream> {
    // ALL_PROXY 常见是 socks5；HTTPS_PROXY 优先，因为节点是 TLS 流量
    for key in ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy", "HTTP_PROXY", "http_proxy"] {
        if let Ok(v) = std::env::var(key) {
            if let Some(u) = parse_url(v.trim(), "环境变量") {
                return Some(u);
            }
        }
    }
    None
}

/// 解析 `scheme://[user:pass@]host:port`；没有 scheme 时按 HTTP。
pub fn parse_url(value: &str, source: &'static str) -> Option<Upstream> {
    if value.is_empty() {
        return None;
    }
    let with_scheme = if value.contains("://") { value.to_string() } else { format!("http://{value}") };
    let url = url::Url::parse(&with_scheme).ok()?;
    let kind = match url.scheme() {
        "http" | "https" => UpstreamKind::Http,
        "socks" | "socks5" | "socks5h" => UpstreamKind::Socks,
        _ => return None,
    };
    let host = url.host_str()?.trim_start_matches('[').trim_end_matches(']').to_string();
    let port = url.port().unwrap_or(match kind {
        UpstreamKind::Http => 8080,
        UpstreamKind::Socks => 1080,
    });
    let user = Some(url.username()).filter(|s| !s.is_empty()).map(|s| percent(s));
    let pass = url.password().map(percent);
    Some(Upstream { kind, host, port, user, pass, source })
}

fn percent(s: &str) -> String {
    url::form_urlencoded::parse(format!("x={s}").as_bytes())
        .next()
        .map(|(_, v)| v.into_owned())
        .unwrap_or_else(|| s.to_string())
}

/// Windows「Internet 选项」里的 `ProxyServer`：`host:port`，或 `http=h:p;https=h:p;socks=h:p`。
pub fn parse_windows_proxy_server(value: &str) -> Option<Upstream> {
    let value = value.trim();
    if !value.contains('=') {
        return parse_url(value, "系统代理");
    }
    let mut by_scheme = std::collections::HashMap::new();
    for part in value.split(';') {
        if let Some((k, v)) = part.split_once('=') {
            by_scheme.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    if let Some(v) = by_scheme.get("https").or_else(|| by_scheme.get("http")) {
        return parse_url(v, "系统代理");
    }
    by_scheme.get("socks").and_then(|v| parse_url(&format!("socks5://{v}"), "系统代理"))
}

/// macOS `scutil --proxy` 的输出（`HTTPSEnable : 1` / `HTTPSProxy : 127.0.0.1` / `HTTPSPort : 7890` …）。
pub fn parse_scutil(out: &str) -> Option<Upstream> {
    let get = |k: &str| -> Option<String> {
        out.lines().find_map(|l| {
            let (key, val) = l.split_once(':')?;
            (key.trim() == k).then(|| val.trim().to_string())
        })
    };
    for (prefix, kind) in [("HTTPS", UpstreamKind::Http), ("HTTP", UpstreamKind::Http), ("SOCKS", UpstreamKind::Socks)] {
        if get(&format!("{prefix}Enable")).as_deref() != Some("1") {
            continue;
        }
        let (Some(host), Some(port)) = (get(&format!("{prefix}Proxy")), get(&format!("{prefix}Port"))) else { continue };
        let Ok(port) = port.parse::<u16>() else { continue };
        return Some(Upstream { kind, host, port, user: None, pass: None, source: "系统代理" });
    }
    None
}

#[cfg(windows)]
fn from_system() -> Option<Upstream> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;
    let key = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings")
        .ok()?;
    let enabled: u32 = key.get_value("ProxyEnable").ok()?;
    if enabled == 0 {
        return None;
    }
    let server: String = key.get_value("ProxyServer").ok()?;
    parse_windows_proxy_server(&server)
}

#[cfg(target_os = "macos")]
fn from_system() -> Option<Upstream> {
    let out = std::process::Command::new("scutil").arg("--proxy").output().ok()?;
    parse_scutil(&String::from_utf8_lossy(&out.stdout))
}

#[cfg(not(any(windows, target_os = "macos")))]
fn from_system() -> Option<Upstream> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_env_style_urls() {
        let u = parse_url("http://127.0.0.1:7890", "t").unwrap();
        assert_eq!((u.kind, u.host.as_str(), u.port), (UpstreamKind::Http, "127.0.0.1", 7890));
        let u = parse_url("socks5h://a%40b:p%3Aw@proxy.corp:1080", "t").unwrap();
        assert_eq!(u.kind, UpstreamKind::Socks);
        assert_eq!(u.user.as_deref(), Some("a@b"));
        assert_eq!(u.pass.as_deref(), Some("p:w"));
        let u = parse_url("10.0.0.5:3128", "t").unwrap();
        assert_eq!((u.kind, u.port), (UpstreamKind::Http, 3128));
        assert!(parse_url("", "t").is_none());
        assert!(parse_url("ftp://x:1", "t").is_none());
        // Display 不带口令
        assert_eq!(parse_url("http://u:secret@h:1", "t").unwrap().to_string(), "http://h:1");
    }

    #[test]
    fn parses_windows_proxy_server() {
        let u = parse_windows_proxy_server("127.0.0.1:10809").unwrap();
        assert_eq!((u.kind, u.port), (UpstreamKind::Http, 10809));
        let u = parse_windows_proxy_server("http=127.0.0.1:8080;https=127.0.0.1:8443;ftp=x:21").unwrap();
        assert_eq!(u.port, 8443);
        let u = parse_windows_proxy_server("socks=127.0.0.1:10808").unwrap();
        assert_eq!((u.kind, u.port), (UpstreamKind::Socks, 10808));
    }

    #[test]
    fn parses_scutil() {
        let out = "<dictionary> {\n  HTTPEnable : 0\n  HTTPSEnable : 1\n  HTTPSPort : 7890\n  HTTPSProxy : 127.0.0.1\n  SOCKSEnable : 1\n  SOCKSPort : 7891\n  SOCKSProxy : 127.0.0.1\n}";
        let u = parse_scutil(out).unwrap();
        assert_eq!((u.kind, u.port), (UpstreamKind::Http, 7890));
        let only_socks = "  SOCKSEnable : 1\n  SOCKSPort : 7891\n  SOCKSProxy : 127.0.0.1\n";
        assert_eq!(parse_scutil(only_socks).unwrap().kind, UpstreamKind::Socks);
        assert!(parse_scutil("  HTTPSEnable : 0\n").is_none());
    }
}
