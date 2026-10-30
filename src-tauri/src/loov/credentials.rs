//! 设备令牌与 bootstrap 缓存的加密落盘。
//!
//! 为什么不放 settings 表：DB 会被导入导出 / WebDAV / S3 同步整库上传，令牌会随备份出走。
//!
//! 格式 v2（当前）：
//! - `credentials.enc` = `LOOV2` ‖ nonce(12) ‖ AES-256-GCM(数据密钥, JSON)
//! - `credentials.key` = 被封装的 32 字节随机数据密钥，首字节标明封装方式：
//!   - `0x02` Windows DPAPI（绑定当前 Windows 用户，不弹窗）
//!   - `0x01` 机器绑定（HKDF-SHA256(机器 ID ‖ 用户名) 派生的密钥做 AES-GCM），macOS / Linux 及 DPAPI 失败时
//!
//! macOS 暂不用钥匙串：未签名 / ad-hoc 签名的 app 每次更新后访问钥匙串都会弹授权框；Linux 没有
//! secret-service 的桌面（CRD、精简发行版）钥匙串不可用或需要解锁。有了开发者签名后再切过去。
//! 旧格式 v1（`LOOV1` ‖ nonce ‖ AES-GCM(机器密钥, JSON)）读取时自动迁移到 v2。

use std::path::PathBuf;

use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use super::hub_client::Bootstrap;

const MAGIC: &[u8; 5] = b"LOOV1";
const MAGIC_V2: &[u8; 5] = b"LOOV2";
const WRAP_MACHINE: u8 = 0x01;
const WRAP_DPAPI: u8 = 0x02;
const HKDF_SALT: &[u8] = b"loov-agent-credentials-v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Credentials {
    pub device_token: String,
    pub username: String,
    pub device_id: String,
    /// 最近一次成功拉取的 bootstrap。内存里含真实 Key；落盘时 Key 被清空（审计 S1-02），
    /// 启动时用设备令牌向 hub 重新拿。
    pub bootstrap: Option<Bootstrap>,
    pub cached_at: i64,
}

/// `<app_config_dir>/loov/`：凭据、xray 运行目录都在这里。
pub fn loov_dir() -> PathBuf {
    crate::config::get_app_config_dir().join("loov")
}

pub fn credentials_path() -> PathBuf {
    loov_dir().join("credentials.enc")
}

pub fn key_path() -> PathBuf {
    loov_dir().join("credentials.key")
}

fn machine_ikm() -> Vec<u8> {
    let mid = machine_uid::get().unwrap_or_else(|_| "no-machine-id".to_string());
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "user".to_string());
    let mut ikm = mid.into_bytes();
    ikm.push(0);
    ikm.extend_from_slice(user.as_bytes());
    ikm
}

fn derive_key() -> Key<Aes256Gcm> {
    let hk = Hkdf::<Sha256>::new(Some(HKDF_SALT), &machine_ikm());
    let mut okm = [0u8; 32];
    hk.expand(b"aes-256-gcm", &mut okm).expect("hkdf expand");
    *Key::<Aes256Gcm>::from_slice(&okm)
}

pub fn seal(plain: &[u8]) -> Vec<u8> {
    let cipher = Aes256Gcm::new(&derive_key());
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ct = cipher.encrypt(&nonce, plain).expect("aes-gcm encrypt");
    let mut out = Vec::with_capacity(MAGIC.len() + 12 + ct.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    out
}

pub fn open(blob: &[u8]) -> Result<Vec<u8>, String> {
    if blob.len() < MAGIC.len() + 12 + 16 || &blob[..MAGIC.len()] != MAGIC {
        return Err("凭据文件格式不对".into());
    }
    let nonce = Nonce::from_slice(&blob[MAGIC.len()..MAGIC.len() + 12]);
    let cipher = Aes256Gcm::new(&derive_key());
    cipher
        .decrypt(nonce, &blob[MAGIC.len() + 12..])
        .map_err(|_| "凭据文件无法解密（被篡改或来自另一台机器）".to_string())
}

fn random_key() -> [u8; 32] {
    use aes_gcm::aead::rand_core::RngCore;
    let mut k = [0u8; 32];
    OsRng.fill_bytes(&mut k);
    k
}

/// 用数据密钥加 / 解密（v2 主体）。
fn seal_with(key: &[u8; 32], plain: &[u8]) -> Vec<u8> {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ct = cipher.encrypt(&nonce, plain).expect("aes-gcm encrypt");
    let mut out = Vec::with_capacity(MAGIC_V2.len() + 12 + ct.len());
    out.extend_from_slice(MAGIC_V2);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    out
}

fn open_with(key: &[u8; 32], blob: &[u8]) -> Result<Vec<u8>, String> {
    if blob.len() < MAGIC_V2.len() + 12 + 16 || &blob[..MAGIC_V2.len()] != MAGIC_V2 {
        return Err("凭据文件格式不对".into());
    }
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let nonce = Nonce::from_slice(&blob[MAGIC_V2.len()..MAGIC_V2.len() + 12]);
    cipher
        .decrypt(nonce, &blob[MAGIC_V2.len() + 12..])
        .map_err(|_| "凭据文件无法解密（被篡改或密钥不匹配）".to_string())
}

/// 封装数据密钥：Windows 优先 DPAPI，失败或其它平台用机器绑定密钥。
pub fn wrap_key(key: &[u8; 32]) -> Vec<u8> {
    #[cfg(windows)]
    {
        match dpapi::protect(key) {
            Ok(blob) => {
                let mut out = vec![WRAP_DPAPI];
                out.extend_from_slice(&blob);
                return out;
            }
            Err(e) => log::warn!("[loov] DPAPI 不可用，改用机器绑定密钥: {e}"),
        }
    }
    let mut out = vec![WRAP_MACHINE];
    out.extend_from_slice(&seal(key));
    out
}

pub fn unwrap_key(blob: &[u8]) -> Result<[u8; 32], String> {
    let (tag, rest) = blob.split_first().ok_or("密钥文件为空")?;
    let raw = match *tag {
        WRAP_MACHINE => open(rest)?,
        #[cfg(windows)]
        WRAP_DPAPI => dpapi::unprotect(rest)?,
        #[cfg(not(windows))]
        WRAP_DPAPI => return Err("密钥由 Windows DPAPI 保护，本平台无法解开".into()),
        other => return Err(format!("未知的密钥封装方式 {other:#x}")),
    };
    raw.as_slice()
        .try_into()
        .map_err(|_| "密钥长度不对".to_string())
}

pub fn load() -> Result<Option<Credentials>, String> {
    let path = credentials_path();
    let blob = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("读取凭据失败：{e}")),
    };
    let plain = if blob.starts_with(MAGIC_V2) {
        let wrapped = std::fs::read(key_path()).map_err(|e| format!("读取凭据密钥失败：{e}"))?;
        open_with(&unwrap_key(&wrapped)?, &blob)?
    } else {
        // v1：机器密钥直接加密，读出后迁移到 v2
        open(&blob)?
    };
    let creds: Credentials = serde_json::from_slice(&plain).map_err(|e| format!("凭据内容损坏：{e}"))?;
    if !blob.starts_with(MAGIC_V2) {
        match save(&creds) {
            Ok(()) => log::info!("[loov] 凭据已迁移到 v2 格式"),
            Err(e) => log::warn!("[loov] 凭据迁移失败（继续使用旧格式）: {e}"),
        }
    }
    Ok(Some(creds))
}

pub fn save(creds: &Credentials) -> Result<(), String> {
    let path = credentials_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建目录失败：{e}"))?;
        restrict_dir(dir);
    }
    // 复用已有数据密钥（解不开就换一把新的）
    let key = std::fs::read(key_path())
        .ok()
        .and_then(|w| unwrap_key(&w).ok())
        .unwrap_or_else(random_key);
    let wrapped = wrap_key(&key);
    let key_tmp = key_path().with_extension("key.tmp");
    write_private(&key_tmp, &wrapped)?;
    std::fs::rename(&key_tmp, key_path()).map_err(|e| format!("写入凭据密钥失败：{e}"))?;

    // 审计 S1-02：真实 API Key 不落盘。macOS / Linux 的数据密钥由非秘密的机器 ID ‖ 用户名派生，
    // 同用户进程能解开这个文件；能拿走的只剩可吊销的设备令牌，Key 要在线向 hub 换。
    let mut on_disk = creds.clone();
    if let Some(b) = on_disk.bootstrap.as_mut() {
        b.api.key.clear();
        b.api.keys.clear();
    }
    let plain = serde_json::to_vec(&on_disk).map_err(|e| e.to_string())?;
    let blob = seal_with(&key, &plain);
    let tmp = path.with_extension("enc.tmp");
    write_private(&tmp, &blob)?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("写入凭据失败：{e}"))?;
    Ok(())
}

pub fn clear() -> Result<(), String> {
    for p in [credentials_path(), key_path()] {
        match std::fs::remove_file(&p) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("删除凭据失败：{e}")),
        }
    }
    Ok(())
}

#[cfg(windows)]
mod dpapi {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        // SAFETY: 成功返回时 pbData 指向 cbData 字节、由 LocalAlloc 分配的缓冲区
        let v = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec() };
        unsafe { LocalFree(out.pbData as _) };
        v
    }

    pub fn protect(data: &[u8]) -> Result<Vec<u8>, String> {
        let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
        let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
        // SAFETY: 输入缓冲区在调用期间有效；可选参数都传空
        let ok = unsafe {
            CryptProtectData(&input, std::ptr::null(), std::ptr::null(), std::ptr::null(), std::ptr::null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out)
        };
        if ok == 0 {
            return Err(format!("CryptProtectData 失败：{}", std::io::Error::last_os_error()));
        }
        Ok(take(out))
    }

    pub fn unprotect(data: &[u8]) -> Result<Vec<u8>, String> {
        let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
        let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
        // SAFETY: 同上
        let ok = unsafe {
            CryptUnprotectData(&input, std::ptr::null_mut(), std::ptr::null(), std::ptr::null(), std::ptr::null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out)
        };
        if ok == 0 {
            return Err(format!("CryptUnprotectData 失败（换了 Windows 用户？）：{}", std::io::Error::last_os_error()));
        }
        Ok(take(out))
    }
}

pub(crate) fn write_private(path: &std::path::Path, data: &[u8]) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| format!("写入 {} 失败：{e}", path.display()))?;
        f.write_all(data).map_err(|e| e.to_string())?;
        f.sync_all().ok();
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, data).map_err(|e| format!("写入 {} 失败：{e}", path.display()))
    }
}

pub(crate) fn restrict_dir(dir: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_open_roundtrip_and_tamper_detection() {
        let plain = br#"{"device_token":"ldt_x"}"#;
        let blob = seal(plain);
        assert_eq!(open(&blob).unwrap(), plain);
        let mut bad = blob.clone();
        let last = bad.len() - 1;
        bad[last] ^= 1;
        assert!(open(&bad).is_err());
        assert!(open(b"garbage").is_err());
    }

    #[test]
    fn save_load_clear_in_test_home() {
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("CC_SWITCH_TEST_HOME", tmp.path());
        let creds = Credentials {
            device_token: "ldt_abc".into(),
            username: "alice".into(),
            device_id: "0123456789abcdef".into(),
            bootstrap: None,
            cached_at: 1,
        };
        save(&creds).unwrap();
        assert_eq!(load().unwrap().unwrap(), creds);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(credentials_path()).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        let enc = std::fs::read(credentials_path()).unwrap();
        assert!(enc.starts_with(MAGIC_V2), "新写入应为 v2");
        assert!(key_path().exists());
        // 再存一次复用同一把数据密钥
        let k1 = std::fs::read(key_path()).unwrap();
        save(&creds).unwrap();
        assert_eq!(unwrap_key(&k1).unwrap(), unwrap_key(&std::fs::read(key_path()).unwrap()).unwrap());

        // v1 旧文件：能读出并自动迁移到 v2
        clear().unwrap();
        write_private(&credentials_path(), &seal(&serde_json::to_vec(&creds).unwrap())).unwrap();
        assert_eq!(load().unwrap().unwrap(), creds);
        assert!(std::fs::read(credentials_path()).unwrap().starts_with(MAGIC_V2), "应已迁移");
        assert_eq!(load().unwrap().unwrap(), creds);

        // 审计 S1-02：bootstrap 里的真实 Key 不落盘，其余字段照常缓存
        let with_key = Credentials {
            bootstrap: Some(serde_json::from_value(serde_json::json!({
                "user": {"username": "alice"},
                "device": {"id": "0123456789abcdef", "name": "pc"},
                "api": {"baseUrl": "https://me.loov.cc", "key": "sk-secret-key"},
                "tunnel": {"version": 1, "nodes": []},
                "models": ["claude-sonnet-5"]
            })).expect("bootstrap fixture")),
            ..creds.clone()
        };
        save(&with_key).unwrap();
        let loaded = load().unwrap().unwrap();
        let b = loaded.bootstrap.as_ref().unwrap();
        assert_eq!(b.api.key, "", "Key 不应落盘");
        assert_eq!(b.api.base_url, "https://me.loov.cc");
        assert_eq!(b.models, vec!["claude-sonnet-5".to_string()]);
        let wrapped = std::fs::read(key_path()).unwrap();
        let plain = open_with(&unwrap_key(&wrapped).unwrap(), &std::fs::read(credentials_path()).unwrap()).unwrap();
        assert!(!String::from_utf8_lossy(&plain).contains("sk-secret-key"));
        save(&creds).unwrap();

        // 密钥文件丢了：报错而不是悄悄当作未登录
        std::fs::remove_file(key_path()).unwrap();
        assert!(load().is_err());

        clear().unwrap();
        assert!(load().unwrap().is_none());
        assert!(!key_path().exists());
        std::env::remove_var("CC_SWITCH_TEST_HOME");
    }

    #[test]
    fn key_wrapping_roundtrip() {
        let k = random_key();
        let w = wrap_key(&k);
        if cfg!(windows) {
            assert_eq!(w[0], WRAP_DPAPI, "Windows 上应使用 DPAPI");
        } else {
            assert_eq!(w[0], WRAP_MACHINE);
        }
        assert_eq!(unwrap_key(&w).unwrap(), k);
        let mut bad = w.clone();
        let last = bad.len() - 1;
        bad[last] ^= 1;
        assert!(unwrap_key(&bad).is_err());
        assert!(unwrap_key(&[0x7f, 1, 2]).is_err());
    }
}
