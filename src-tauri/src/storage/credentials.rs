//! credentials —— API Key 的加密落盘（Windows DPAPI）
//!
//! 背景：settings.json 曾直接持有各家 api_key 的明文，只靠数据目录的 ACL 兜底。
//! 那份明文一旦随同步盘、备份、整目录拷贝离开本机，就等于泄露。本模块把密钥从
//! 设置文件里抽出来单独存一份 `credentials.json`，值用 Windows 的 DPAPI
//! （`CryptProtectData`）加密：密文与"当前用户 + 本机"绑定，拷到别的机器或别的
//! Windows 账户都解不开。换机需重填是这套选择的必然结果（没有账户系统时的唯一
//! 自洽方案），不是缺陷。
//!
//! 设计要点：
//! - **引用键**（`llm.providers.<name>.api_key`）标识条目，settings 侧只留空值，
//!   由本层承接钥值（"配置只带引用"）。
//! - 值一律带 `dpapi:v1:` 前缀 + 十六进制密文。缺前缀的条目一律跳过——fail loud，
//!   不让手写的明文混进来冒充密文。
//! - 内存里持明文（`reveal_api_key`、`resolve_active` 都要用），与改造前一致，
//!   不引入新的内存暴露面。
//! - 落盘沿用 settings.rs 同款策略：tmp + rename 原子替换 + **写后重读校验**。
//! - 文件损坏 → 备份 `credentials.json.bak` 后回空并留下说明，**绝不静默覆盖**
//!   （同 settings.rs 的 parse-fail 处置）。
//! - 盘上 version 高于当前 → 只读降级（绝不覆盖写高版本数据）。
//!
//! 非 Windows 目标：加密/解密走桩（一律报错），文件读写仍可编译通过——与
//! capture.rs、exec/spawn_win32.rs 的 cfg 门控同构。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 凭据文件名（data_dir 下）。
pub const CREDENTIALS_FILE: &str = "credentials.json";
const CREDENTIALS_TMP: &str = "credentials.json.tmp";
const CREDENTIALS_BAK: &str = "credentials.json.bak";

/// 盘上结构版本。**结构性破坏才升**；新增引用键类型不升。
/// 高于当前 → 只读降级（旧程序不覆盖写新格式）。
pub const CREDENTIALS_VERSION: u32 = 1;

/// 密文值前缀（算法标识 + 版本）。缺了它的一律视为非法值。
const CIPHER_PREFIX: &str = "dpapi:v1:";

/// DPAPI 附加熵：掺入应用专属盐，通用的 DPAPI 解密工具即便拿到文件也不好直接解。
/// **改它的值等于作废所有已存密钥**，故把版本写进盐里。
const ENTROPY: &[u8] = b"prts-terminal/credentials/v1";

/// 供应商 key 在本层的引用键（单一来源，别处不要重新拼）。
pub fn provider_key_ref(provider: &str) -> String {
    format!("llm.providers.{provider}.api_key")
}

// ═══════════════════════════════════════════════════════
// 盘上结构
// ═══════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CredentialsFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    secrets: HashMap<String, String>,
}

impl Default for CredentialsFile {
    fn default() -> Self {
        Self {
            version: CREDENTIALS_VERSION,
            secrets: HashMap::new(),
        }
    }
}

// ═══════════════════════════════════════════════════════
// DPAPI 封装
// ═══════════════════════════════════════════════════════

/// 用当前用户凭据加密一段明文，返回 DPAPI 密文块。
#[cfg(windows)]
fn protect(plain: &[u8]) -> Result<Vec<u8>, String> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{
        CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    let input = CRYPT_INTEGER_BLOB {
        cbData: plain.len() as u32,
        pbData: plain.as_ptr() as *mut u8,
    };
    let entropy = CRYPT_INTEGER_BLOB {
        cbData: ENTROPY.len() as u32,
        pbData: ENTROPY.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptProtectData(
            &input,
            PCWSTR::null(),
            Some(&entropy),
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
        .map_err(|e| format!("密钥加密失败：{e}"))?;
        let blob = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        // output.pbData 由 DPAPI 用 LocalAlloc 分配，必须自己释放。
        let _ = LocalFree(Some(HLOCAL(output.pbData as *mut core::ffi::c_void)));
        Ok(blob)
    }
}

/// 解密一段 DPAPI 密文块。换机、换用户、换熵都会失败。
#[cfg(windows)]
fn unprotect(blob: &[u8]) -> Result<Vec<u8>, String> {
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{
        CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    if blob.is_empty() {
        return Err("密文块为空".to_string());
    }
    let input = CRYPT_INTEGER_BLOB {
        cbData: blob.len() as u32,
        pbData: blob.as_ptr() as *mut u8,
    };
    let entropy = CRYPT_INTEGER_BLOB {
        cbData: ENTROPY.len() as u32,
        pbData: ENTROPY.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptUnprotectData(
            &input,
            None,
            Some(&entropy),
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
        .map_err(|e| format!("密钥解密失败（密钥与本机账户绑定，换机后需重新填写）：{e}"))?;
        let plain = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(output.pbData as *mut core::ffi::c_void)));
        Ok(plain)
    }
}

#[cfg(not(windows))]
fn protect(_plain: &[u8]) -> Result<Vec<u8>, String> {
    Err("密钥加密仅在 Windows 上可用。".to_string())
}

#[cfg(not(windows))]
fn unprotect(_blob: &[u8]) -> Result<Vec<u8>, String> {
    Err("密钥解密仅在 Windows 上可用。".to_string())
}

// ═══════════════════════════════════════════════════════
// 十六进制编解码（不引依赖；密文块几十到几百字节，体积无所谓）
// ═══════════════════════════════════════════════════════

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(s.len() / 2);
    for pair in bytes.chunks(2) {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out.push(((hi << 4) | lo) as u8);
    }
    Some(out)
}

// ═══════════════════════════════════════════════════════
// 存储
// ═══════════════════════════════════════════════════════

/// 凭据存储：内存持明文，落盘持密文。
///
/// 与 settings.rs 的 `SettingsStore` 分工一致——本层只负责"引用键 → 明文"的
/// 存取与加密落盘，不参与设置语义（谁是 active、掩码怎么下发由命令层决定）。
pub struct CredentialsStore {
    path: PathBuf,
    /// 引用键 → 明文值。
    secrets: HashMap<String, String>,
    /// 盘上 version 高于当前 → 只读降级。
    readonly: bool,
    /// 载入时的处置说明（损坏回退、非法条目跳过等），供调用方记录与上报。
    pub load_note: Option<String>,
}

impl CredentialsStore {
    /// 从数据根载入。文件缺失 = 空存储；**不建目录**（首次保存时才建）。
    pub fn load(data_dir: &Path) -> Self {
        let mut store = Self {
            path: data_dir.join(CREDENTIALS_FILE),
            secrets: HashMap::new(),
            readonly: false,
            load_note: None,
        };

        let raw = match std::fs::read_to_string(&store.path) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return store,
            Err(e) => {
                store.load_note = Some(format!("凭据文件读取失败（{e}），按空存储继续"));
                return store;
            }
        };

        let parsed: CredentialsFile = match serde_json::from_str(raw.trim_start_matches('\u{feff}'))
        {
            Ok(p) => p,
            Err(e) => {
                // 损坏：先留备份再回空——绝不静默覆盖看不懂的内容。
                let _ = std::fs::copy(&store.path, data_dir.join(CREDENTIALS_BAK));
                store.load_note = Some(format!(
                    "凭据文件解析失败（{e}），已备份为 {CREDENTIALS_BAK} 并回空"
                ));
                return store;
            }
        };

        if parsed.version > CREDENTIALS_VERSION {
            store.readonly = true;
            store.load_note = Some(format!(
                "凭据文件 version={} 高于当前（{CREDENTIALS_VERSION}），只读降级",
                parsed.version
            ));
            return store;
        }

        // 逐条解密。跳过非法条目而不是整盘失败：一条坏值不该毁掉其余可用的密钥。
        let mut skipped: Vec<String> = Vec::new();
        for (key, value) in parsed.secrets {
            let Some(b64) = value.strip_prefix(CIPHER_PREFIX) else {
                skipped.push(format!(
                    "{key}（值缺少 {CIPHER_PREFIX} 前缀，疑似手写明文）"
                ));
                continue;
            };
            let Some(blob) = hex_decode(b64) else {
                skipped.push(format!("{key}（密文不是合法十六进制）"));
                continue;
            };
            match unprotect(&blob).and_then(|p| {
                String::from_utf8(p).map_err(|_| "解出的明文不是合法 UTF-8".to_string())
            }) {
                Ok(plain) if !plain.is_empty() => {
                    store.secrets.insert(key, plain);
                }
                Ok(_) => skipped.push(format!("{key}（解出的明文为空）")),
                Err(e) => skipped.push(format!("{key}（{e}）")),
            }
        }
        if !skipped.is_empty() {
            store.load_note = Some(format!(
                "跳过 {} 条凭据：{}",
                skipped.len(),
                skipped.join("；")
            ));
        }
        store
    }

    /// 引用键是否有非空值。
    pub fn has(&self, key: &str) -> bool {
        self.secrets
            .get(key)
            .map(|v| !v.is_empty())
            .unwrap_or(false)
    }

    /// 取明文值。
    pub fn get(&self, key: &str) -> Option<&str> {
        self.secrets.get(key).map(|s| s.as_str())
    }

    /// 写入明文（**只改内存**，落盘走 [`save`]）。空串 = 删除该条。
    ///
    /// 这里立刻做一次加密，让"加密失败"在写入口就暴露：失败则内存不变，
    /// 不会出现"看着存上了、其实存不下"的中间态。
    pub fn set(&mut self, key: &str, plaintext: &str) -> Result<(), String> {
        if self.readonly {
            return Err(format!(
                "凭据文件 version 高于当前（{CREDENTIALS_VERSION}），拒绝写入（需升级应用）"
            ));
        }
        if plaintext.is_empty() {
            self.secrets.remove(key);
            return Ok(());
        }
        protect(plaintext.as_bytes())?;
        self.secrets.insert(key.to_string(), plaintext.to_string());
        Ok(())
    }

    /// 条目数（测试与诊断用）。
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.secrets.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.secrets.is_empty()
    }

    /// 只读降级中（高版本文件）。
    #[allow(dead_code)]
    pub fn is_readonly(&self) -> bool {
        self.readonly
    }

    /// 整写：全部条目重新加密 + tmp/rename 原子替换 + 写后重读校验。
    /// 失败 Err 上抛；内存已同步，盘保持旧值。
    pub fn save(&self) -> Result<(), String> {
        if self.readonly {
            return Err(format!(
                "凭据文件 version 高于当前（{CREDENTIALS_VERSION}），拒绝写入（需升级应用）"
            ));
        }
        let mut secrets = HashMap::with_capacity(self.secrets.len());
        for (key, plain) in &self.secrets {
            let blob = protect(plain.as_bytes())?;
            secrets.insert(key.clone(), format!("{CIPHER_PREFIX}{}", hex_encode(&blob)));
        }
        let file = CredentialsFile {
            version: CREDENTIALS_VERSION,
            secrets,
        };
        let text =
            serde_json::to_string_pretty(&file).map_err(|e| format!("凭据序列化失败：{e}"))?;

        let dir = self
            .path
            .parent()
            .ok_or_else(|| "凭据文件路径无父目录".to_string())?;
        std::fs::create_dir_all(dir).map_err(|e| format!("凭据目录创建失败：{e}"))?;

        let tmp = self.path.with_file_name(CREDENTIALS_TMP);
        std::fs::write(&tmp, &text).map_err(|e| format!("凭据临时文件写入失败：{e}"))?;
        std::fs::rename(&tmp, &self.path).map_err(|e| format!("凭据文件替换失败：{e}"))?;

        // 写后重读校验（同 settings.rs）：读回不一致即报错，绝不当作写成功。
        let back =
            std::fs::read_to_string(&self.path).map_err(|e| format!("凭据文件回读失败：{e}"))?;
        if back.trim() != text.trim() {
            return Err("凭据文件写后校验失败（回读内容与写入不一致）".to_string());
        }
        Ok(())
    }

    /// 迁移辅助：把一批（引用键, 明文）并入内存并落盘。任一条加密失败即整体失败。
    pub fn merge_and_save<'a, I>(&mut self, entries: I) -> Result<(), String>
    where
        I: IntoIterator<Item = (&'a str, &'a str)>,
    {
        for (key, plain) in entries {
            self.set(key, plain)?;
        }
        self.save()
    }
}

// ═══════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("prts-cred-{tag}-{nanos}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn hex_roundtrip() {
        for case in [
            vec![],
            vec![0u8],
            vec![255u8, 0, 128],
            (0u8..=255).collect(),
        ] {
            let enc = hex_encode(&case);
            assert_eq!(hex_decode(&enc).unwrap(), case);
        }
        assert!(hex_decode("abc").is_none(), "奇数字符串非法");
        assert!(hex_decode("zz").is_none(), "非十六进制字符非法");
    }

    #[test]
    fn provider_key_ref_shape() {
        assert_eq!(
            provider_key_ref("deepseek"),
            "llm.providers.deepseek.api_key"
        );
        assert_eq!(
            provider_key_ref("my-proxy"),
            "llm.providers.my-proxy.api_key"
        );
    }

    #[test]
    fn missing_file_loads_empty() {
        let dir = unique_temp_dir("missing");
        let store = CredentialsStore::load(&dir);
        assert!(store.is_empty());
        assert!(!store.has(&provider_key_ref("deepseek")));
        assert!(store.load_note.is_none());
        // 载入不建文件
        assert!(!dir.join(CREDENTIALS_FILE).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_file_is_backed_up_and_starts_empty() {
        let dir = unique_temp_dir("corrupt");
        std::fs::write(dir.join(CREDENTIALS_FILE), "{ not json").unwrap();
        let store = CredentialsStore::load(&dir);
        assert!(store.is_empty());
        assert!(store.load_note.is_some());
        assert!(
            dir.join(CREDENTIALS_BAK).exists(),
            "损坏内容必须先备份再回空"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn version_too_high_degrades_to_readonly() {
        let dir = unique_temp_dir("highver");
        std::fs::write(
            dir.join(CREDENTIALS_FILE),
            format!(
                r#"{{"version": {}, "secrets": {{}}}}"#,
                CREDENTIALS_VERSION + 1
            ),
        )
        .unwrap();
        let mut store = CredentialsStore::load(&dir);
        assert!(store.is_readonly());
        assert!(store.set(&provider_key_ref("deepseek"), "sk-x").is_err());
        assert!(store.save().is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    mod windows_io {
        use super::*;

        #[test]
        fn set_save_load_roundtrip_and_no_plaintext_on_disk() {
            let dir = unique_temp_dir("roundtrip");
            let key = provider_key_ref("deepseek");
            let secret = "sk-this-is-a-test-key-value-0123456789";

            let mut store = CredentialsStore::load(&dir);
            store.set(&key, secret).unwrap();
            store.save().unwrap();

            // 明文绝不出现在盘上
            let text = std::fs::read_to_string(dir.join(CREDENTIALS_FILE)).unwrap();
            assert!(!text.contains(secret), "凭据文件里出现了明文密钥：{text}");
            assert!(text.contains(CIPHER_PREFIX), "值应带 dpapi 前缀");

            // 重新载入应解出同一明文
            let reloaded = CredentialsStore::load(&dir);
            assert_eq!(reloaded.get(&key), Some(secret));
            assert!(reloaded.has(&key));
            assert!(reloaded.load_note.is_none());

            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn empty_value_removes_entry() {
            let dir = unique_temp_dir("empty");
            let key = provider_key_ref("kimi");
            let mut store = CredentialsStore::load(&dir);
            store.set(&key, "sk-kimi").unwrap();
            assert!(store.has(&key));
            store.set(&key, "").unwrap();
            assert!(!store.has(&key));
            assert_eq!(store.len(), 0);
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn hand_written_plaintext_entry_is_skipped() {
            let dir = unique_temp_dir("plain");
            std::fs::write(
                dir.join(CREDENTIALS_FILE),
                r#"{"version": 1, "secrets": {"llm.providers.x.api_key": "sk-plaintext"}}"#,
            )
            .unwrap();
            let store = CredentialsStore::load(&dir);
            assert!(store.is_empty(), "缺前缀的明文条目必须被跳过");
            assert!(store.load_note.is_some());
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn undecryptable_value_is_skipped_with_note() {
            let dir = unique_temp_dir("badcipher");
            std::fs::write(
                dir.join(CREDENTIALS_FILE),
                r#"{"version": 1, "secrets": {"llm.providers.y.api_key": "dpapi:v1:deadbeef"}}"#,
            )
            .unwrap();
            let store = CredentialsStore::load(&dir);
            assert!(store.is_empty());
            assert!(store.load_note.is_some());
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn merge_and_save_writes_all_entries() {
            let dir = unique_temp_dir("merge");
            let mut store = CredentialsStore::load(&dir);
            store
                .merge_and_save([
                    (provider_key_ref("deepseek").as_str(), "sk-a"),
                    (provider_key_ref("zhipu").as_str(), "sk-b"),
                ])
                .unwrap();
            let reloaded = CredentialsStore::load(&dir);
            assert_eq!(reloaded.get(&provider_key_ref("deepseek")), Some("sk-a"));
            assert_eq!(reloaded.get(&provider_key_ref("zhipu")), Some("sk-b"));
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
