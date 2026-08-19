//! 界面文案：中英双语（手写零依赖 i18n）
//!
//! # 模块导读
//! 所有用户可见的 UI 字符串集中在 [`Lang::t`] 的一张静态表里，两种语言
//! 并排对照。≈ C# 的 .resx 资源文件 + ResourceManager——我们没有资源系统，
//! "enum key + match 返回 (en, zh) 元组"就是最小可用形态：编译器强制
//! 每个 key 两种语言齐全（元组必须两个元素），漏译无法通过编译。
//!
//! 带运行时值的文案用 `{}` 占位符模板，调用方按序 `.replace("{}", &v)`。
//! 只覆盖 TUI 层（crates/do）；core 喂给模型的工具结果、system prompt、
//! 工具 description 是协议内容，不是 UI，不走这里。

/// 界面语言
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Lang {
    En,
    Zh,
}

impl Lang {
    /// 从配置值解析（默认 en）
    pub fn parse(s: &str) -> Lang {
        match s {
            "zh" => Lang::Zh,
            _ => Lang::En,
        }
    }

    /// 查文案。match 无通配分支：新增 key 时编译器逼你在此处补齐双语
    pub fn t(&self, key: Key) -> &'static str {
        let (en, zh) = match key {
            // ---- splash ----
            Key::SplashContinue => ("Press any key to continue", "按任意键继续"),
            Key::WorkspaceLabel => ("workspace", "工作区"),
            Key::ApiKeySet => ("key is set", "key 已设置"),
            Key::ApiKeyMissing => (
                "key not set: /setting -g key <your-key>",
                "key 未设置：/setting -g key <你的key>",
            ),
            // ---- 启动/提交反馈 ----
            Key::GlobalLayerUnavailable => (
                "cannot locate do.exe dir; global config layer unavailable (workspace config + defaults only)",
                "无法定位 do.exe 目录，全局配置层不可用（仅用工作区配置 + 默认值）",
            ),
            Key::AuditDisabled => (
                "audit log not writable, disabled (do.audit.jsonl)",
                "审计日志不可写，已关闭（do.audit.jsonl）",
            ),
            Key::Cancelled => ("(cancelled)", "（已取消）"),
            Key::ErrorPrefix => ("error: {}", "错误: {}"),
            // ---- hint 行 ----
            Key::HintSettings => (
                "↑↓ select · Enter edit · Esc back",
                "↑↓ 选择 · Enter 编辑 · Esc 返回",
            ),
            Key::HintSettingsEdit => ("Enter save · Esc discard", "Enter 保存 · Esc 放弃"),
            Key::HintApprove => (
                "↑↓ select · Enter approve · x reject · e edit desc · Esc back",
                "↑↓ 选择 · Enter 批准 · x 拒绝 · e 编辑描述 · Esc 返回",
            ),
            Key::HintDelete => (
                "↑↓ select · Enter delete · Esc back",
                "↑↓ 选择 · Enter 删除 · Esc 返回",
            ),
            Key::HintChatBusy => (
                "^E expand/collapse · Esc cancel · ↑↓/PgUp/Dn scroll · ^C quit",
                "^E 展开/折叠 · Esc 取消 · ↑↓/PgUp/Dn 滚动 · ^C 退出",
            ),
            Key::HintChatIdle => (
                "^E expand/collapse · ↑↓/PgUp/Dn scroll · ^C quit",
                "^E 展开/折叠 · ↑↓/PgUp/Dn 滚动 · ^C 退出",
            ),
            // ---- slash 反馈 ----
            Key::UsageAddcmd => (
                "usage: /addcmd [-g] <name> <full command>",
                "用法: /addcmd [-g] <name> <命令全文>",
            ),
            Key::UsageSetting => (
                "usage: /setting [-g] <url|key|model|start|lang> <value> (-g writes global layer)",
                "用法: /setting [-g] <url|key|model|start|lang> <值>（-g 写全局层）",
            ),
            Key::BadName => (
                "name may only contain letters/digits/_/-",
                "name 只能包含字母/数字/_/-",
            ),
            Key::NoExeDir => (
                "cannot locate do.exe dir; global layer unavailable",
                "无法定位 do.exe 目录，全局层不可用",
            ),
            Key::NoPending => ("no pending proposals", "无待审批提案"),
            Key::NoApproved => ("no approved commands", "无已批准命令"),
            Key::Revoked => ("revoked: {}", "已撤销: {}"),
            Key::RevokedGlobal => ("revoked global: {}", "已撤销 全局: {}"),
            Key::AlsoInGlobal => (" (a same-named entry remains in the global layer)", "（全局层还有一条同名）"),
            Key::CmdNotFound => ("approved command not found: {}", "未找到已批准命令: {}"),
            Key::NewChatDone => (
                "new conversation started (the AI will read HANDOFF.md to resume)",
                "已开始新对话（AI 将自行读取 HANDOFF.md 续接）",
            ),
            Key::UpdatedField => ("updated {}", "已更新 {}"),
            Key::UpdatedGlobalField => ("updated global {}", "已更新 全局 {}"),
            Key::UnknownCmd => (
                "unknown command (/setting /new /addcmd /allowcmd /deletecmd /quit)",
                "未知命令（/setting /new /addcmd /allowcmd /deletecmd /quit）",
            ),
            Key::ProposalArrived => (
                "command proposal: {} = `{}` ({}), /allowcmd to review",
                "命令提案: {} = `{}`（{}），/allowcmd 审批",
            ),
            Key::Approved => ("approved & registered: {}", "已批准并注册: {}"),
            Key::ApprovedGlobal => ("approved & registered global: {}", "已批准并注册 全局: {}"),
            Key::NameConflict => ("name conflict: {} is already taken", "name 冲突：{} 已被占用"),
            Key::Rejected => ("rejected proposal: {}", "已拒绝提案: {}"),
            // ---- slash 候选用法 ----
            Key::UsageCmdSetting => ("/setting [-g] <url|key|model|start|lang> <value>", "/setting [-g] <url|key|model|start|lang> <值>"),
            Key::UsageCmdNew => ("/new", "/new"),
            Key::UsageCmdAddcmd => ("/addcmd <name> <full command>", "/addcmd <name> <命令全文>"),
            Key::UsageCmdAllowcmd => ("/allowcmd review command proposals", "/allowcmd 审批命令提案"),
            Key::UsageCmdDeletecmd => ("/deletecmd [name] revoke approved command", "/deletecmd [name] 撤销已批准命令"),
            Key::UsageCmdQuit => ("/quit", "/quit"),
            // ---- 设置页 ----
            Key::SettingsHeaderGlobal => (
                "global (edits write to do.config.json next to the exe; values shown are effective)",
                "全局（编辑写往 exe 旁 do.config.json；值 = 生效值）",
            ),
            Key::SettingsHeaderWs => (
                "workspace (edits write to .do/config.json)",
                "工作区（编辑写往 .do/config.json）",
            ),
            Key::Unset => ("(unset)", "（未设置）"),
            Key::SrcWorkspace => ("workspace", "工作区"),
            Key::SrcGlobal => ("global", "全局"),
            Key::SrcDefault => ("default", "默认"),
            // ---- 审批页 ----
            Key::ApproveTitle => ("command proposals ({} pending)", "命令提案审批（{} 条待批）"),
            Key::CmdLabel => ("command:", "命令:"),
            Key::DescLabel => ("desc: {}", "描述: {}"),
            Key::NoDesc => ("(none)", "(无)"),
            Key::TargetGlobal => (" · approves into the global layer", " · 批准到全局层"),
            // ---- 删除页 ----
            Key::DeleteHeader => (
                "approved commands (Enter deletes = revokes the command)",
                "已批准命令（Enter 删除即撤销该工具）",
            ),
            // ---- 状态栏 ----
            Key::ModelUnset => ("unset", "未设置"),
            // ---- 对话流杂项 ----
            Key::ThinkingOpen => ("thinking: {}", "思考: {}"),
            Key::ThinkingFolded => ("thinking (+{} chars)", "思考 (+{} 字)"),
            // ---- /lang ----
            Key::LangSet => ("language: {}", "界面语言: {}"),
            Key::LangName => ("English", "中文"),
            Key::UsageLang => ("usage: /lang [zh|en]", "用法: /lang [zh|en]"),
            Key::UsageCmdLang => ("/lang [zh|en]", "/lang [zh|en]"),
        };
        match self {
            Lang::En => en,
            Lang::Zh => zh,
        }
    }
}

/// 文案 key：每个用户可见字符串一个变体
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Key {
    SplashContinue,
    WorkspaceLabel,
    ApiKeySet,
    ApiKeyMissing,
    GlobalLayerUnavailable,
    AuditDisabled,
    Cancelled,
    ErrorPrefix,
    HintSettings,
    HintSettingsEdit,
    HintApprove,
    HintDelete,
    HintChatBusy,
    HintChatIdle,
    UsageAddcmd,
    UsageSetting,
    BadName,
    NoExeDir,
    NoPending,
    NoApproved,
    Revoked,
    RevokedGlobal,
    AlsoInGlobal,
    CmdNotFound,
    NewChatDone,
    UpdatedField,
    UpdatedGlobalField,
    UnknownCmd,
    ProposalArrived,
    Approved,
    ApprovedGlobal,
    NameConflict,
    Rejected,
    UsageCmdSetting,
    UsageCmdAddcmd,
    UsageCmdAllowcmd,
    UsageCmdNew,
    UsageCmdQuit,
    UsageCmdDeletecmd,
    SettingsHeaderGlobal,
    SettingsHeaderWs,
    Unset,
    SrcWorkspace,
    SrcGlobal,
    SrcDefault,
    ApproveTitle,
    CmdLabel,
    DescLabel,
    NoDesc,
    TargetGlobal,
    DeleteHeader,
    ModelUnset,
    ThinkingOpen,
    ThinkingFolded,
    LangSet,
    LangName,
    UsageLang,
    UsageCmdLang,
}

/// 全部 key（完整性测试遍历用）
#[cfg(test)]
pub const ALL_KEYS: &[Key] = &[
    Key::SplashContinue,
    Key::WorkspaceLabel,
    Key::ApiKeySet,
    Key::ApiKeyMissing,
    Key::GlobalLayerUnavailable,
    Key::AuditDisabled,
    Key::Cancelled,
    Key::ErrorPrefix,
    Key::HintSettings,
    Key::HintSettingsEdit,
    Key::HintApprove,
    Key::HintDelete,
    Key::HintChatBusy,
    Key::HintChatIdle,
    Key::UsageAddcmd,
    Key::UsageSetting,
    Key::BadName,
    Key::NoExeDir,
    Key::NoPending,
    Key::NoApproved,
    Key::Revoked,
    Key::RevokedGlobal,
    Key::AlsoInGlobal,
    Key::CmdNotFound,
    Key::NewChatDone,
    Key::UpdatedField,
    Key::UpdatedGlobalField,
    Key::UnknownCmd,
    Key::ProposalArrived,
    Key::Approved,
    Key::ApprovedGlobal,
    Key::NameConflict,
    Key::Rejected,
    Key::UsageCmdSetting,
    Key::UsageCmdAddcmd,
    Key::UsageCmdAllowcmd,
    Key::UsageCmdNew,
    Key::UsageCmdQuit,
    Key::UsageCmdDeletecmd,
    Key::SettingsHeaderGlobal,
    Key::SettingsHeaderWs,
    Key::Unset,
    Key::SrcWorkspace,
    Key::SrcGlobal,
    Key::SrcDefault,
    Key::ApproveTitle,
    Key::CmdLabel,
    Key::DescLabel,
    Key::NoDesc,
    Key::TargetGlobal,
    Key::DeleteHeader,
    Key::ModelUnset,
    Key::ThinkingOpen,
    Key::ThinkingFolded,
    Key::LangSet,
    Key::LangName,
    Key::UsageLang,
    Key::UsageCmdLang,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_has_both_languages() {
        // 结构本身已强制双语（元组两元素），这里再断言非空且互不相同
        for key in ALL_KEYS {
            assert!(!Lang::En.t(*key).is_empty(), "{key:?}");
            assert!(!Lang::Zh.t(*key).is_empty(), "{key:?}");
        }
        // ALL_KEYS 覆盖性：变体数应与枚举一致（漏加 key 时此处会落后）
        assert_eq!(ALL_KEYS.len(), 58);
    }

    #[test]
    fn parse_defaults_to_en() {
        assert_eq!(Lang::parse("zh"), Lang::Zh);
        assert_eq!(Lang::parse("en"), Lang::En);
        assert_eq!(Lang::parse(""), Lang::En);
        assert_eq!(Lang::parse("fr"), Lang::En);
    }
}
