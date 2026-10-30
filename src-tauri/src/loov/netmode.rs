//! 网络模式与节点偏好；「代理 / 全局」模式下接管系统代理。
//!
//! - 直连（默认）：安全通道只给 Loov 自己用（new-api、安装包、经 Loov 启动的应用），系统代理不动。
//! - 代理：系统代理指向本机的 `sys-in` 入站，国内网站（geosite:cn / geoip:cn）直连，其余经节点。
//! - 全局：系统代理指向 `sys-in`，除内网外全部经节点。
//!
//! 系统代理不支持用户名口令，所以 `sys-in` 是单独的免认证入站（只监听 127.0.0.1）；Loov 自己用的
//! 带口令入站不变。开启前把员工原来的系统代理设置备份到 `<loov>/sysproxy-backup.json`，切回直连、
//! 隧道断开、登出、退出时按备份原样还原（员工自己的 Clash 等设置不丢）。上次异常退出留下的备份在
//! 启动时先还原。

use std::path::PathBuf;
use std::sync::atomic::{AtomicU16, Ordering};

use serde::{Deserialize, Serialize};

use super::credentials::{loov_dir, write_private};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NetMode {
    #[default]
    Direct,
    Rule,
    Global,
}

/// 员工的选择：手选的节点（None = 自动）与网络模式。存在 `<loov>/prefs.json`。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Prefs {
    pub node: Option<String>,
    pub mode: NetMode,
    /// agent 权限档位（`loov::safety`）
    pub safety: super::safety::SafetyLevel,
    /// Loov 上次写进各家配置的权限值（键见 `safety::*_KEY`）：员工改过的不覆盖，恢复时据此清除
    pub safety_applied: std::collections::BTreeMap<String, String>,
}

fn prefs_path() -> PathBuf {
    loov_dir().join("prefs.json")
}

pub fn load_prefs() -> Prefs {
    std::fs::read(prefs_path())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save_prefs(p: &Prefs) -> Result<(), String> {
    let _ = std::fs::create_dir_all(loov_dir());
    write_private(&prefs_path(), serde_json::to_string_pretty(p).unwrap_or_default().as_bytes())
}

// ───────────────────────────── 本机 sys-in 端口 ─────────────────────────────

/// `sys-in` 的 HTTP / SOCKS 端口。优先固定端口（员工排障、其它工具手动填代理时好记），被占就随机。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SysPorts {
    pub http: u16,
    pub socks: u16,
}

const PREFERRED_SYS_PORTS: SysPorts = SysPorts { http: 17890, socks: 17891 };

/// 当前在用的 sys-in 端口（0 = 没开）。`sysproxy::detect` 据此跳过指向自己的系统代理。
static ACTIVE_HTTP: AtomicU16 = AtomicU16::new(0);
static ACTIVE_SOCKS: AtomicU16 = AtomicU16::new(0);

pub fn pick_sys_ports() -> Result<SysPorts, String> {
    let free = |p: u16| std::net::TcpListener::bind(("127.0.0.1", p)).is_ok();
    if free(PREFERRED_SYS_PORTS.http) && free(PREFERRED_SYS_PORTS.socks) {
        return Ok(PREFERRED_SYS_PORTS);
    }
    let a = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| format!("分配端口失败：{e}"))?;
    let b = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| format!("分配端口失败：{e}"))?;
    Ok(SysPorts {
        http: a.local_addr().map_err(|e| e.to_string())?.port(),
        socks: b.local_addr().map_err(|e| e.to_string())?.port(),
    })
}

/// 本机地址的这个端口是不是 Loov 的 sys-in（读系统代理找上游时要跳过）。
pub fn is_own_port(port: u16) -> bool {
    port != 0
        && (port == ACTIVE_HTTP.load(Ordering::SeqCst)
            || port == ACTIVE_SOCKS.load(Ordering::SeqCst)
            || port == PREFERRED_SYS_PORTS.http
            || port == PREFERRED_SYS_PORTS.socks)
}

// ───────────────────────────── 系统代理 ─────────────────────────────

fn backup_path() -> PathBuf {
    loov_dir().join("sysproxy-backup.json")
}

/// 绕过列表：本机与内网不经代理。Windows 的 ProxyOverride 只认通配，macOS / GNOME 认网段。
#[cfg(windows)]
const BYPASS: &[&str] = &[
    "localhost", "127.*", "10.*", "172.16.*", "172.17.*", "172.18.*", "172.19.*", "172.2*", "172.30.*",
    "172.31.*", "192.168.*", "*.local",
];
#[cfg(not(windows))]
const BYPASS: &[&str] = &["localhost", "127.0.0.0/8", "::1", "10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16", "*.local"];

/// 把系统代理指向 sys-in。第一次开启时先备份原设置；已有备份（例如从代理切到全局）不覆盖。
pub fn engage(ports: SysPorts) -> Result<(), String> {
    if !backup_path().exists() {
        let saved = platform::snapshot()?;
        let _ = std::fs::create_dir_all(loov_dir());
        write_private(&backup_path(), serde_json::to_string_pretty(&saved).unwrap_or_default().as_bytes())?;
    }
    ACTIVE_HTTP.store(ports.http, Ordering::SeqCst);
    ACTIVE_SOCKS.store(ports.socks, Ordering::SeqCst);
    platform::set(ports)
}

/// 按备份还原系统代理并删掉备份；没有备份（从没接管过）什么都不做。
pub fn release() {
    ACTIVE_HTTP.store(0, Ordering::SeqCst);
    ACTIVE_SOCKS.store(0, Ordering::SeqCst);
    let path = backup_path();
    let Ok(bytes) = std::fs::read(&path) else { return };
    match serde_json::from_slice::<serde_json::Value>(&bytes) {
        Ok(saved) => match platform::restore(&saved) {
            Ok(()) => log::info!("[loov] 已还原系统代理"),
            Err(e) => {
                // 留着备份，下次启动 / 切换时再试
                log::error!("[loov] 还原系统代理失败：{e}");
                return;
            }
        },
        Err(e) => log::error!("[loov] 系统代理备份无法解析，丢弃：{e}"),
    }
    let _ = std::fs::remove_file(path);
}

/// 有没有接管着系统代理（备份在就算）。
pub fn engaged() -> bool {
    backup_path().exists()
}

#[cfg(windows)]
mod platform {
    use super::{SysPorts, BYPASS};
    use serde_json::{json, Value};
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};
    use winreg::RegKey;

    const KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings";

    fn open(write: bool) -> Result<RegKey, String> {
        let flags = if write { KEY_READ | KEY_WRITE } else { KEY_READ };
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(KEY, flags)
            .map_err(|e| format!("打开 Internet 设置失败：{e}"))
    }

    pub fn snapshot() -> Result<Value, String> {
        let k = open(false)?;
        Ok(json!({
            "ProxyEnable": k.get_value::<u32, _>("ProxyEnable").unwrap_or(0),
            "ProxyServer": k.get_value::<String, _>("ProxyServer").ok(),
            "ProxyOverride": k.get_value::<String, _>("ProxyOverride").ok(),
            "AutoConfigURL": k.get_value::<String, _>("AutoConfigURL").ok(),
        }))
    }

    pub fn set(p: SysPorts) -> Result<(), String> {
        let k = open(true)?;
        let server = format!("http=127.0.0.1:{h};https=127.0.0.1:{h};socks=127.0.0.1:{s}", h = p.http, s = p.socks);
        let bypass = format!("{};<local>", BYPASS.join(";"));
        k.set_value("ProxyServer", &server).map_err(|e| e.to_string())?;
        k.set_value("ProxyOverride", &bypass).map_err(|e| e.to_string())?;
        // PAC 优先于手动代理：接管期间先拿掉，还原时放回
        let _ = k.delete_value("AutoConfigURL");
        k.set_value("ProxyEnable", &1u32).map_err(|e| e.to_string())?;
        notify();
        Ok(())
    }

    pub fn restore(saved: &Value) -> Result<(), String> {
        let k = open(true)?;
        let put = |name: &str| -> Result<(), String> {
            match saved.get(name).and_then(Value::as_str) {
                Some(v) => k.set_value(name, &v.to_string()).map_err(|e| e.to_string()),
                None => {
                    let _ = k.delete_value(name);
                    Ok(())
                }
            }
        };
        put("ProxyServer")?;
        put("ProxyOverride")?;
        put("AutoConfigURL")?;
        let enable = saved.get("ProxyEnable").and_then(Value::as_u64).unwrap_or(0) as u32;
        k.set_value("ProxyEnable", &enable).map_err(|e| e.to_string())?;
        notify();
        Ok(())
    }

    /// 通知 WinINet 设置已改（浏览器等不用重启就生效）。
    fn notify() {
        use windows_sys::Win32::Networking::WinInet::{
            InternetSetOptionW, INTERNET_OPTION_REFRESH, INTERNET_OPTION_SETTINGS_CHANGED,
        };
        unsafe {
            InternetSetOptionW(std::ptr::null(), INTERNET_OPTION_SETTINGS_CHANGED, std::ptr::null(), 0);
            InternetSetOptionW(std::ptr::null(), INTERNET_OPTION_REFRESH, std::ptr::null(), 0);
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{SysPorts, BYPASS};
    use serde_json::{json, Value};
    use std::process::Command;

    fn ns(args: &[&str]) -> Result<String, String> {
        let out = Command::new("networksetup").args(args).output().map_err(|e| format!("networksetup：{e}"))?;
        if !out.status.success() {
            return Err(format!(
                "networksetup {} 失败：{}",
                args.first().unwrap_or(&""),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// 启用的网络服务（第一行是说明，带 * 的是停用的服务）。
    fn services() -> Result<Vec<String>, String> {
        Ok(ns(&["-listallnetworkservices"])?
            .lines()
            .skip(1)
            .filter(|l| !l.trim().is_empty() && !l.starts_with('*'))
            .map(str::to_string)
            .collect())
    }

    /// `-getwebproxy` 的输出：Enabled / Server / Port。
    fn parse(out: &str) -> Value {
        let get = |k: &str| {
            out.lines()
                .find_map(|l| l.split_once(':').filter(|(a, _)| a.trim() == k).map(|(_, b)| b.trim().to_string()))
                .unwrap_or_default()
        };
        json!({ "enabled": get("Enabled") == "Yes", "server": get("Server"), "port": get("Port") })
    }

    const KINDS: [(&str, &str, &str); 3] = [
        ("web", "-getwebproxy", "-setwebproxy"),
        ("secureweb", "-getsecurewebproxy", "-setsecurewebproxy"),
        ("socks", "-getsocksfirewallproxy", "-setsocksfirewallproxy"),
    ];

    pub fn snapshot() -> Result<Value, String> {
        let mut all = serde_json::Map::new();
        for svc in services()? {
            let mut one = serde_json::Map::new();
            for (kind, get, _) in KINDS {
                one.insert(kind.to_string(), parse(&ns(&[get, &svc])?));
            }
            let bypass: Vec<String> = ns(&["-getproxybypassdomains", &svc])?
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with("There aren't"))
                .map(str::to_string)
                .collect();
            one.insert("bypass".into(), json!(bypass));
            all.insert(svc, Value::Object(one));
        }
        Ok(Value::Object(all))
    }

    pub fn set(p: SysPorts) -> Result<(), String> {
        let (h, s) = (p.http.to_string(), p.socks.to_string());
        for svc in services()? {
            ns(&["-setwebproxy", &svc, "127.0.0.1", &h])?;
            ns(&["-setsecurewebproxy", &svc, "127.0.0.1", &h])?;
            ns(&["-setsocksfirewallproxy", &svc, "127.0.0.1", &s])?;
            let mut args = vec!["-setproxybypassdomains", svc.as_str()];
            args.extend(BYPASS.iter().copied());
            ns(&args)?;
        }
        Ok(())
    }

    pub fn restore(saved: &Value) -> Result<(), String> {
        let Some(all) = saved.as_object() else { return Ok(()) };
        for (svc, one) in all {
            for (kind, _, set) in KINDS {
                let v = &one[kind];
                let server = v["server"].as_str().unwrap_or_default();
                let port = v["port"].as_str().unwrap_or_default();
                if !server.is_empty() && port != "0" && !port.is_empty() {
                    let _ = ns(&[set, svc, server, port]);
                }
                let state_flag = format!("{set}state");
                let on = if v["enabled"].as_bool().unwrap_or(false) { "on" } else { "off" };
                ns(&[&state_flag, svc, on])?;
            }
            let bypass: Vec<&str> = one["bypass"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
            let mut args = vec!["-setproxybypassdomains", svc.as_str()];
            if bypass.is_empty() {
                args.push("Empty");
            } else {
                args.extend(bypass);
            }
            let _ = ns(&args);
        }
        Ok(())
    }
}

/// Linux：GNOME 的 gsettings（KDE 等桌面暂不支持，返回错误由界面提示）。
#[cfg(not(any(windows, target_os = "macos")))]
mod platform {
    use super::{SysPorts, BYPASS};
    use serde_json::{json, Value};
    use std::process::Command;

    const KEYS: [(&str, &str); 8] = [
        ("org.gnome.system.proxy", "mode"),
        ("org.gnome.system.proxy", "ignore-hosts"),
        ("org.gnome.system.proxy.http", "host"),
        ("org.gnome.system.proxy.http", "port"),
        ("org.gnome.system.proxy.https", "host"),
        ("org.gnome.system.proxy.https", "port"),
        ("org.gnome.system.proxy.socks", "host"),
        ("org.gnome.system.proxy.socks", "port"),
    ];

    fn gs(args: &[&str]) -> Result<String, String> {
        let out = Command::new("gsettings").args(args).output().map_err(|e| format!("gsettings 不可用（目前只支持 GNOME 桌面）：{e}"))?;
        if !out.status.success() {
            return Err(format!("gsettings 失败：{}", String::from_utf8_lossy(&out.stderr).trim()));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    pub fn snapshot() -> Result<Value, String> {
        let mut m = serde_json::Map::new();
        for (schema, key) in KEYS {
            m.insert(format!("{schema} {key}"), json!(gs(&["get", schema, key])?));
        }
        Ok(Value::Object(m))
    }

    pub fn set(p: SysPorts) -> Result<(), String> {
        let (h, s) = (p.http.to_string(), p.socks.to_string());
        for scheme in ["http", "https"] {
            let schema = format!("org.gnome.system.proxy.{scheme}");
            gs(&["set", &schema, "host", "127.0.0.1"])?;
            gs(&["set", &schema, "port", &h])?;
        }
        gs(&["set", "org.gnome.system.proxy.socks", "host", "127.0.0.1"])?;
        gs(&["set", "org.gnome.system.proxy.socks", "port", &s])?;
        let ignore = format!("[{}]", BYPASS.iter().map(|b| format!("'{b}'")).collect::<Vec<_>>().join(", "));
        gs(&["set", "org.gnome.system.proxy", "ignore-hosts", &ignore])?;
        gs(&["set", "org.gnome.system.proxy", "mode", "manual"])?;
        Ok(())
    }

    pub fn restore(saved: &Value) -> Result<(), String> {
        let Some(m) = saved.as_object() else { return Ok(()) };
        for (k, v) in m {
            let (Some((schema, key)), Some(value)) = (k.split_once(' '), v.as_str()) else { continue };
            gs(&["set", schema, key, value])?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefs_default_is_auto_direct_and_roundtrips() {
        let p = Prefs::default();
        assert_eq!(p.node, None);
        assert_eq!(p.mode, NetMode::Direct);
        let p = Prefs { node: Some("tw1".into()), mode: NetMode::Rule, ..Default::default() };
        let text = serde_json::to_string(&p).unwrap();
        assert!(text.starts_with(r#"{"node":"tw1","mode":"rule","safety":"standard""#), "{text}");
        assert_eq!(serde_json::from_str::<Prefs>(&text).unwrap(), p);
        // 旧文件 / 缺字段：用默认值
        assert_eq!(serde_json::from_str::<Prefs>("{}").unwrap(), Prefs::default());
    }

    #[test]
    fn own_ports_are_recognised() {
        assert!(is_own_port(17890));
        assert!(is_own_port(17891));
        assert!(!is_own_port(7890));
        assert!(!is_own_port(0));
    }
}
