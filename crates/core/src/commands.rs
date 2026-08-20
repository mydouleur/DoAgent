//! 固定命令白名单：`.do/commands.json` 的读写与 name 校验
//!
//! # 模块导读
//! AI 的 shell 能力边界就这一张表：AI 通过 `addcmd` 工具**提案**
//! 固定命令（name + command + description + mode），人类在 TUI 里审批
//! （可编辑描述再批）后才落盘到 `.do/commands.json`。
//! 批准后的命令**不进 tools 数组**——发现式注入：AI 用 runcmd 无参
//! 列出白名单、带 name 现读现执行。tools 因此冻结为 7 个内建工具，
//! prompt 缓存前缀稳定，批准动作零缓存代价（论证见 agent.rs
//! chat_round 的"缓存论点"注释）。
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

/// 全局层文件名（exe 同目录，与 do.config.json 并排）
pub const GLOBAL_FILE: &str = "do.commands.json";

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
    /// 待批期间的目标层标记：true = 批准到全局层（/addcmd -g）。
    /// `#[serde(skip)]`：不进落盘 JSON——层归属由文件位置表达，不入数据
    #[serde(skip)]
    pub global: bool,
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

/// 读工作区层批准列表；文件不存在/损坏 → 空列表。
/// 这是 core 自己人通道：直接读 `.do/`，不过 workspace 守卫。
pub fn load(root: &Path) -> Vec<ApprovedCommand> {
    let Ok(text) = std::fs::read_to_string(root.join(".do").join("commands.json")) else {
        return Vec::new();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

/// 写工作区层批准列表（`.do/` 目录不存在则先建）。
pub fn save(root: &Path, cmds: &[ApprovedCommand]) -> io::Result<()> {
    let dir = root.join(".do");
    std::fs::create_dir_all(&dir)?;
    let text = serde_json::to_string_pretty(cmds)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    std::fs::write(dir.join("commands.json"), text)
}

/// 读全局层批准列表（exe 旁 do.commands.json）。
/// 全局层在工作区之外，对 AI 物理不可达——天然满足隐形。
pub fn load_global(exe_dir: &Path) -> Vec<ApprovedCommand> {
    let Ok(text) = std::fs::read_to_string(exe_dir.join(GLOBAL_FILE)) else {
        return Vec::new();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

/// 写全局层批准列表
pub fn save_global(exe_dir: &Path, cmds: &[ApprovedCommand]) -> io::Result<()> {
    let text = serde_json::to_string_pretty(cmds)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    std::fs::write(exe_dir.join(GLOBAL_FILE), text)
}

/// 来源层：一条已批准命令来自哪一层。
/// 层语义用枚举表达，不用"工作区"/"全局"字符串——TUI 曾拿中文字面量
/// 做逻辑分支（core 改措辞就会静默写错层），且违反 i18n 约定。
/// 展示文案不归 core 管：TUI 侧按界面语言映射（lang.rs），
/// 喂模型的文本由 [`Layer::for_model`] 固定中文（协议内容不走 i18n）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    /// 工作区层（`.do/commands.json`）
    Workspace,
    /// 全局层（exe 旁 `do.commands.json`）
    Global,
}

impl Layer {
    /// 模型可见文本里的层标注（协议内容，固定中文，不走 TUI 的 lang 表）
    pub fn for_model(&self) -> &'static str {
        match self {
            Layer::Workspace => "工作区",
            Layer::Global => "全局",
        }
    }
}

/// 合并视图：工作区层 + 全局层，重名工作区赢；每条带来源层标注。
/// 元组 ≈ C# 的 (T, TLayer) ValueTuple——轻量捆绑，不值得单建类型。
pub fn merged(root: &Path, exe_dir: Option<&Path>) -> Vec<(ApprovedCommand, Layer)> {
    let mut out: Vec<(ApprovedCommand, Layer)> = load(root)
        .into_iter()
        .map(|c| (c, Layer::Workspace))
        .collect();
    for c in exe_dir.map(load_global).unwrap_or_default() {
        if !out.iter().any(|(w, _)| w.name == c.name) {
            out.push((c, Layer::Global));
        }
    }
    out
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
            global: false,
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

    fn cmd(name: &str) -> ApprovedCommand {
        ApprovedCommand {
            name: name.into(),
            command: format!("echo {name}"),
            description: String::new(),
            mode: "once".into(),
            global: false,
        }
    }

    #[test]
    fn merged_view_workspace_wins_with_source_labels() {
        let (ws, exe) = (temp_dir("mw"), temp_dir("me"));
        save(&ws, &[cmd("a"), cmd("shared")]).unwrap();
        let mut gshared = cmd("shared");
        gshared.command = "echo shared-global".into(); // 同名不同串，区分谁赢
        save_global(&exe, &[gshared, cmd("g")]).unwrap();
        // 落盘 JSON 不含 global 标记字段（层归属由文件位置表达）
        let text = std::fs::read_to_string(ws.join(".do/commands.json")).unwrap();
        assert!(!text.contains("global"));

        let view = merged(&ws, Some(&exe));
        let names: Vec<&str> = view.iter().map(|(c, _)| c.name.as_str()).collect();
        assert_eq!(names, ["a", "shared", "g"]); // 重名只出现一次
        // 重名工作区赢：shared 的 command 是工作区层那一条
        let shared = view.iter().find(|(c, _)| c.name == "shared").unwrap();
        assert_eq!(shared.0.command, "echo shared");
        assert_eq!(shared.1, Layer::Workspace);
        assert_eq!(view.iter().find(|(c, _)| c.name == "g").unwrap().1, Layer::Global);
        // exe_dir 不可用 → 只工作区层
        assert_eq!(merged(&ws, None).len(), 2);
        let _ = (std::fs::remove_dir_all(&ws), std::fs::remove_dir_all(&exe));
    }
}
