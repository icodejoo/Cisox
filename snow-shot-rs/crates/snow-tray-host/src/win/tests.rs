//! 真实平台的离屏集成测试：真起（批处理假冒的）UI 进程、真建会话管道，不创建托盘与热键。

use super::*;
use snow_host_protocol::frame::decode_line;
use snow_host_protocol::message::UiMessage;
use snow_platform::single_instance::session_pipe::SessionPipeClient;
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

/// 事件等待上限。
const WAIT: Duration = Duration::from_secs(10);

/// 测试用临时目录（每个测试一个）。
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("snow-tray-host-win-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// 写一个假 UI 批处理：把收到的命令行参数写进 `args.txt`，然后执行 `tail`（可用来让它停留一会儿）。
fn fake_ui(dir: &std::path::Path, tail: &str) -> PathBuf {
    let bat = dir.join("fake-ui.bat");
    let args = dir.join("args.txt");
    let body = format!("@echo off\r\necho %*> \"{}\"\r\n{tail}\r\n", args.display());
    std::fs::write(&bat, body).unwrap();
    bat
}

/// 建一个指向假 UI 的平台。
fn platform_with(ui: PathBuf, root: PathBuf) -> (WinPlatform, Receiver<Event>) {
    let (tx, rx) = channel();
    let mut platform = WinPlatform::new(tx, root, PathBuf::from("snow-shot.exe"));
    platform.ui_exe = ui;
    (platform, rx)
}

/// 等到满足条件的事件（其余事件丢弃）。
fn wait_for(rx: &Receiver<Event>, pick: impl Fn(&Event) -> bool) -> Event {
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        let event = rx.recv_timeout(left).expect("等待事件超时");
        if pick(&event) {
            return event;
        }
    }
}

/// 等假 UI 把参数写出来，取出 `--session` 后面的会话名。
fn session_from_args(dir: &std::path::Path) -> String {
    let args_path = dir.join("args.txt");
    let deadline = std::time::Instant::now() + WAIT;
    let args = loop {
        if let Ok(text) = std::fs::read_to_string(&args_path)
            && text.contains("--token")
        {
            break text;
        }
        assert!(std::time::Instant::now() < deadline, "假 UI 没写出参数");
        std::thread::sleep(Duration::from_millis(50));
    };
    args.split_whitespace()
        .skip_while(|a| *a != "--session")
        .nth(1)
        .expect("参数里应有会话名")
        .to_string()
}

/// 拉起：参数是 `--ui --session <名> --token <64 位十六进制>`；进程退出后投递 `UiExited`。
#[test]
fn spawn_passes_launch_args_and_reports_exit() {
    let dir = scratch("args");
    let (mut platform, rx) = platform_with(fake_ui(&dir, ""), dir.clone());
    let spawned = platform.spawn_ui(7, UiHint::None).unwrap();
    assert!(spawned.pid > 0);
    assert!(snow_host_protocol::message::is_valid_token(&spawned.token));
    wait_for(&rx, |e| matches!(e, Event::UiExited { generation: 7 }));
    let args = std::fs::read_to_string(dir.join("args.txt")).unwrap();
    assert!(
        args.contains("--ui --session cisox.snow_shot.session."),
        "{args}"
    );
    assert!(
        args.contains(&format!("--token {}", spawned.token)),
        "{args}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 启动提示：带 `Dictation` 时命令行末尾多 `--hint dictation`，没有提示时不带这个开关。
#[test]
fn spawn_adds_hint_arg_only_when_hinted() {
    let dir = scratch("hint");
    let (mut platform, rx) = platform_with(fake_ui(&dir, ""), dir.clone());
    platform.spawn_ui(1, UiHint::Dictation).unwrap();
    wait_for(&rx, |e| matches!(e, Event::UiExited { generation: 1 }));
    let args = std::fs::read_to_string(dir.join("args.txt")).unwrap();
    assert!(args.contains("--hint dictation"), "{args}");
    let _ = std::fs::remove_dir_all(&dir);

    let dir = scratch("nohint");
    let (mut platform, rx) = platform_with(fake_ui(&dir, ""), dir.clone());
    platform.spawn_ui(2, UiHint::None).unwrap();
    wait_for(&rx, |e| matches!(e, Event::UiExited { generation: 2 }));
    let args = std::fs::read_to_string(dir.join("args.txt")).unwrap();
    assert!(!args.contains("--hint"), "{args}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 会话管道：UI 一侧连上后，宿主收到连接事件（带客户端 PID）与消息行；宿主写的消息 UI 读得到；
/// 过期代数的写入被拒绝；关闭会话后再写失败。
#[test]
fn session_pipe_carries_messages_both_ways() {
    let dir = scratch("pipe");
    // 假 UI 停留约 3 秒（`ping`），让管道在测试期间一直可连
    let (mut platform, rx) = platform_with(fake_ui(&dir, "ping -n 4 127.0.0.1 >NUL"), dir.clone());
    let spawned = platform.spawn_ui(1, UiHint::None).unwrap();
    let session = session_from_args(&dir);
    let mut client = SessionPipeClient::connect(&pipe_name_for(&session).unwrap()).unwrap();
    assert_eq!(
        wait_for(&rx, |e| matches!(e, Event::SessionConnected { .. })),
        Event::SessionConnected {
            generation: 1,
            client_pid: std::process::id()
        }
    );
    // UI → 宿主
    let hello = encode_line(&UiMessage::Hello {
        pid: spawned.pid,
        proto: snow_host_protocol::PROTOCOL_VERSION,
        token: spawned.token.clone(),
    })
    .unwrap();
    client.writer().send_line(hello.trim_end()).unwrap();
    let Event::SessionLine { generation, line } =
        wait_for(&rx, |e| matches!(e, Event::SessionLine { .. }))
    else {
        unreachable!()
    };
    assert_eq!(generation, 1);
    assert!(matches!(
        decode_line::<UiMessage>(&line).unwrap(),
        UiMessage::Hello { .. }
    ));
    // 宿主 → UI
    platform.send(1, &HostMessage::ExitOk).unwrap();
    assert_eq!(
        client.read_line(Some(WAIT)).unwrap().as_deref(),
        Some("{\"type\":\"exit_ok\"}")
    );
    assert!(
        platform.send(2, &HostMessage::ExitOk).is_err(),
        "过期代数的写入必须被拒绝"
    );
    platform.close_session(1);
    assert!(platform.send(1, &HostMessage::ExitOk).is_err());
    wait_for(&rx, |e| matches!(e, Event::UiExited { generation: 1 }));
    let _ = std::fs::remove_dir_all(&dir);
}

/// 强制结束：只动当前会话的进程（别的 PID 被忽略）；结束后 `UiExited` 到达。
#[test]
fn kill_terminates_only_the_session_process() {
    let dir = scratch("kill");
    // 假 UI 停留约 30 秒，靠 kill 提前结束
    let (mut platform, rx) = platform_with(fake_ui(&dir, "ping -n 30 127.0.0.1 >NUL"), dir.clone());
    let spawned = platform.spawn_ui(3, UiHint::None).unwrap();
    platform.kill_ui(spawned.pid.wrapping_add(1));
    assert!(
        rx.recv_timeout(Duration::from_millis(500)).is_err(),
        "结束一个不在会话里的 PID 不得有任何效果"
    );
    platform.kill_ui(spawned.pid);
    wait_for(&rx, |e| matches!(e, Event::UiExited { generation: 3 }));
    let _ = std::fs::remove_dir_all(&dir);
}

/// 找不到 UI exe：拉起报错，不留会话（状态机据此走“启动失败”分支）。
#[test]
fn missing_ui_exe_fails_cleanly() {
    let dir = scratch("missing");
    let (mut platform, _rx) = platform_with(dir.join("no-such-ui.exe"), dir.clone());
    let error = platform.spawn_ui(1, UiHint::None).unwrap_err();
    assert!(error.contains("no-such-ui.exe"), "{error}");
    assert!(platform.session.is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

/// 数据根下没有 `host/spec.json` 时如实报 Missing；写入后能读到。
#[test]
fn read_spec_follows_the_data_root() {
    let dir = scratch("spec");
    let (mut platform, _rx) = platform_with(dir.join("x.exe"), dir.clone());
    assert!(matches!(platform.read_spec(), Err(SpecFileError::Missing)));
    let spec = crate::view::fallback_spec();
    let path = snow_data_paths::host_spec_path(&dir);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, spec.to_json().unwrap()).unwrap();
    assert_eq!(platform.read_spec().unwrap(), spec);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 随机十六进制：长度翻倍、两次不同、符合令牌格式。
#[test]
fn random_hex_is_hex_and_unique() {
    let a = random_hex(TOKEN_BYTES).unwrap();
    let b = random_hex(TOKEN_BYTES).unwrap();
    assert_ne!(a, b);
    assert!(snow_host_protocol::message::is_valid_token(&a));
    assert_eq!(
        random_hex(LAUNCH_ID_BYTES).unwrap().len(),
        LAUNCH_ID_BYTES * 2
    );
}

/// 无窗口更新检查：真起假 UI，参数是 `--headless update-check`；标准输出第一行作为提示文案随
/// `HeadlessDone` 带回；进程没输出时提示为 `None`。
#[test]
fn headless_update_check_reports_notice_from_stdout() {
    let dir = scratch("headless-update");
    let (mut platform, rx) =
        platform_with(fake_ui(&dir, "echo Update 9.9.9 available"), dir.clone());
    platform.spawn_headless(&HeadlessJob::UpdateCheck).unwrap();
    let Event::HeadlessDone { kind, notice } =
        wait_for(&rx, |e| matches!(e, Event::HeadlessDone { .. }))
    else {
        unreachable!()
    };
    assert_eq!(kind, HeadlessKind::UpdateCheck);
    assert_eq!(notice.as_deref(), Some("Update 9.9.9 available"));
    let args = std::fs::read_to_string(dir.join("args.txt")).unwrap();
    assert_eq!(args.trim(), "--headless update-check");
    let _ = std::fs::remove_dir_all(&dir);

    let dir = scratch("headless-update-quiet");
    let (mut platform, rx) = platform_with(fake_ui(&dir, ""), dir.clone());
    platform.spawn_headless(&HeadlessJob::UpdateCheck).unwrap();
    let Event::HeadlessDone { notice, .. } =
        wait_for(&rx, |e| matches!(e, Event::HeadlessDone { .. }))
    else {
        unreachable!()
    };
    assert_eq!(notice, None);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 无窗口改配置：参数带 `--key` / `--value`（JSON）；结束后投递 `HeadlessDone`，不带提示。
#[test]
fn headless_set_config_passes_key_and_value() {
    let dir = scratch("headless-config");
    let (mut platform, rx) = platform_with(fake_ui(&dir, ""), dir.clone());
    let job = HeadlessJob::SetConfig {
        key: "global_shortcuts/disable_on_focused_fullscreen_window".into(),
        value: serde_json::json!(true),
    };
    platform.spawn_headless(&job).unwrap();
    let Event::HeadlessDone { kind, notice } =
        wait_for(&rx, |e| matches!(e, Event::HeadlessDone { .. }))
    else {
        unreachable!()
    };
    assert_eq!((kind, notice), (HeadlessKind::SetConfig, None));
    let args = std::fs::read_to_string(dir.join("args.txt")).unwrap();
    assert_eq!(
        args.trim(),
        "--headless set-config --key global_shortcuts/disable_on_focused_fullscreen_window --value true"
    );
    // 生成快照有自己的路径，通用入口拒绝它
    assert!(platform.spawn_headless(&HeadlessJob::EmitSpec).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

/// 提示读取：取第一行并去空白；空输出 / 空行 / 非法 UTF-8 都不 panic；超长输出被截到上限。
#[test]
fn read_notice_takes_first_line_within_limit() {
    assert_eq!(
        read_notice("有新版本 1.2\r\nignored".as_bytes()),
        Some("有新版本 1.2".to_string())
    );
    assert_eq!(read_notice("".as_bytes()), None);
    assert_eq!(read_notice("\n\n".as_bytes()), None);
    assert_eq!(
        read_notice(&b"\xff\xfe ok\n"[..]).as_deref(),
        Some("\u{fffd}\u{fffd} ok")
    );
    let long = "x".repeat(MAX_NOTICE_BYTES * 3);
    assert_eq!(
        read_notice(long.as_bytes()).unwrap().len(),
        MAX_NOTICE_BYTES
    );
}

/// 鼠标手势：没有有效绑定不装钩子（`mouse` 保持为空）；取消手势在没有钩子时也安全。
#[test]
fn mouse_hook_is_not_installed_without_bindings() {
    let dir = scratch("mouse");
    let (mut platform, _rx) = platform_with(dir.join("x.exe"), dir.clone());
    platform.set_mouse_bindings(&[]).unwrap();
    // 全是无效项（没有修饰键）等同于没有绑定
    platform
        .set_mouse_bindings(&[MouseBindingSpec {
            action: "screenshot_copy".into(),
            modifiers: vec![],
            button: "left_drag".into(),
        }])
        .unwrap();
    assert!(platform.mouse.is_none());
    platform.cancel_gesture();
    assert!(platform.unix_now() > 1_600_000_000);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 工作集修剪与前台授权不会失败崩溃（失败只记日志）。
#[test]
fn trim_and_grant_are_safe() {
    let dir = scratch("misc");
    let (mut platform, _rx) = platform_with(dir.join("x.exe"), dir.clone());
    platform.trim_working_set();
    platform.grant_foreground(std::process::id());
    // 前台探测只要调用安全即可（结果取决于当前桌面）
    let _ = platform.fullscreen_focused();
    let _ = std::fs::remove_dir_all(&dir);
}
