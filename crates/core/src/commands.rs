//! 固定命令白名单：`.do/commands.json` 的读写与 name 校验
//!
//! # 模块导读
//! AI 的 shell 能力边界就这一张表：AI 通过 `propose_command` 工具**提案**
//! 固定命令（name + command + description + mode），人类在 TUI 里审批
//! （可改名再批）后才落盘到 `.do/commands.json`，此后该命令作为**零参数
//! 动态工具**出现在 tool schema 里，AI 调用时按审批的固定字符串执行。
//!
//! # 安全模型（红线）
//! - 审批的字符串 = 永远执行的全部内容：运行时零拼接、零参数、固定 cwd
//!   为工作区根——所以即便经 shell 包装执行也无注入面；
//! - 持久批准（落盘），不做逐次审批（防审批疲劳）；
//! - name 必须匹配 `^[a-zA-Z0-9_-]+$`，非法名在校验阶段就被拒绝；
//! - `.do/` 对 AI 隐形（见 workspace 模块），批准列表天然对 AI 不可见。

use serde::{Deserialize, Serialize};
use std::io;
use std::path::Path;

/// 一条已批准（或待批准）的固定命令。
/// 提案与落盘共用同一结构——审批通过 = 原样写入，不改一个字。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovedCommand {
    /// 工具名：`^[a-zA-Z0-9_-]+$`
    pub name: String,
    /// 固定执行串（人工审批的常量，运行时原样执行）
    pub command: String,
    /// 给模型看的工具描述
    pub description: String,
    /// `once` = 一次性（等结束、返回输出尾部）；
    /// `daemon` = 常驻（后台 spawn 立即返回）
    pub mode: String,
}

/// name 合法性：`^[a-zA-Z0-9_-]+$`。
/// 手写字符检查——这么小的规则不值得动正则
///（≈ C# 的 `s.All(char.IsLetterOrDigit)` 一行 LINQ）。
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// 读批准列表；文件不存在/损坏 → 空列表（"没批准过任何命令"是合法状态）。
/// 这是 core 自己人通道：直接读 `.do/`，不过 workspace 守卫。
pub fn load(root: &Path) -> Vec<ApprovedCommand> {
    let Ok(text) = std::fs::read_to_string(root.join(".do").join("commands.json")) else {
        return Vec::new();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

/// 写批准列表（`.do/` 目录不存在则先建）。
pub fn save(root: &Path, cmds: &[ApprovedCommand]) -> io::Result<()> {
    let dir = root.join(".do");
    std::fs::create_dir_all(&dir)?;
    let text = serde_json::to_string_pretty(cmds)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    std::fs::write(dir.join("commands.json"), text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("doagent-cmd-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn roundtrip() {
        let dir = temp_dir("rt");
        assert!(load(&dir).is_empty()); // 无文件 = 空列表
        let cmds = vec![ApprovedCommand {
            name: "dev".into(),
            command: "npm run dev".into(),
            description: "启动开发服务器".into(),
            mode: "daemon".into(),
        }];
        save(&dir, &cmds).unwrap();
        let back = load(&dir);
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].name, "dev");
        assert_eq!(back[0].command, "npm run dev");
        assert_eq!(back[0].mode, "daemon");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn name_validation() {
        assert!(valid_name("dev"));
        assert!(valid_name("build-prod_v2"));
        assert!(!valid_name("")); // 空
        assert!(!valid_name("my cmd")); // 空格
        assert!(!valid_name("a/b")); // 斜杠
        assert!(!valid_name("cmd;rm")); // 注入字符
        assert!(!valid_name("中文名")); // 非 ASCII
    }
}
