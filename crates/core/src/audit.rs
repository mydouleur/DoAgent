//! 全局审计日志：`do.audit.jsonl`（exe 旁，与 do.config.json 同目录）
//!
//! # 模块导读
//! 记录三类事件：用户输入（input）、一轮对话完成（reply）、工具执行（tool）。
//! JSONL（JSON Lines）≈ C# NLog 的行格式目标：一行一条独立 JSON 记录，
//! 追加写、逐行解析，不需要任何数据库。不选 SQLite 的账：那会把单文件
//! 二进制从 2 MB 级推向 5 MB 级——审计只需要"追加一行"，JSONL 够用。
//!
//! # 为什么必须放工作区外
//! `.do/` 的隐形只是"代码级"（AI 的工具看不到它）；审计要的是**物理不可达**——
//! 放在 exe 旁，AI 的所有工具都被锁在工作区根内，够不到这个文件，
//! 也就无法伪造或擦除自己的操作记录。这才叫可审查。
//!
//! # 降级原则
//! 写失败静默（审计不能搞挂主流程）；exe_dir 不可用或文件不可写时
//! 整体降级关闭（`enabled() == false`），由调用方在启动时给一条提示。

use serde_json::{json, Value};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// 审计日志文件名（exe 同目录）
pub const AUDIT_FILE: &str = "do.audit.jsonl";

/// 审计写入器。`file: None` = 降级关闭状态（log 全成空操作）。
pub struct Audit {
    file: Option<File>,
    /// 工作区路径（每条记录的 ws 字段）
    ws: String,
}

impl Audit {
    /// 打开全局审计日志（exe 旁）；任何一步失败都降级为关闭。
    pub fn new(ws: &Path) -> Audit {
        let path = crate::config::exe_dir().map(|d| d.join(AUDIT_FILE));
        Audit::at(path, ws)
    }

    /// 指定路径打开（测试用）；`None` 或打不开即降级关闭
    pub fn at(path: Option<PathBuf>, ws: &Path) -> Audit {
        let file = path.and_then(|p| {
            OpenOptions::new().create(true).append(true).open(p).ok()
        });
        Audit {
            file,
            ws: ws.display().to_string(),
        }
    }

    /// 审计是否可用（启动时据此给用户提示）
    pub fn enabled(&self) -> bool {
        self.file.is_some()
    }

    /// 追加一条记录。File 无缓冲，write 即落 OS——
    /// 等价于"每条写完 flush"（≈ C# StreamWriter.AutoFlush = true）。
    /// 错误一律吞掉：审计失败不能搞挂主流程。
    pub fn log(&mut self, kind: &str, payload: Value) {
        let Some(f) = &mut self.file else { return };
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        // 平铺一条记录：先铺 payload 各字段，再写固定三字段——
        // serde_json 的 Map 同键后写覆盖先写，这个顺序保证 payload
        // 里即使夹带 ts/ws/kind 也覆盖不掉固定字段的真实值
        let mut rec = match payload {
            Value::Object(_) => payload,
            // 非 object 的 payload 丢弃（与旧行为一致：只有固定字段）
            _ => json!({}),
        };
        if let Value::Object(r) = &mut rec {
            r.insert("ts".into(), json!(ts));
            r.insert("ws".into(), json!(self.ws));
            r.insert("kind".into(), json!(kind));
        }
        let _ = writeln!(f, "{rec}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_kinds_each_one_json_line() {
        let dir = std::env::temp_dir().join(format!("doagent-audit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("audit.jsonl");
        let mut a = Audit::at(Some(path.clone()), Path::new("C:\\ws"));
        assert!(a.enabled());
        a.log("input", json!({"text": "改一下 main.rs"}));
        a.log("tool", json!({"name": "read", "args": "{}", "duration_ms": 3, "result": "...tail"}));
        a.log("reply", json!({"tokens": 1234}));
        drop(a); // 关文件再读

        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        // 每行都是合法 JSON，固定字段齐全
        let kinds: Vec<String> = lines
            .iter()
            .map(|l| {
                let v: Value = serde_json::from_str(l).unwrap();
                assert!(v["ts"].is_number() && v["ws"].is_string());
                v["kind"].as_str().unwrap().to_string()
            })
            .collect();
        assert_eq!(kinds, ["input", "tool", "reply"]);
        // 负载平铺进同一行
        let tool: Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(tool["name"], "read");
        assert_eq!(tool["duration_ms"], 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fixed_fields_cannot_be_overridden() {
        // payload 夹带 ts/ws/kind 时不得覆盖固定字段的真实值
        let dir = std::env::temp_dir().join(format!("doagent-audit2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("audit.jsonl");
        let mut a = Audit::at(Some(path.clone()), Path::new("C:\\ws"));
        a.log("tool", json!({"kind": "fake", "ws": "X:\\evil", "ts": 0, "name": "read"}));
        drop(a);
        let text = std::fs::read_to_string(&path).unwrap();
        let v: Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(v["kind"], "tool");
        assert_eq!(v["ws"], "C:\\ws");
        assert!(v["ts"].as_u64().unwrap() > 0);
        // payload 自有字段照常平铺
        assert_eq!(v["name"], "read");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unwritable_degrades_silently() {
        // 不存在的目录 → 打不开 → 降级关闭，log 不 panic
        let mut a = Audit::at(
            Some(PathBuf::from("Z:\\no\\such\\dir\\audit.jsonl")),
            Path::new("."),
        );
        assert!(!a.enabled());
        a.log("input", json!({"text": "x"})); // 静默，不炸
        // None（exe_dir 不可用的降级路径）同样安全
        let mut a = Audit::at(None, Path::new("."));
        a.log("reply", json!({"tokens": 1}));
    }
}
