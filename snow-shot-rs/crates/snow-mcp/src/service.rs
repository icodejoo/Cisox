//! 按需启动的 MCP 服务：命名管道 + 描述符 + 令牌（仅 Windows）。
//!
//! 只有启动后才占用线程与管道；丢弃即断开连接、关闭管道并删除描述符，不留后台线程。

use crate::auth::{AuthGate, Token};
use crate::descriptor::{self, Descriptor};
use crate::registry::{Registry, Scope};
use crate::session::{Session, Shared};
use crate::tools::{AppBackend, ServerInfo};
use snow_platform::single_instance::line_pipe::{
    LinePipeServer, LineReply, LineSession, SessionFactory, pipe_name_for,
};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// 管道名里的应用标识。
pub const PIPE_APP_ID: &str = "cisox-mcp";
/// 代数标识的随机字节数。
const GENERATION_BYTES: usize = 8;

/// 服务启动参数。
pub struct ServiceConfig {
    /// 应用数据根目录（描述符写在其下 `mcp/`）。
    pub data_root: PathBuf,
    /// 服务信息。
    pub server: ServerInfo,
    /// 授权的权限域。
    pub granted: Vec<Scope>,
    /// 管道名；`None` 用当前用户的默认名（测试可指定唯一名）。
    pub pipe_name: Option<String>,
}

/// 运行中的 MCP 服务；丢弃即关闭。
pub struct McpService {
    /// 管道服务端（丢弃时先断开连接）。
    server: Option<LinePipeServer>,
    /// 描述符路径。
    descriptor_path: PathBuf,
    /// 本次启用的代数标识。
    generation: String,
    /// 管道完整名。
    pipe_name: String,
}

/// 把 [`Session`] 适配成管道会话；鉴权失败的退避在这里睡眠。
struct PipeSession(Session);

impl LineSession for PipeSession {
    /// 交给会话处理，必要时先按退避时长等待再回应。
    fn on_line(&mut self, line: &str) -> LineReply {
        let outcome = self.0.handle_line(line);
        if !outcome.delay.is_zero() {
            std::thread::sleep(outcome.delay);
        }
        LineReply {
            text: outcome.reply,
            close: outcome.close,
        }
    }
}

impl McpService {
    /// 生成令牌、写描述符并开始监听。
    ///
    /// # 参数
    /// - `config`：启动参数。
    /// - `backend`：应用数据来源（须线程安全）。
    ///
    /// # 返回
    /// 运行中的服务；任一步失败返回原因（不含令牌），并清理已写出的描述符。
    pub fn start(config: ServiceConfig, backend: Arc<dyn AppBackend>) -> Result<Self, String> {
        let token = Token::generate_system()?;
        let mut gen_bytes = [0u8; GENERATION_BYTES];
        snow_platform::random::fill_random(&mut gen_bytes)?;
        let generation: String = gen_bytes.iter().map(|b| format!("{b:02x}")).collect();
        let pipe_name = match config.pipe_name {
            Some(name) => name,
            None => pipe_name_for(PIPE_APP_ID)?,
        };
        let descriptor_path = descriptor::write(
            &config.data_root,
            &Descriptor {
                pipe: &pipe_name,
                token: token.expose(),
                generation: &generation,
                scopes: &config.granted,
            },
        )?;
        let shared = Arc::new(Shared {
            token,
            registry: Registry::build(),
            backend,
            server: config.server,
            granted: config.granted,
            gate: Mutex::new(AuthGate::default()),
        });
        let factory: SessionFactory =
            Box::new(move || Box::new(PipeSession(Session::new(Arc::clone(&shared)))));
        match LinePipeServer::start(&pipe_name, factory) {
            Ok(server) => {
                tracing::info!(pipe = %pipe_name, "MCP 服务已启动");
                Ok(Self {
                    server: Some(server),
                    descriptor_path,
                    generation,
                    pipe_name,
                })
            }
            Err(e) => {
                descriptor::remove_if_generation(&descriptor_path, &generation);
                Err(e)
            }
        }
    }

    /// 管道完整名。
    pub fn pipe_name(&self) -> &str {
        &self.pipe_name
    }

    /// 描述符路径。
    pub fn descriptor_path(&self) -> &Path {
        &self.descriptor_path
    }
}

impl Drop for McpService {
    /// 先断开管道与连接，再删描述符（令牌随之作废）。
    fn drop(&mut self) {
        self.server.take();
        descriptor::remove_if_generation(&self.descriptor_path, &self.generation);
        tracing::info!("MCP 服务已关闭");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::PROTOCOL_VERSION;
    use crate::tools::FakeBackend;
    use serde_json::{Value, json};
    use snow_platform::single_instance::line_pipe::LinePipeClient;
    use std::time::Duration;

    /// 读超时。
    const WAIT: Duration = Duration::from_secs(3);

    /// 发送一条请求并解析回应。
    fn rpc(client: &mut LinePipeClient, msg: Value) -> Value {
        client.send_line(&msg.to_string()).unwrap();
        serde_json::from_str(&client.read_line(WAIT).unwrap()).unwrap()
    }

    /// 真实管道端到端：描述符 → 鉴权 → 握手 → 调用；错误令牌被断开；关闭后描述符消失、管道不可连。
    #[test]
    fn end_to_end_over_real_pipe() {
        let root = std::env::temp_dir().join(format!("cisox-mcp-svc-{}", std::process::id()));
        let pipe = format!(r"\\.\pipe\cisox-mcp-test-{}", std::process::id());
        let service = McpService::start(
            ServiceConfig {
                data_root: root.clone(),
                server: ServerInfo {
                    name: "t".into(),
                    version: "1".into(),
                },
                granted: Scope::DEFAULT_GRANTED.to_vec(),
                pipe_name: Some(pipe.clone()),
            },
            Arc::new(FakeBackend),
        )
        .unwrap();
        let desc: Value =
            serde_json::from_str(&std::fs::read_to_string(service.descriptor_path()).unwrap())
                .unwrap();
        assert_eq!(desc["pipe"], pipe.as_str());
        let token = desc["token"].as_str().unwrap().to_string();
        assert_eq!(token.len(), 64);

        // 错误令牌：统一未授权并断开
        let mut bad = LinePipeClient::connect(&pipe).unwrap();
        let v = rpc(
            &mut bad,
            json!({"jsonrpc":"2.0","id":1,"method":"cisox/auth","params":{"token":"nope"}}),
        );
        assert_eq!(v["error"]["code"], crate::protocol::ERR_UNAUTHORIZED);
        assert!(bad.read_line(Duration::from_millis(500)).is_err());
        drop(bad);

        let mut c = LinePipeClient::connect(&pipe).unwrap();
        let v = rpc(
            &mut c,
            json!({"jsonrpc":"2.0","id":1,"method":"cisox/auth","params":{"token":token}}),
        );
        assert_eq!(v["result"]["authenticated"], true);
        let v = rpc(
            &mut c,
            json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":PROTOCOL_VERSION}}),
        );
        assert_eq!(v["result"]["serverInfo"]["name"], "t");
        let v = rpc(
            &mut c,
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"snow_shot_mcp_status","arguments":{}}}),
        );
        assert_eq!(v["result"]["structuredContent"]["tools_total"], 101);
        drop(c);

        let path = service.descriptor_path().to_path_buf();
        drop(service);
        assert!(!path.exists());
        assert!(LinePipeClient::connect(&pipe).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
