//! 7 个 AI 工具：read / write / edit / ls / grep / addcmd / runcmd（数组冻结）
//!
//! # 模块导读
//! 每个工具 = 一个参数 schema（发给模型的 JSON Schema）+ 一个执行函数。
//! 所有文件路径都先过 [`crate::workspace::Workspace`] 守卫，
//! `.do/` 在这里被彻底隐形：read/edit 报"不存在"、ls/grep 跳过、
//! 只有 write 报 Windows 权限拒绝（os error 5）。
//!
//! # 源头截断（上下文管理第一条）
//! read 限 400 行、ls 限 200 条、grep 限 100 条匹配、runcmd 限尾部 20 KB——
//! 超限直接在源头掐断，只把截断后的内容送回模型。

use crate::commands::ApprovedCommand;
use crate::config::Config;
use crate::workspace::Workspace;
use serde_json::{json, Value};
use std::borrow::Cow;

/// 单个工具的定义：名称 + 一句话描述 + 参数 JSON Schema。
/// 注意：tool schema 是发给模型的 token 大头，描述务必保持极简。
/// 名称/描述用 Cow（clone-on-write ≈ C# 的"既是 string 又是 string 引用"
/// 的类型）：内建工具借用静态字符串零分配，动态命令工具用拥有的 String。
pub struct ToolDef {
    pub name: Cow<'static, str>,
    pub description: Cow<'static, str>,
    pub parameters: Value,
}

/// 全部 6 个工具的定义，随每次 API 请求发送。
/// Vec ≈ C# 的 List<T>；json! 宏就地构造 JSON（≈ 匿名对象字面量）。
pub fn defs() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "read".into(),
            description: "读文件内容。start/limit 选读行区间，默认前 400 行".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path":  { "type": "string", "description": "相对工作区的路径" },
                    "start": { "type": "integer", "description": "起始行(从1计,默认1)" },
                    "limit": { "type": "integer", "description": "最多行数(默认400)" }
                },
                "required": ["path"]
            }),
        },
        ToolDef {
            name: "write".into(),
            description: "整文件写入（覆盖或新建）".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path":    { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"]
            }),
        },
        ToolDef {
            name: "edit".into(),
            description: "精确替换：把 old 换成 new。old 不唯一时须 all=true 全部替换".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "old":  { "type": "string" },
                    "new":  { "type": "string" },
                    "all":  { "type": "boolean", "description": "替换全部出现(默认false)" }
                },
                "required": ["path", "old", "new"]
            }),
        },
        ToolDef {
            name: "ls".into(),
            description: "列目录（最多 200 条，目录名带 / 后缀）".into(),
            parameters: json!({
                "type": "object",
                "properties": { "path": { "type": "string", "description": "默认根目录" } }
            }),
        },
        ToolDef {
            name: "grep".into(),
            description: "正则搜索文件内容，返回 文件:行号: 内容，最多 100 条".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "正则表达式" },
                    "path":    { "type": "string", "description": "起始目录,默认根" }
                },
                "required": ["pattern"]
            }),
        },
        ToolDef {
            // 命令白名单的入口：AI 只提案，人类在 TUI 审批后才生效
            name: "addcmd".into(),
            description: "提案一条固定命令，人类批准后可用 runcmd 调用".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name":        { "type": "string", "description": "工具名,仅限字母数字_-" },
                    "command":     { "type": "string", "description": "固定执行串(零参数)" },
                    "description": { "type": "string", "description": "一句话说明" },
                    "mode":        { "type": "string", "description": "once 一次性 / daemon 常驻" }
                },
                "required": ["name", "command", "description", "mode"]
            }),
        },
        ToolDef {
            // 发现式调用：白名单内容不进 tools 数组（见 agent.rs 缓存论点注释），
            // AI 用 runcmd 无参列出白名单，带 name 执行
            name: "runcmd".into(),
            description: "列出或执行已批准的固定命令（不带 name 列出全部）".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "命令名,省略则列出白名单" }
                }
            }),
        },
    ]
}

/// 执行工具。返回给模型的永远是字符串：成功是结果，失败是错误文案。
/// 参数用 `&Value`（借用）：调用方保留所有权，这里只读——
/// ≈ C# 传引用不拷贝，但 Rust 在编译期就保证没人能偷偷改。
pub async fn run(ws: &Workspace, name: &str, args: &Value) -> String {
    match name {
        "read" => tool_read(ws, args),
        "write" => tool_write(ws, args),
        "edit" => tool_edit(ws, args),
        "ls" => tool_ls(ws, args),
        "grep" => tool_grep(ws, args),
        "runcmd" => tool_runcmd(ws, args).await,
        // addcmd 正常由 agent 拦截（要发审批事件）；
        // 走到这里说明漏拦，至少别把提案弄丢
        "addcmd" => "已提交人类审批，结果待人工确认".to_string(),
        other => format!("未知工具: {other}"),
    }
}

/// 从参数里取字符串字段，缺失时给统一文案
fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    // Option 的 ok_or / ? 组合：None 直接转成 Err 提前返回
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("缺少参数 {key}"))
}

fn tool_read(ws: &Workspace, args: &Value) -> String {
    let path = match arg_str(args, "path") {
        Ok(p) => p,
        Err(e) => return e,
    };
    // 守卫：`.do` 在这里变成与"文件不存在"逐字相同的错误
    let real = match ws.resolve_read(path) {
        Ok(p) => p,
        Err(e) => return e.to_string(),
    };
    let start = args.get("start").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(400) as usize;

    let content = match std::fs::read_to_string(&real) {
        Ok(c) => c,
        Err(e) => return e.to_string(),
    };
    // 迭代器适配器链 ≈ C# LINQ：skip/take ≈ Skip/Take，collect ≈ ToList
    let total = content.lines().count();
    let picked: Vec<String> = content
        .lines()
        .skip(start.saturating_sub(1))
        .take(limit)
        // enumerate 给每行配上行号，方便模型对准 edit 位置
        .enumerate()
        .map(|(i, l)| format!("{}\t{l}", start + i))
        .collect();
    let mut out = picked.join("\n");
    if start + picked.len() <= total {
        out.push_str(&format!("\n... (共 {total} 行, 已截断)"));
    }
    out
}

fn tool_write(ws: &Workspace, args: &Value) -> String {
    let (path, content) = match (arg_str(args, "path"), arg_str(args, "content")) {
        (Ok(p), Ok(c)) => (p, c),
        (Err(e), _) | (_, Err(e)) => return e,
    };
    // 写是唯一例外：`.do` 内写入返回 os error 5（权限拒绝）
    let real = match ws.resolve_write(path) {
        Ok(p) => p,
        Err(e) => return e.to_string(),
    };
    // 父目录不存在时顺手创建（写入新嵌套文件本应成功）
    if let Some(parent) = real.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return e.to_string();
        }
    }
    match std::fs::write(&real, content) {
        Ok(()) => format!("已写入 {} 字节", content.len()),
        Err(e) => e.to_string(),
    }
}

fn tool_edit(ws: &Workspace, args: &Value) -> String {
    let (path, old, new) = match (arg_str(args, "path"), arg_str(args, "old"), arg_str(args, "new"))
    {
        (Ok(p), Ok(o), Ok(n)) => (p, o, n),
        (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => return e,
    };
    let all = args.get("all").and_then(|v| v.as_bool()).unwrap_or(false);
    let real = match ws.resolve_read(path) {
        Ok(p) => p,
        Err(e) => return e.to_string(),
    };
    let content = match std::fs::read_to_string(&real) {
        Ok(c) => c,
        Err(e) => return e.to_string(),
    };
    let hits = content.matches(old).count();
    if hits == 0 {
        return "未找到 old 指定的内容".to_string();
    }
    if hits > 1 && !all {
        // 不唯一且未声明 all：拒绝，防止误替换
        return format!("old 不唯一（{hits} 处），请补充上下文或设 all=true");
    }
    let replaced = if all {
        content.replace(old, new)
    } else {
        content.replacen(old, new, 1)
    };
    match std::fs::write(&real, replaced) {
        Ok(()) => format!("已替换 {hits} 处"),
        Err(e) => e.to_string(),
    }
}

fn tool_ls(ws: &Workspace, args: &Value) -> String {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
    let real = match ws.resolve_read(path) {
        Ok(p) => p,
        Err(e) => return e.to_string(),
    };
    let rd = match std::fs::read_dir(&real) {
        Ok(r) => r,
        Err(e) => return e.to_string(),
    };
    let mut names: Vec<String> = Vec::new();
    for entry in rd {
        if names.len() >= 200 {
            names.push("... (已截断至 200 条)".to_string());
            break;
        }
        // `if let Ok(e) = ...`：只关心成功的分支，失败静默跳过
        if let Ok(e) = entry {
            // `.do` 不列出：文件名规范化后判断
            if crate::workspace::is_hidden_name(&e.file_name().to_string_lossy()) {
                continue;
            }
            let mut n = e.file_name().to_string_lossy().into_owned();
            if e.path().is_dir() {
                n.push('/');
            }
            names.push(n);
        }
    }
    names.sort();
    names.join("\n")
}

fn tool_grep(ws: &Workspace, args: &Value) -> String {
    let pattern = match arg_str(args, "pattern") {
        Ok(p) => p,
        Err(e) => return e,
    };
    // regex-lite：轻量正则（≈ C# Regex 的小子集），换来极小体积
    let re = match regex_lite::Regex::new(pattern) {
        Ok(r) => r,
        Err(e) => return format!("正则错误: {e}"),
    };
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
    let real = match ws.resolve_read(path) {
        Ok(p) => p,
        Err(e) => return e.to_string(),
    };
    let root = ws.root();
    let mut out: Vec<String> = Vec::new();
    // ignore crate 的 Walk：递归遍历且默认尊重 .gitignore / 跳过隐藏与 .git
    for entry in ignore::Walk::new(&real) {
        if out.len() >= 100 {
            out.push("... (已截断至 100 条匹配)".to_string());
            break;
        }
        let Ok(e) = entry else { continue }; // let-else：不匹配就提前 continue
        let p = e.path();
        if !p.is_file() {
            continue;
        }
        // `.do` 跳过（含 realpath 后暴露出来的，双保险走 workspace 判断）
        if ws.is_hidden_path(p) {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(p) else { continue };
        // strip_prefix 把绝对路径转回工作区相对路径，输出更短
        let rel = p.strip_prefix(root).unwrap_or(p);
        for (i, line) in content.lines().enumerate() {
            if re.is_match(line) {
                out.push(format!("{}:{}: {}", rel.display(), i + 1, line));
                if out.len() >= 100 {
                    break;
                }
            }
        }
    }
    if out.is_empty() {
        "无匹配".to_string()
    } else {
        out.join("\n")
    }
}

/// runcmd：发现式调用——不带 name 列出白名单，带 name 现读现执行。
/// 不走 workspace 守卫——`.do` 对 AI 隐形，但工具自己人可读。
async fn tool_runcmd(ws: &Workspace, args: &Value) -> String {
    let list = whitelist(ws);
    match args.get("name").and_then(|v| v.as_str()) {
        // 无参：格式化列出全部（name、命令全文、mode、来源层、description）
        None => {
            if list.is_empty() {
                return "当前无已批准命令，可用 addcmd 提案".to_string();
            }
            list.iter()
                .map(|(c, src)| {
                    format!("{} = `{}`（{}·{}）{}", c.name, c.command, c.mode, src, c.description)
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
        Some(n) => match list.iter().find(|(c, _)| c.name == n) {
            Some((c, _)) => run_approved(ws, c).await,
            // 自愈原则：错误信息附上当前白名单，模型看到可自我纠正
            None => {
                let names = if list.is_empty() {
                    "（空）".to_string()
                } else {
                    list.iter().map(|(c, _)| c.name.as_str()).collect::<Vec<_>>().join(", ")
                };
                format!("未知命令 {n}，当前已批准：{names}")
            }
        },
    }
}

/// 白名单合并视图 = 工作区层 + 全局层（重名工作区赢，带来源标注）
/// 外加隐式 start 条目（config.start 非空时并入工作区侧，不落盘）。
/// 全局层文件在 exe 旁、工作区之外，AI 物理不可达。
fn whitelist(ws: &Workspace) -> Vec<(ApprovedCommand, &'static str)> {
    let mut cmds = crate::commands::merged(ws.root(), crate::config::exe_dir().as_deref());
    let start = Config::load_workspace(ws.root()).start;
    if !start.is_empty() && !cmds.iter().any(|(c, _)| c.name == "start") {
        cmds.push((
            ApprovedCommand {
                name: "start".into(),
                command: start,
                description: "配置的启动/构建命令（/setting start 设置）".into(),
                mode: "once".into(),
                global: false,
            },
            "工作区",
        ));
    }
    cmds
}

/// 执行批准的固定命令：once 等结束返回输出尾部；daemon 后台 spawn 立即返回。
/// cwd 永远固定为工作区根。
async fn run_approved(ws: &Workspace, cmd: &ApprovedCommand) -> String {
    if cmd.mode == "daemon" {
        // 常驻命令：后台启动即返回。stdio 全部置空——
        // 继承 TUI 终端会把 alternate screen 的画面对花
        let r = shell_command(&cmd.command)
            .current_dir(ws.root())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        return match r {
            Ok(_) => format!("已后台启动（daemon）：{}", cmd.command),
            Err(e) => e.to_string(),
        };
    }
    run_once(ws, &cmd.command).await
}

/// once 语义：执行固定命令串，等结束，返回输出尾部 20 KB。
/// start 与批准的动态命令共用这一条执行路径。
async fn run_once(ws: &Workspace, cmd: &str) -> String {
    let out = match shell_command(cmd)
        .current_dir(ws.root())
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => return e.to_string(),
    };
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !stderr.is_empty() {
        text.push_str("\n--- stderr ---\n");
        text.push_str(&stderr);
    }
    // 只保留尾部 20 KB（编译报错通常在最后）
    const TAIL: usize = 20 * 1024;
    if text.len() > TAIL {
        let cut = text.len() - TAIL;
        // 在 char 边界上切，防止切断 UTF-8 多字节字符
        let mut i = cut;
        while i < text.len() && !text.is_char_boundary(i) {
            i += 1;
        }
        text = format!("... (前部已截断)\n{}", &text[i..]);
    }
    format!("exit code: {}\n{text}", out.status.code().unwrap_or(-1))
}

/// 用系统 shell 包装执行固定命令串（Windows `cmd /c`，Unix `sh -c`）。
/// 为什么包一层而不是空白切分后直接 spawn：Windows 上 `npm` 实为
/// `npm.cmd`，CreateProcess 不做 PATHEXT 查找，直接 spawn "npm" 会报
/// "找不到文件"；`cmd /c` 才有完整的命令查找语义
/// （≈ C# 的 ProcessStartInfo.UseShellExecute = true）。
/// 安全性：这里执行的字符串全部来自人工审批/用户配置的**常量**，
/// 运行时零拼接零参数——shell 包装不引入注入面。
fn shell_command(cmd: &str) -> tokio::process::Command {
    // cfg! 宏：编译期布尔常量，另一分支会被死代码消除
    if cfg!(windows) {
        let mut c = tokio::process::Command::new("cmd");
        c.args(["/c", cmd]);
        c
    } else {
        let mut c = tokio::process::Command::new("sh");
        c.args(["-c", cmd]);
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp_ws(tag: &str) -> (Workspace, std::path::PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("doagent-tools-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".do")).unwrap();
        (Workspace::new(&dir).unwrap(), dir)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn read_do_identical_to_missing() {
        let (ws, dir) = temp_ws("t1");
        std::fs::write(dir.join(".do/secret.txt"), "x").unwrap();
        // `.do` 内读 与 读真实不存在文件：返回给 AI 的文案逐字一致
        let hidden = run(&ws, "read", &json!({"path": ".do/secret.txt"})).await;
        let missing = run(&ws, "read", &json!({"path": "no-such.txt"})).await;
        assert_eq!(hidden, missing);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn write_do_fails_silently() {
        let (ws, dir) = temp_ws("t2");
        // 写 `.do`：报错（权限拒绝），且文件绝不能真的被写出来
        let out = run(&ws, "write", &json!({"path": ".do/x.txt", "content": "hi"})).await;
        assert!(!out.starts_with("已写入"));
        assert!(!dir.join(".do/x.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ls_hides_do() {
        let (ws, dir) = temp_ws("t3");
        std::fs::write(dir.join("visible.txt"), "x").unwrap();
        let out = run(&ws, "ls", &json!({})).await;
        assert!(out.contains("visible.txt"));
        assert!(!out.contains(".do"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn grep_skips_do() {
        let (ws, dir) = temp_ws("t4");
        std::fs::write(dir.join(".do/config.json"), "needle").unwrap();
        std::fs::write(dir.join("a.txt"), "needle here").unwrap();
        let out = run(&ws, "grep", &json!({"pattern": "needle"})).await;
        assert!(out.contains("a.txt"));
        assert!(!out.contains(".do"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn edit_uniqueness() {
        let (ws, dir) = temp_ws("t5");
        std::fs::write(dir.join("f.txt"), "aa bb aa").unwrap();
        // 多处命中且未 all：拒绝
        let out = run(&ws, "edit", &json!({"path":"f.txt","old":"aa","new":"cc"})).await;
        assert!(out.contains("不唯一"), "got: {out}");
        assert_eq!(std::fs::read_to_string(dir.join("f.txt")).unwrap(), "aa bb aa");
        // all=true：全部替换
        let out =
            run(&ws, "edit", &json!({"path":"f.txt","old":"aa","new":"cc","all":true})).await;
        assert!(out.contains("2 处"));
        assert_eq!(std::fs::read_to_string(dir.join("f.txt")).unwrap(), "cc bb cc");
        // 未命中
        let out = run(&ws, "edit", &json!({"path":"f.txt","old":"zz","new":"q"})).await;
        assert!(out.contains("未找到"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn read_truncates_at_400() {
        let (ws, dir) = temp_ws("t6");
        let big: String = (1..=500).map(|i| format!("line{i}\n")).collect();
        std::fs::write(dir.join("big.txt"), big).unwrap();
        let out = run(&ws, "read", &json!({"path": "big.txt"})).await;
        assert!(out.contains("400\tline400"));
        assert!(!out.contains("line401"));
        assert!(out.contains("共 500 行"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dotdot_escape_blocked() {
        let (ws, dir) = temp_ws("t7");
        let out = run(&ws, "read", &json!({"path": "../outside.txt"})).await;
        assert!(out.contains("越出工作区"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn sample_cmds() -> Vec<ApprovedCommand> {
        vec![
            ApprovedCommand {
                name: "hello".into(),
                // "echo hello" 经 cmd /c 或 sh -c 都能跑（跨平台测试命令）
                command: "echo hello-from-cmd".into(),
                description: "测试用一次性命令".into(),
                mode: "once".into(),
                global: false,
            },
            ApprovedCommand {
                name: "srv".into(),
                command: "echo hi".into(),
                description: "测试用常驻命令".into(),
                mode: "daemon".into(),
                global: false,
            },
        ]
    }

    #[test]
    fn defs_frozen_at_seven() {
        // tools 数组冻结：恰为 7 个固定内建工具，
        // 白名单内容不进入 defs（缓存论点见 agent.rs）
        // as_ref 借的是临时 Vec，先绑定再取（临时值生命周期 ≈ C# 里
        // 对方法返回值直接取引用会被编译器拦住——Rust 强制先落变量）
        let defs = defs();
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_ref()).collect();
        assert_eq!(
            names,
            ["read", "write", "edit", "ls", "grep", "addcmd", "runcmd"]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn runcmd_lists_whitelist() {
        let (ws, dir) = temp_ws("c1");
        // 空名单提示
        let out = run(&ws, "runcmd", &json!({})).await;
        assert!(out.contains("当前无已批准命令"), "{out}");
        // 落盘后列出：name/命令全文/mode 都在
        crate::commands::save(&dir, &sample_cmds()).unwrap();
        let out = run(&ws, "runcmd", &json!({})).await;
        assert!(out.contains("hello = `echo hello-from-cmd`（once·工作区）"), "{out}");
        assert!(out.contains("srv"), "{out}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn runcmd_executes_and_reports_unknown() {
        let (ws, dir) = temp_ws("c2");
        crate::commands::save(&dir, &sample_cmds()).unwrap();
        // once：真实执行，输出原样返回
        let out = run(&ws, "runcmd", &json!({"name": "hello"})).await;
        assert!(out.contains("hello-from-cmd"), "{out}");
        assert!(out.contains("exit code: 0"), "{out}");
        // daemon：立即返回"已后台启动"，不等待
        let out = run(&ws, "runcmd", &json!({"name": "srv"})).await;
        assert!(out.contains("已后台启动"), "{out}");
        // 未知 name：错误附当前白名单（自愈）
        let out = run(&ws, "runcmd", &json!({"name": "nosuch"})).await;
        assert!(out.contains("未知命令 nosuch"), "{out}");
        assert!(out.contains("hello") && out.contains("srv"), "{out}");
        // 移除后工具立即消失（每次现读名单）
        let mut cmds = crate::commands::load(&dir);
        cmds.retain(|c| c.name != "hello");
        crate::commands::save(&dir, &cmds).unwrap();
        let out = run(&ws, "runcmd", &json!({"name": "hello"})).await;
        assert!(out.contains("未知命令 hello"), "{out}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn implicit_start_entry() {
        let (ws, dir) = temp_ws("c3");
        // config.start 为空：列表里没有 start
        let out = run(&ws, "runcmd", &json!({})).await;
        assert!(!out.contains("start"), "{out}");
        // 配上 start：成为隐式条目（once），且可通过 runcmd 执行
        let mut cfg = Config::default();
        cfg.set("start", "echo implicit-start").unwrap();
        cfg.save(&dir).unwrap();
        let out = run(&ws, "runcmd", &json!({})).await;
        assert!(out.contains("start = `echo implicit-start`（once·工作区）"), "{out}");
        let out = run(&ws, "runcmd", &json!({"name": "start"})).await;
        assert!(out.contains("implicit-start"), "{out}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
