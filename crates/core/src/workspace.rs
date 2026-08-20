//! 工作区守卫：路径归一化、根内校验、`.do/` 隐形
//!
//! # 模块导读
//! 所有 AI 工具拿到的路径都必须先过这里的校验，规则有四层（顺序敏感）：
//! 1. **词法归一**：不访问文件系统，纯字符串折叠 `.` / `..`、统一分隔符。
//! 2. **`.do` 隐形判断**：对归一化后的相对路径，比较前统一小写并去掉
//!    每段的尾部空格/点（Windows 上 `.DO`、`.do `、`.do.` 都是同一目录）。
//! 3. **realpath 最深已存在祖先**：从目标一路向上找第一个真实存在的祖先
//!    并做符号链接解析（canonicalize），防 symlink 逃逸。
//! 4. **根内校验**：解析后的绝对路径必须仍在工作区根内。
//!
//! # `.do` 隐形语义（本模块最重要的输出）
//! 对 AI 而言 `.do/` 是**不存在**而不是"被拒绝"：
//! - 读/编辑类访问返回与"文件不存在"**逐字相同**的 io 错误；
//! - 写是唯一例外：返回 `from_raw_os_error(5)`（Windows 权限拒绝，
//!   `ErrorKind::PermissionDenied`，显示 "Access is denied."），
//!   行为与一个碰巧不可写的目录不可区分。

use std::io;
use std::path::{Component, Path, PathBuf};

/// 校验后的放行决定：调用方据此决定"继续"还是"装死"
pub enum Access {
    /// 正常放行，附带规范化后的绝对路径
    Allow(PathBuf),
    /// `.do` 内部路径：当作不存在（read/edit/ls 用）
    Hidden,
    /// `.do` 内部路径的写入：伪装成普通权限拒绝（write 用）
    HiddenWrite,
}

/// 工作区根。启动时以 cwd 创建，此后所有路径都以它为界。
pub struct Workspace {
    /// realpath 后的根目录（绝对、无符号链接）
    root: PathBuf,
}

impl Workspace {
    /// 以 `root` 为工作区根创建守卫。
    /// 创建时 canonicalize 一次，之后所有比较都基于这个真实路径。
    pub fn new(root: impl AsRef<Path>) -> io::Result<Workspace> {
        // `?` 运算符：出错则把错误向上 return（≈ C# 里把异常原样抛给调用方的
        // 结构化写法），成功则取出 Ok 里的值继续。Rust 里没有异常，Result 就是
        // "可能失败的返回值"，`?` 是处理它的惯用语法。
        let root = root.as_ref().canonicalize()?;
        Ok(Workspace { root })
    }

    /// 当前进程的工作区（cwd）。`pub` 给 main 用。
    pub fn cwd() -> io::Result<Workspace> {
        Workspace::new(std::env::current_dir()?)
    }

    /// 工作区根（真实路径）
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 完整校验：四层规则按序执行。
    /// `for_write` 区分读写，决定 `.do` 命中的伪装方式。
    pub fn resolve(&self, path: &str, for_write: bool) -> io::Result<Access> {
        // 第 1 层：词法归一（纯字符串，不碰磁盘）
        let joined = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            self.root.join(path)
        };
        let norm = normalize(&joined);

        // 第 2 层：`.do` 隐形判断（基于词法路径，任何段命中都算）
        if touches_do(&norm) {
            // match ≈ C# switch 的模式匹配增强版：必须穷尽所有分支
            return Ok(if for_write {
                Access::HiddenWrite
            } else {
                Access::Hidden
            });
        }

        // 第 3 层：realpath 最深已存在祖先（防 symlink 逃逸）。
        // 目标本身可能不存在（比如 write 新文件），所以向上找。
        let mut cursor = norm.clone();
        let real = loop {
            // `loop {}` 无限循环 + `break 值` 还能把循环当表达式用（返回值）
            match cursor.canonicalize() {
                Ok(p) => break p,
                Err(_) => {
                    // pop 去掉最后一段；到根还失败就把错误抛出去
                    if !cursor.pop() {
                        return Err(io::Error::new(
                            io::ErrorKind::NotFound,
                            "路径无法解析",
                        ));
                    }
                }
            }
        };
        // 把不存在的那段尾巴拼回 realpath 结果上。
        // 注意：suffix 为空时不能 join——join("") 会追加空组件，
        // 路径带上尾部 `\`，读文件会得到 os error 267（目录名称无效）。
        let suffix = norm.strip_prefix(&cursor).unwrap_or(Path::new(""));
        let resolved = if suffix.as_os_str().is_empty() {
            real
        } else {
            real.join(suffix)
        };

        // realpath 之后再判一次 `.do`：AI 可能在工作区建 symlink 指向 `.do/`，
        // 词法路径看不出，但 realpath 会暴露出真实的 `.do` 段——天然挡住。
        if touches_do(&resolved) {
            return Ok(if for_write {
                Access::HiddenWrite
            } else {
                Access::Hidden
            });
        }

        // 第 4 层：根内校验。注意 resolved 里 real 部分已是真实路径，
        // suffix 部分词法上无 `..`（第 1 层已折叠），前缀比较即可。
        if !within_root(&resolved, &self.root) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "路径越出工作区",
            ));
        }
        Ok(Access::Allow(resolved))
    }

    /// 读类访问的便捷入口：命中 `.do` 时返回"文件不存在"同款 io 错误。
    /// 工具里多数场景用这个，省得各自 match Access。
    pub fn resolve_read(&self, path: &str) -> io::Result<PathBuf> {
        match self.resolve(path, false)? {
            Access::Allow(p) => Ok(p),
            // 关键：与真实不存在逐字一致——直接对路径做 canonicalize，
            // 让操作系统自己产生那个 NotFound 错误（文案随系统语言，
            // 但这正是"真实不存在"时程序会看到的那个错误）。
            // HiddenWrite 在此实际不可达：resolve_read 固定传 for_write=false，
            // resolve 只会产出 Hidden；合并写法只因两种隐藏的读语义相同
            Access::Hidden | Access::HiddenWrite => Err(not_found()),
        }
    }

    /// 写类访问的便捷入口：命中 `.do` 时返回 Windows 权限拒绝（os error 5）。
    pub fn resolve_write(&self, path: &str) -> io::Result<PathBuf> {
        match self.resolve(path, true)? {
            Access::Allow(p) => Ok(p),
            Access::Hidden => Err(not_found()),
            // from_raw_os_error(5)：ERROR_ACCESS_DENIED。
            // AI 只会看到"这路径写不进去"，看不出这里藏着配置目录。
            Access::HiddenWrite => Err(io::Error::from_raw_os_error(5)),
        }
    }

    /// 判断某条已归一化的路径是否落在 `.do` 内（ls/grep 过滤用）。
    /// 输入为绝对路径（调用方已 join 过根）。
    pub fn is_hidden_path(&self, abs: &Path) -> bool {
        touches_do(abs)
    }
}

/// 与"文件不存在"逐字相同的错误：ERROR_FILE_NOT_FOUND。
/// read 一个真实不存在的文件时，OS 报的就是这个码，
/// 因此 kind 与文案天然逐字一致（都随系统语言走）。
fn not_found() -> io::Error {
    io::Error::from_raw_os_error(2)
}

/// 第 4 层的根内比较：resolved 是否落在 root 之内。
/// Windows 文件系统大小写不敏感，但 Path::starts_with 逐字节比较——
/// 同一目录写成 `d:\doagent` 与 `D:\DoAgent` 会被误判越界，
/// 所以 Windows 下两边统一小写再按组件比较（is_hidden_name 同款归一）。
/// Unix 路径大小写敏感，逐字节比较才是正确语义，不能统一小写，
/// 用 cfg 分支区分（编译期选择，另一分支被死代码消除）。
fn within_root(resolved: &Path, root: &Path) -> bool {
    #[cfg(windows)]
    {
        Path::new(&resolved.to_string_lossy().to_lowercase())
            .starts_with(Path::new(&root.to_string_lossy().to_lowercase()))
    }
    #[cfg(not(windows))]
    {
        resolved.starts_with(root)
    }
}

/// 第 1 层：词法归一化。
/// 用 Path::components() 遍历——Rust 标准库已经把 `\`、`/`、重复分隔符
/// 都规整成 Component 枚举，我们只需处理 `.` 与 `..` 的折叠。
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {} // `.` 直接丢弃
            // `..`：能弹就弹（吃掉前一段），弹不动（已是根/盘符）则越界——
            // 保留它，让第 4 层的根内校验最终拒绝。
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            // `x => ...` 通配：其余 Component（RootDir/Prefix/Normal）原样保留
            x => out.push(x.as_os_str()),
        }
    }
    out
}

/// 判断单个文件/目录名是否是 `.do`（ls 过滤条目用）
pub fn is_hidden_name(name: &str) -> bool {
    name.to_lowercase().trim_end_matches([' ', '.']) == ".do"
}

/// 判断路径是否有任何一段属于 `.do` 目录。
/// Windows 文件名规则：大小写不敏感、尾部空格和点会被忽略，
/// 所以比较前统一 `小写 + 去尾部空格/点`。
fn touches_do(p: &Path) -> bool {
    p.components().any(|c| match c {
        // 闭包 `|c| ...` ≈ C# 的 lambda `c => ...`；
        // `.any(...)` 是迭代器适配器，≈ LINQ 的 Any()。
        Component::Normal(seg) => {
            let s = seg.to_string_lossy().to_lowercase();
            s.trim_end_matches([' ', '.']) == ".do"
        }
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 建一个临时工作区（测试辅助，不依赖第三方 crate）
    fn temp_ws(tag: &str) -> (Workspace, PathBuf) {
        let dir = std::env::temp_dir().join(format!("doagent-ws-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".do")).unwrap();
        (Workspace::new(&dir).unwrap(), dir)
    }

    #[test]
    fn dotdot_escape_rejected() {
        let (ws, dir) = temp_ws("w1");
        let r = ws.resolve("../evil.txt", false);
        assert!(matches!(r, Err(e) if e.kind() == io::ErrorKind::PermissionDenied));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn do_variants_hidden() {
        let (ws, dir) = temp_ws("w2");
        // 大小写 / 尾部空格 / 尾部点 / 嵌套，全部视为 .do
        for p in [
            ".do/config.json",
            ".DO/config.json",
            ".do /config.json",
            ".do./config.json",
            "a/../.do/config.json",
        ] {
            let r = ws.resolve_read(p);
            assert!(r.is_err(), "{p} 应被拒绝");
            assert_eq!(r.unwrap_err().kind(), io::ErrorKind::NotFound, "{p}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn do_read_matches_real_not_found() {
        let (ws, dir) = temp_ws("w3");
        // .do 内路径 与 真实不存在路径 的错误必须逐字一致。
        // 真实不存在的错误来自 OS 的 fs 调用，用它做基准。
        let hidden = ws.resolve_read(".do/config.json").unwrap_err();
        let missing = std::fs::read(dir.join("no-such-file.txt")).unwrap_err();
        assert_eq!(hidden.kind(), missing.kind());
        assert_eq!(hidden.to_string(), missing.to_string());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn do_write_is_os_error_5() {
        let (ws, dir) = temp_ws("w4");
        let e = ws.resolve_write(".do/config.json").unwrap_err();
        assert_eq!(e.raw_os_error(), Some(5));
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn normal_path_allowed() {
        let (ws, dir) = temp_ws("w5");
        std::fs::write(dir.join("ok.txt"), "hi").unwrap();
        let p = ws.resolve_read("ok.txt").unwrap();
        assert!(p.ends_with("ok.txt"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn case_mismatch_still_in_root() {
        // Windows：同一目录不同大小写不得误判"越出工作区"
        assert!(within_root(
            Path::new("d:\\doagent\\src\\main.rs"),
            Path::new("D:\\DoAgent"),
        ));
        // 但真正的越界仍须拒绝（大小写归一不能放宽边界）
        assert!(!within_root(
            Path::new("d:\\other\\x.txt"),
            Path::new("D:\\DoAgent"),
        ));
        // 前缀撞车也不能放行：doagent2 不是 doagent 的子路径
        assert!(!within_root(
            Path::new("D:\\DoAgent2\\x.txt"),
            Path::new("d:\\doagent"),
        ));
    }

    #[test]
    fn symlink_into_do_blocked() {
        // AI 在工作区建 symlink 指向 .do，顺着读必须被 realpath 挡住。
        // （Windows 建目录符号链接需要权限，失败则跳过本测试）
        let (ws, dir) = temp_ws("w6");
        let link = dir.join("link");
        if std::os::windows::fs::symlink_dir(dir.join(".do"), &link).is_ok() {
            let e = ws.resolve_read("link/config.json").unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::NotFound);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
