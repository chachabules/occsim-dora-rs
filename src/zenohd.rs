//! 拉起或复用 zenohd 路由器。
//!
//! 浏览器（zenoh-ts）只能通过 `zenoh-plugin-remote-api` 的 WebSocket 端点接入，
//! 而该插件与宿主存在 rustc 版本 / zenoh feature 集合的硬校验：
//! 本机 `C:\tools\zenoh-remote-api` 的 DLL 是 rustc 1.97.1 编的，直接嵌进
//! 我们 1.98.1 的进程会被拒绝加载。因此改为拉起同源预编译的 zenohd 子进程，
//! 业务节点以 client 身份连 7447 —— 数据面行为完全一致，不受 ABI 版本约束。

use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

/// 路由器监听地址，与 `integration/router.json5` 一致。
pub const ROUTER_ADDR: &str = "127.0.0.1:7447";

/// 持有子进程，Drop 时回收，避免残留 zenohd。
pub struct RouterGuard {
    child: Option<Child>,
}

impl Drop for RouterGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// 7447 是否已在监听。
pub fn port_open() -> bool {
    let addr: SocketAddr = ROUTER_ADDR
        .parse()
        .expect("ROUTER_ADDR 是写死的合法地址");
    TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok()
}

/// 确保路由器可用：已在跑就直接复用，否则拉子进程并等端口就绪。
pub fn ensure(zenohd: &Path, config: &Path, plugin_dir: &Path) -> Result<RouterGuard> {
    if port_open() {
        tracing::info!("复用已在运行的 zenoh 路由器（{ROUTER_ADDR}）");
        return Ok(RouterGuard { child: None });
    }
    if !zenohd.exists() {
        bail!("找不到 zenohd：{}", zenohd.display());
    }
    if !config.exists() {
        bail!("找不到路由器配置：{}", config.display());
    }
    if !plugin_dir.exists() {
        bail!("找不到 remote-api 插件目录：{}", plugin_dir.display());
    }

    let child = Command::new(zenohd)
        .arg("-c")
        .arg(config)
        .arg("--plugin-search-dir")
        .arg(plugin_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("启动 zenohd 失败：{}", zenohd.display()))?;
    let guard = RouterGuard { child: Some(child) };

    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if port_open() {
            tracing::info!("zenohd 已就绪（{ROUTER_ADDR} + WS 10000）");
            return Ok(guard);
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    bail!("zenohd 15 秒内未监听 {ROUTER_ADDR}；请确认 7447/10000 未被 sensors.py 或其它 zenoh 实例占用")
}
