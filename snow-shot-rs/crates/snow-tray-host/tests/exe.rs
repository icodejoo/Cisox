//! 宿主 exe（`cisox.exe`）的离屏集成测试：只跑不创建窗口 / 线程的入口，
//! 不会抢单实例互斥体，也不会碰运行中的实例。
#![cfg(windows)]

use std::process::Command;

/// 被测 exe（cargo 在构建集成测试时已把二进制构建好）。
const EXE: &str = env!("CARGO_BIN_EXE_cisox");

/// 进程加固在一个全新进程里确实生效：扩展点禁用策略读回为开启，`ImmDisableIME(-1)` 成功。
/// 这是“`main` 第一行就加固”的回归测试：加固失效（比如被挪到窗口创建之后、或 API 调用错误）时，
/// 宿主一弹托盘菜单就会被输入法 DLL 注入，私有内存从 3MB 涨到 36MB。
#[test]
fn hardening_is_effective_in_a_fresh_process() {
    let output = Command::new(EXE)
        .arg("--self-check-harden")
        .output()
        .expect("启动宿主 exe 失败");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "加固自检失败: {stdout} {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("extension_points_disabled=true"),
        "{stdout}"
    );
    assert!(stdout.contains("ime_disabled=true"), "{stdout}");
}

/// 未知参数：退出码 2，stderr 里有用法提示（`--ui` 是 UI 进程的开关，宿主不认）。
#[test]
fn unknown_arguments_print_usage_and_exit_with_code_2() {
    for bad in ["--bogus", "--ui"] {
        let output = Command::new(EXE).arg(bad).output().unwrap();
        assert_eq!(output.status.code(), Some(2), "{bad}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("用法"), "{bad}: {stderr}");
    }
    let output = Command::new(EXE).args(["--cmd", "boom"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2));
}

/// `--resident` 回退：找不到 UI exe 时退出码 1 并说明原因，不留进程。
#[test]
fn resident_fallback_reports_a_missing_ui_exe() {
    let missing = std::env::temp_dir().join("snow-tray-host-no-such-ui.exe");
    let output = Command::new(EXE)
        .arg("--resident")
        .env("SNOW_UI_EXE", &missing)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("no-such-ui.exe"));
}

/// `--resident` 回退：UI exe 存在时带着 `--resident` 拉起它（用批处理假冒，把参数写进文件）。
#[test]
fn resident_fallback_launches_the_ui_exe_with_the_flag() {
    let dir = std::env::temp_dir().join(format!("snow-tray-host-resident-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("args.txt");
    let bat = dir.join("fake-ui.bat");
    std::fs::write(
        &bat,
        format!("@echo off\r\necho %*> \"{}\"\r\n", out.display()),
    )
    .unwrap();
    let status = Command::new(EXE)
        .arg("--resident")
        .env("SNOW_UI_EXE", &bat)
        .status()
        .unwrap();
    assert!(status.success());
    // 批处理由子进程异步执行：等它写出结果
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let text = loop {
        if let Ok(text) = std::fs::read_to_string(&out)
            && !text.trim().is_empty()
        {
            break text;
        }
        assert!(std::time::Instant::now() < deadline, "假 UI 没有被拉起");
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    assert_eq!(text.trim(), "--resident");
    let _ = std::fs::remove_dir_all(&dir);
}
