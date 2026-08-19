//! 页面交互：设置页 / 审批页 / 删除页的按键处理与落盘动作
//!
//! # 模块导读
//! 每个页面一个按键处理器（模态状态机，≈ C# 的 enum State + switch），
//! 加上配套的读写动作。渲染在 pages 子模块；这里管"按键 → 状态/磁盘"。

use super::{Item, Mode, Ui, SETTINGS_FIELDS};
use agent_core::config::Config;
use agent_core::{AgentHandle, Cmd};
use crossterm::event::KeyCode;
use std::path::Path;

pub(super) fn settings_key(ui: &mut Ui, code: KeyCode, root: &Path) {
    // 编辑态：Enter 保存、Esc 放弃、字符进缓冲
    if ui.set_editing.is_some() {
        match code {
            KeyCode::Enter => {
                let value = ui.set_editing.take().unwrap_or_default();
                settings_save(ui, root, value);
            }
            KeyCode::Esc => ui.set_editing = None,
            KeyCode::Char(c) => {
                if let Some(buf) = &mut ui.set_editing {
                    buf.push(c);
                }
            }
            KeyCode::Backspace => {
                if let Some(buf) = &mut ui.set_editing {
                    buf.pop();
                }
            }
            _ => {}
        }
        return;
    }
    // 选择态
    match code {
        KeyCode::Up => ui.set_sel = ui.set_sel.saturating_sub(1),
        KeyCode::Down => ui.set_sel = (ui.set_sel + 1).min(SETTINGS_FIELDS.len() - 1),
        // 进入编辑态：缓冲预填当前真值（key 给的是未掩码的真值）
        KeyCode::Enter => ui.set_editing = Some(ui.set_values[ui.set_sel].clone()),
        KeyCode::Esc => ui.mode = Mode::Chat,
        _ => {}
    }
}

/// Approve 模式的按键：模态编辑器的状态机——
/// ≈ C# 里用 enum State { View, EditDesc } + switch 的手写状态机：
/// 同一个 Enter/Esc 在不同状态下含义不同（View：批准/拒绝/退出；EditDesc：保存/放弃）。
/// name 不可编辑（要改名 = 拒绝后用 /addcmd 重新注册），
/// command 不可改（审批的字符串 = 永远执行的全部内容，安全红线）。
pub(super) fn approve_key(ui: &mut Ui, code: KeyCode, agent: &mut AgentHandle, root: &Path) {
    // EditDesc 状态：字符直接改进选中条的 description，Enter 保存、Esc 还原备份
    if ui.appr_editing {
        match code {
            KeyCode::Enter => ui.appr_editing = false, // 保存：改动已就地生效
            KeyCode::Esc => {
                let backup = std::mem::take(&mut ui.appr_desc_backup);
                if let Some(p) = ui.pending.get_mut(ui.appr_sel) {
                    p.description = backup;
                }
                ui.appr_editing = false;
            }
            KeyCode::Char(c) => {
                if let Some(p) = ui.pending.get_mut(ui.appr_sel) {
                    p.description.push(c);
                }
            }
            KeyCode::Backspace => {
                if let Some(p) = ui.pending.get_mut(ui.appr_sel) {
                    p.description.pop();
                }
            }
            _ => {}
        }
        return;
    }
    // View 状态：列表导航 + 单条操作
    match code {
        KeyCode::Up => ui.appr_sel = ui.appr_sel.saturating_sub(1),
        KeyCode::Down => {
            if !ui.pending.is_empty() {
                ui.appr_sel = (ui.appr_sel + 1).min(ui.pending.len() - 1);
            }
        }
        KeyCode::Enter => approve_current(ui, agent, root),
        // x / d 拒绝选中条
        KeyCode::Char('x') | KeyCode::Char('d') => reject_current(ui),
        KeyCode::Char('e') => {
            // 进入描述编辑态：备份现值供 Esc 还原
            if let Some(p) = ui.pending.get(ui.appr_sel) {
                ui.appr_desc_backup = p.description.clone();
                ui.appr_editing = true;
            }
        }
        // Esc 退出页面：剩余提案保留在队列，下次 /allowcmd 继续
        KeyCode::Esc => ui.mode = Mode::Chat,
        _ => {}
    }
}

/// 批准选中提案：command 原样（含就地编辑过的 desc）落盘到目标层。
/// 目标层由提案的 global 标记决定：只有人类 /addcmd -g 能置位；
/// AI 提案（addcmd 工具）永远是工作区层——AI 不能获得跨项目生效的命令，
/// 这是一条安全边界（构造提案时硬编码 false，这里不再提供改写途径）。
fn approve_current(ui: &mut Ui, agent: &mut AgentHandle, root: &Path) {
    let Some(p) = ui.pending.get(ui.appr_sel).cloned() else {
        ui.mode = Mode::Chat;
        return;
    };
    let name = p.name.trim().to_string();
    // 批准前再验一次 name（AI 提案在 check_call 已验，这里双保险）
    if !agent_core::commands::valid_name(&name) {
        ui.items.push(Item::Info("name 只能包含字母/数字/_/-".into()));
        return;
    }
    // 选定目标层：读该层名单
    let target_dir = if p.global {
        match agent_core::config::exe_dir() {
            Some(d) => Some(d),
            None => {
                ui.items.push(Item::Info("无法定位 do.exe 目录，全局层不可用".into()));
                return;
            }
        }
    } else {
        None
    };
    let mut cmds = match &target_dir {
        Some(d) => agent_core::commands::load_global(d),
        None => agent_core::commands::load(root),
    };
    // 与内建工具或同层已批准命令重名 = 拒绝（覆盖已有工具太危险）。
    // start 是隐式保留名（白名单视图会合并 config.start），一并保护
    const BUILTIN: &[&str] = &["read", "write", "edit", "ls", "grep", "addcmd", "runcmd", "start"];
    if BUILTIN.contains(&name.as_str()) || cmds.iter().any(|c| c.name == name) {
        ui.items.push(Item::Info(format!("name 冲突：{name} 已被占用")));
        return;
    }
    cmds.push(p.clone());
    let save_result = match &target_dir {
        Some(d) => agent_core::commands::save_global(d, &cmds),
        None => agent_core::commands::save(root, &cmds),
    };
    match save_result {
        Ok(()) => {
            let layer = if p.global { " 全局" } else { "" };
            ui.items.push(Item::Info(format!("已批准并注册{layer}: {name}")));
            ui.pending.remove(ui.appr_sel);
            // 批准后通知模型：以 user 角色注入历史（不触发 API）。
            // tools 数组是冻结的，批准不改变 prompt 前缀——零缓存代价
            if ui.has_key {
                agent.send(Cmd::Notify(format!(
                    "（系统提示：命令 {name} 已获批准，可用 runcmd 调用）"
                )));
            }
            after_proposal_removed(ui);
        }
        Err(e) => ui.items.push(Item::Info(e.to_string())),
    }
}

/// 拒绝选中提案：丢弃，不入盘
fn reject_current(ui: &mut Ui) {
    if let Some(p) = ui.pending.get(ui.appr_sel) {
        ui.items.push(Item::Info(format!("已拒绝提案: {}", p.name)));
        ui.pending.remove(ui.appr_sel);
    }
    after_proposal_removed(ui);
}

/// 移除一条后的收尾：留在页面并自动选中下一条；批空自动退出回对话
fn after_proposal_removed(ui: &mut Ui) {
    ui.appr_editing = false;
    if ui.pending.is_empty() {
        ui.mode = Mode::Chat;
    } else {
        ui.appr_sel = ui.appr_sel.min(ui.pending.len() - 1);
    }
}

/// Delete 模式的按键：↑↓ 选择、Enter 删除（按来源层落盘）、Esc 返回
pub(super) fn delete_key(ui: &mut Ui, code: KeyCode, root: &Path) {
    match code {
        KeyCode::Up => ui.del_sel = ui.del_sel.saturating_sub(1),
        KeyCode::Down => {
            if !ui.del_list.is_empty() {
                ui.del_sel = (ui.del_sel + 1).min(ui.del_list.len() - 1);
            }
        }
        KeyCode::Enter => {
            if ui.del_sel < ui.del_list.len() {
                let (gone, src) = ui.del_list.remove(ui.del_sel);
                // 按来源层删：读该层名单、retain、写回
                let result = if src == "全局" {
                    agent_core::config::exe_dir().map(|d| {
                        let mut cmds = agent_core::commands::load_global(&d);
                        cmds.retain(|c| c.name != gone.name);
                        agent_core::commands::save_global(&d, &cmds)
                    })
                } else {
                    let mut cmds = agent_core::commands::load(root);
                    cmds.retain(|c| c.name != gone.name);
                    Some(agent_core::commands::save(root, &cmds))
                };
                match result {
                    Some(Ok(())) => {
                        let layer = if src == "全局" { " 全局" } else { "" };
                        ui.items.push(Item::Info(format!("已撤销{layer}: {}", gone.name)));
                    }
                    Some(Err(e)) => ui.items.push(Item::Info(e.to_string())),
                    None => ui.items.push(Item::Info("无法定位 do.exe 目录".into())),
                }
                ui.del_sel = ui.del_sel.min(ui.del_list.len().saturating_sub(1));
                if ui.del_list.is_empty() {
                    ui.mode = Mode::Chat;
                }
            }
        }
        KeyCode::Esc => ui.mode = Mode::Chat,
        _ => {}
    }
}

/// 设置页保存一个字段：写哪层只读写哪层（与 /setting 命令同一纪律）
fn settings_save(ui: &mut Ui, root: &Path, value: String) {
    let (field, global) = SETTINGS_FIELDS[ui.set_sel];
    let result = if global {
        match agent_core::config::exe_dir() {
            Some(dir) => {
                let mut cfg = Config::load_global(&dir);
                cfg.set(field, &value)
                    .and_then(|()| cfg.save_global(&dir).map_err(|e| e.to_string()))
            }
            None => Err("无法定位 do.exe 目录，全局配置层不可用".to_string()),
        }
    } else {
        let mut cfg = Config::load_workspace(root);
        cfg.set(field, &value)
            .and_then(|()| cfg.save(root).map_err(|e| e.to_string()))
    };
    match result {
        Ok(()) => {
            // 重新载入合并视图：显示始终是生效值（若被更高优先级层
            // 覆盖，保存的值不会出现在显示里——这正是覆盖语义）
            load_settings(ui, root);
            // 两层都影响状态栏，保存后刷新
            if field == "model" {
                ui.model = if value.is_empty() { "未设置".into() } else { value.clone() };
            }
            if field == "key" {
                ui.has_key = !value.is_empty();
            }
            ui.items.push(Item::Info(format!(
                "已更新{} {field}",
                if global { " 全局" } else { "" }
            )));
        }
        Err(e) => ui.items.push(Item::Info(e)),
    }
}

/// agent 事件处理：流式增量拼到对话流最后一条同类记录上
pub(super) fn enter_settings(ui: &mut Ui, root: &Path) {
    load_settings(ui, root);
    ui.set_sel = 0;
    ui.set_editing = None;
    ui.mode = Mode::Settings;
}

/// 载入设置页数据：值 = 合并后的生效值；来源 = 工作区/全局/默认。
fn load_settings(ui: &mut Ui, root: &Path) {
    let exe = agent_core::config::exe_dir();
    let ws = Config::load_workspace(root);
    let global = exe.as_deref().map(Config::load_global);
    let merged = Config::load_merged(root, exe.as_deref());
    ui.set_values = SETTINGS_FIELDS
        .iter()
        .map(|(f, _)| cfg_field(&merged, f))
        .collect();
    ui.set_sources = SETTINGS_FIELDS
        .iter()
        .map(|(f, _)| {
            if !cfg_field(&ws, f).is_empty() {
                "工作区" // 工作区层非空：它优先
            } else if global.as_ref().is_some_and(|g| !cfg_field(g, f).is_empty()) {
                "全局" // 全局便携层兜底
            } else if !cfg_field(&merged, f).is_empty() {
                "默认" // 两层皆空但生效值非空：内置默认（url）
            } else {
                ""
            }
        })
        .collect();
}

/// 按字段名取配置值（设置页载入用）
fn cfg_field(cfg: &Config, field: &str) -> String {
    match field {
        "url" => cfg.url.clone(),
        "key" => cfg.key.clone(),
        "model" => cfg.model.clone(),
        "start" => cfg.start.clone(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::{slash, test_ui};
    use agent_core::ApprovedCommand;
    use crate::tui::pages::approve_lines;

    #[tokio::test(flavor = "current_thread")]
    async fn addcmd_g_marks_global_target() {
        let dir = std::env::temp_dir().join(format!("doagent-tuig-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut agent = AgentHandle::start(&dir).unwrap();
        let mut ui = test_ui();
        // -g：目标层标记为全局（批准时才落盘，此处只验证分流标记）
        slash(&mut ui, &mut agent, &dir, "addcmd -g deploy cargo build --release");
        assert_eq!(ui.pending.len(), 1);
        assert!(ui.pending[0].global);
        assert_eq!(ui.pending[0].name, "deploy");
        assert_eq!(ui.pending[0].command, "cargo build --release");
        assert_eq!(ui.mode, Mode::Approve);
        // 不带 -g：工作区层
        slash(&mut ui, &mut agent, &dir, "addcmd local echo x");
        assert!(!ui.pending[1].global);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn addcmd_self_register_and_desc_editing() {
        let dir = std::env::temp_dir().join(format!("doagent-tui2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut agent = AgentHandle::start(&dir).unwrap();
        let mut ui = test_ui();
        // 自助注册：提案入队（desc 空、mode 默认 once）并直接开审批页选中它
        slash(&mut ui, &mut agent, &dir, "addcmd deploy cargo build --release");
        assert_eq!(ui.pending.len(), 1);
        assert_eq!(ui.pending[0].name, "deploy");
        assert_eq!(ui.pending[0].command, "cargo build --release"); // 含空格的整行剩余
        assert_eq!(ui.pending[0].mode, "once");
        assert!(ui.pending[0].description.is_empty());
        assert_eq!(ui.mode, Mode::Approve);
        assert_eq!(ui.appr_sel, 0);
        // 非法 name：对话流报错且不入队
        slash(&mut ui, &mut agent, &dir, "addcmd bad;name echo x");
        assert_eq!(ui.pending.len(), 1);
        assert!(matches!(ui.items.last(), Some(Item::Info(t)) if t.contains("字母/数字")));
        // 描述编辑态：e 进入 → 敲字 → Enter 保存（就地改进选中条）
        approve_key(&mut ui, KeyCode::Char('e'), &mut agent, &dir);
        assert!(ui.appr_editing);
        for c in "部署".chars() {
            approve_key(&mut ui, KeyCode::Char(c), &mut agent, &dir);
        }
        approve_key(&mut ui, KeyCode::Enter, &mut agent, &dir);
        assert!(!ui.appr_editing);
        assert_eq!(ui.pending[0].description, "部署");
        // 再进编辑态 → 敲字 → Esc 放弃并还原，仍在审批页
        approve_key(&mut ui, KeyCode::Char('e'), &mut agent, &dir);
        approve_key(&mut ui, KeyCode::Char('x'), &mut agent, &dir);
        approve_key(&mut ui, KeyCode::Esc, &mut agent, &dir);
        assert!(!ui.appr_editing);
        assert_eq!(ui.pending[0].description, "部署");
        assert_eq!(ui.mode, Mode::Approve);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn allowcmd_list_view_flow() {
        let dir = std::env::temp_dir().join(format!("doagent-tui3-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut agent = AgentHandle::start(&dir).unwrap();
        let mut ui = test_ui();
        // 两条待批提案
        for (n, d) in [("aaa", "命令甲"), ("bbb", "命令乙")] {
            ui.pending.push(ApprovedCommand {
                name: n.into(),
                command: format!("echo {n}"),
                description: d.into(),
                mode: "once".into(),
                global: false,
            });
        }
        slash(&mut ui, &mut agent, &dir, "allowcmd");
        assert_eq!(ui.mode, Mode::Approve);
        // 列表视图：两条都可见
        let text: String = approve_lines(&ui, 80)
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(text.contains("aaa") && text.contains("bbb"), "{text}");
        assert!(text.contains("2 条待批"), "{text}");
        // ↓ 移动选中，详情区切到选中条
        approve_key(&mut ui, KeyCode::Down, &mut agent, &dir);
        assert_eq!(ui.appr_sel, 1);
        let text: String = approve_lines(&ui, 80)
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(text.contains("echo bbb"), "{text}"); // 详情显示选中条命令全文
        // Enter 批准选中条：落盘、列表缩减、自动选中下一条、留在页面
        approve_key(&mut ui, KeyCode::Enter, &mut agent, &dir);
        assert_eq!(ui.pending.len(), 1);
        assert_eq!(ui.pending[0].name, "aaa");
        assert_eq!(ui.appr_sel, 0);
        assert_eq!(ui.mode, Mode::Approve);
        let saved = agent_core::commands::load(&dir);
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].name, "bbb");
        // x 拒绝剩余条：批空自动退出回对话
        approve_key(&mut ui, KeyCode::Char('x'), &mut agent, &dir);
        assert!(ui.pending.is_empty());
        assert_eq!(ui.mode, Mode::Chat);
        assert!(matches!(ui.items.last(), Some(Item::Info(t)) if t.contains("已拒绝")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn allowcmd_esc_keeps_queue() {
        let dir = std::env::temp_dir().join(format!("doagent-tui4-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut agent = AgentHandle::start(&dir).unwrap();
        let mut ui = test_ui();
        ui.pending.push(ApprovedCommand {
            name: "keep".into(),
            command: "echo keep".into(),
            description: String::new(),
            mode: "once".into(),
            global: false,
        });
        slash(&mut ui, &mut agent, &dir, "allowcmd");
        assert_eq!(ui.mode, Mode::Approve);
        // Esc 退出页面，队列保留；下次 /allowcmd 继续
        approve_key(&mut ui, KeyCode::Esc, &mut agent, &dir);
        assert_eq!(ui.mode, Mode::Chat);
        assert_eq!(ui.pending.len(), 1);
        slash(&mut ui, &mut agent, &dir, "allowcmd");
        assert_eq!(ui.mode, Mode::Approve);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn settings_page_shows_effective_merged_values() {
        // 回归：值写在工作区层时，设置页全局字段区也曾显示"未设置"。
        // 现在显示合并生效值并标注来源层
        let dir = std::env::temp_dir().join(format!("doagent-set-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = Config::default();
        cfg.set("model", "ws-model").unwrap();
        cfg.save(&dir).unwrap(); // 写工作区层
        let mut ui = test_ui();
        enter_settings(&mut ui, &dir);
        let idx = SETTINGS_FIELDS.iter().position(|(f, _)| *f == "model").unwrap();
        assert_eq!(ui.set_values[idx], "ws-model"); // 生效值可见
        assert_eq!(ui.set_sources[idx], "工作区"); // 来源标注
        // url 两层皆空 → 生效值是内置默认，来源"默认"
        let uidx = SETTINGS_FIELDS.iter().position(|(f, _)| *f == "url").unwrap();
        assert_eq!(ui.set_values[uidx], "https://api.openai.com/v1");
        assert_eq!(ui.set_sources[uidx], "默认");
        let _ = std::fs::remove_dir_all(&dir);
    }

}
