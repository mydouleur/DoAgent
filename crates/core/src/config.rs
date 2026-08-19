//! 配置：两层覆盖合并
//!
//! # 模块导读
//! 配置分两层，按优先级覆盖合并：
//! 1. **工作区层** `.do/config.json`（项目级，对 AI 代码级隐形）——
//!    存 `url`/`key`/`model` 的项目级覆盖；
//! 2. **全局便携层** `do.config.json`，放在 **do.exe 同目录**（对 AI 物理不可达，
//!    因为它在工作区之外）——存 `url`/`key`/`model`/`lang` 这些"人的身份"。
//!
//! `lang` 字段仍在 JSON 里，但只由 `/lang` 命令写入，/setting 不管它。
//!
//! 合并规则：**工作区非空字段 > 全局非空字段 > 内置默认**（url 默认
//! https://api.openai.com/v1）。
//!
//! # 核心概念
//! - `std::env::current_exe()`：拿到当前 exe 的绝对路径——这是"绿色软件"
//!   理念的关键（≈ C# 的 `Assembly.Location`）：配置跟着 exe 走，
//!   整个文件夹拷走即用、删掉即完全卸载，不写注册表/AppData。
//! - 全局路径**不写死**在本模块里：加载/保存都接受路径参数，
//!   测试用临时目录喂进来即可（可测试性 = 把 IO 边界做成参数）。

use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

/// 全局层文件名（exe 同目录）
pub const GLOBAL_FILE: &str = "do.config.json";

/// 配置项，缺哪项补哪项。
/// derive 宏：`#[derive(...)]` 让编译器自动生成 trait 实现——
/// Serialize/Deserialize ≈ C# 的 [Serializable] + JsonConverter，但零运行时反射。
/// 没有 deny_unknown_fields：旧配置里的已删字段（如 start）读取时被忽略，
/// 向后兼容不炸。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)] // 反序列化时缺失字段用 Default 填充（读旧版配置不炸）
pub struct Config {
    /// API 地址，缺省 https://api.openai.com/v1
    pub url: String,
    /// Bearer 密钥
    pub key: String,
    /// 模型名
    pub model: String,
    /// 界面语言：zh / en（默认 en；身份项；只由 /lang 命令写入）
    pub lang: String,
}

/// do.exe 所在目录（全局层的安放处）。
/// 失败时返回 None 而不是 panic——调用方据此优雅降级为只用工作区层。
pub fn exe_dir() -> Option<PathBuf> {
    // `?` 用在 Option 上：None 直接提前返回 None（? 同时服务于 Result 和 Option）
    let exe = std::env::current_exe().ok()?;
    exe.parent().map(|p| p.to_path_buf())
}

/// 按优先级取第一个非空值（工作区 > 全局 > 默认）。
/// fold ≈ C# LINQ 的 Aggregate：从空串开始滚动累积，
/// 已拿到非空值就不再让位——空串是"还没决定"的中性元。
fn pick(candidates: [String; 3]) -> String {
    candidates
        .into_iter()
        .fold(String::new(), |acc, s| if acc.is_empty() { s } else { acc })
}

impl Config {
    /// 带缺省值的构造（仅 url 有内置默认）
    pub fn with_defaults() -> Config {
        Config {
            url: "https://api.openai.com/v1".to_string(),
            ..Config::default() // `..` 结构体更新语法：其余字段取默认值
        }
    }

    /// 读任意一个层文件；文件不存在或损坏时返回**全空**层。
    /// 注意连 url 都是空——内置默认只在合并时才补，这样层文件里
    /// 才不会被"顺带烤进"默认值，挡住全局层生效。
    pub fn load_layer(path: &Path) -> Config {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Config::default();
        };
        // unwrap_or_else ≈ C# 的 `??`：解析失败也回落到全空层
        serde_json::from_str(&text).unwrap_or_else(|_| Config::default())
    }

    /// 工作区层：`.do/config.json`
    pub fn load_workspace(root: &Path) -> Config {
        Self::load_layer(&root.join(".do").join("config.json"))
    }

    /// 全局便携层：`{exe_dir}/do.config.json`
    pub fn load_global(exe_dir: &Path) -> Config {
        Self::load_layer(&exe_dir.join(GLOBAL_FILE))
    }

    /// 合并后的生效配置：工作区非空 > 全局非空 > 内置默认。
    /// `exe_dir` 为 None（current_exe 失败）时优雅降级为两层。
    pub fn load_merged(root: &Path, exe_dir: Option<&Path>) -> Config {
        let ws = Self::load_workspace(root);
        let global = exe_dir.map(Self::load_global).unwrap_or_default();
        let def = Config::with_defaults();
        Config {
            url: pick([ws.url, global.url, def.url]),
            key: pick([ws.key, global.key, def.key]),
            model: pick([ws.model, global.model, def.model]),
            lang: pick([ws.lang, global.lang, def.lang]),
        }
    }

    /// 写工作区层 `.do/config.json`（目录不存在则先建）。
    pub fn save(&self, root: &Path) -> io::Result<()> {
        let dir = root.join(".do");
        std::fs::create_dir_all(&dir)?;
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        std::fs::write(dir.join("config.json"), text)
    }

    /// 写全局便携层——只落 url/key/model/lang 身份项。
    pub fn save_global(&self, exe_dir: &Path) -> io::Result<()> {
        let text = serde_json::to_string_pretty(&serde_json::json!({
            "url": self.url,
            "key": self.key,
            "model": self.model,
            "lang": self.lang,
        }))
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        std::fs::write(exe_dir.join(GLOBAL_FILE), text)
    }

    /// 修改某一项；字段名不合法时返回错误文案（给 /setting 用）。
    /// lang 不在这里改——/lang 是唯一切换入口（直接写字段，不走 set）。
    pub fn set(&mut self, field: &str, value: &str) -> Result<(), String> {
        // `&mut self`：可变借用，≈ C# 里直接改 this，但 Rust 保证
        // 同一时刻只有这一个可变引用存在——编译期的"锁"。
        match field {
            "url" => self.url = value.to_string(),
            "key" => self.key = value.to_string(),
            "model" => self.model = value.to_string(),
            "lang" => return Err("lang 请用 /lang 切换（/lang zh|en）".to_string()),
            _ => return Err(format!("未知配置项 {field}（可选: url/key/model）")),
        }
        Ok(())
    }

    /// 修改全局层的某一项（与 set 同规则；保留入口仅为 /setting -g 语义对称）。
    pub fn set_global(&mut self, field: &str, value: &str) -> Result<(), String> {
        self.set(field, value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("doagent-cfg-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn merge_workspace_overrides_global() {
        let (ws, exe) = (temp_dir("m1w"), temp_dir("m1e"));
        // 两层都设了 model：工作区赢
        let mut g = Config::default();
        g.set("model", "global-model").unwrap();
        g.set("key", "global-key").unwrap();
        g.save_global(&exe).unwrap();
        let mut w = Config::default();
        w.set("model", "ws-model").unwrap();
        w.save(&ws).unwrap();

        let merged = Config::load_merged(&ws, Some(&exe));
        assert_eq!(merged.model, "ws-model"); // 工作区覆盖全局
        assert_eq!(merged.key, "global-key"); // 全局兜底
        assert_eq!(merged.url, "https://api.openai.com/v1"); // 默认兜底
        let _ = (std::fs::remove_dir_all(&ws), std::fs::remove_dir_all(&exe));
    }

    #[test]
    fn merge_default_fallback_and_no_global() {
        let ws = temp_dir("m2w");
        // 两层都没有：全部回落内置默认 / 空
        let merged = Config::load_merged(&ws, None); // None = current_exe 失败的降级路径
        assert_eq!(merged.url, "https://api.openai.com/v1");
        assert!(merged.key.is_empty() && merged.model.is_empty() && merged.lang.is_empty());
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn legacy_start_key_ignored() {
        // 向后兼容：旧配置里的 start 键被静默忽略，不炸
        let ws = temp_dir("m3w");
        std::fs::create_dir_all(ws.join(".do")).unwrap();
        std::fs::write(
            ws.join(".do/config.json"),
            "{\"start\": \"cargo build\", \"model\": \"m\"}",
        )
        .unwrap();
        let cfg = Config::load_workspace(&ws);
        assert_eq!(cfg.model, "m");
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn set_rejects_start_and_lang() {
        let mut g = Config::default();
        // start 已连根拔：落入未知字段提示
        assert!(g.set("start", "x").unwrap_err().contains("未知配置项"));
        assert!(g.set_global("start", "x").is_err());
        // lang 不走 /setting：提示用 /lang
        assert!(g.set("lang", "zh").unwrap_err().contains("/lang"));
        assert!(g.set("model", "gpt-4o").is_ok());
        assert_eq!(g.model, "gpt-4o");
    }

    #[test]
    fn lang_roundtrips_global_layer() {
        let exe = temp_dir("lang");
        // /lang 直接写字段，不走 set（结构体更新语法 ≈ C# 的对象初始化器）
        let c = Config { lang: "zh".to_string(), ..Config::default() };
        c.save_global(&exe).unwrap();
        let back = Config::load_global(&exe);
        assert_eq!(back.lang, "zh");
        assert!(std::fs::read_to_string(exe.join(GLOBAL_FILE)).unwrap().contains("\"lang\""));
        let _ = std::fs::remove_dir_all(&exe);
    }
}
