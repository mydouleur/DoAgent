//! 集成测试：从 crate 外部验证公开 API（workspace 守卫 + config）。
//! 集成测试只能访问 pub 接口——这本身就是对"最小公共 API"的一次检查。

use core::config::Config;
use core::workspace::Workspace;

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("doagent-it-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn config_roundtrip() {
    let dir = temp_dir("cfg");
    let layer = Config::load_workspace(&dir);
    assert!(layer.url.is_empty()); // 层文件缺失时全空，默认值只在合并时补
    let merged = Config::load_merged(&dir, None);
    assert_eq!(merged.url, "https://api.openai.com/v1"); // 缺省 url
    let mut cfg = Config::default();
    cfg.set("model", "gpt-4o").unwrap();
    // start 值允许含空格
    cfg.set("start", "cargo build --release").unwrap();
    cfg.save(&dir).unwrap();
    let back = Config::load_workspace(&dir);
    assert_eq!(back.model, "gpt-4o");
    assert_eq!(back.start, "cargo build --release");
    assert!(cfg.set("bogus", "x").is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn nested_do_is_hidden() {
    let dir = temp_dir("nested");
    std::fs::create_dir_all(dir.join("a/.do")).unwrap();
    let ws = Workspace::new(&dir).unwrap();
    // 嵌套 .do 同样隐形
    assert!(ws.resolve_read("a/.do/x.txt").is_err());
    assert!(ws.resolve_read("a/ok.txt").is_ok());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn write_into_do_is_permission_denied() {
    let dir = temp_dir("w");
    let ws = Workspace::new(&dir).unwrap();
    let e = ws.resolve_write(".do/evil.json").unwrap_err();
    assert_eq!(e.raw_os_error(), Some(5)); // Windows: Access is denied
    let _ = std::fs::remove_dir_all(&dir);
}
