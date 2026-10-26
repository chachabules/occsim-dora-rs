//! OccSim → dora 桥接节点。
//!
//! 链路：网页(UE) --WS 10000--> zenohd(remote-api 插件) --7447--> 本节点
//!       --Arrow--> dora daemon --> 下游节点
//!
//! 用法：
//!   occsim-dora-rs                     在 dora 数据流内运行（由 daemon 拉起）
//!   occsim-dora-rs gen-dataflow        按 ports.yaml 生成 dataflow.yml

mod catalog;
mod control;
mod dataflow_gen;
mod decode;
mod encode;
mod pipeline;
mod routing;
mod zenohd;

use std::path::PathBuf;

use anyhow::{anyhow, bail, Result};
use dora_node_api::DoraNode;
use tracing_subscriber::EnvFilter;

use crate::catalog::Catalog;

struct Args {
    command: String,
    ports: PathBuf,
    dataflow: PathBuf,
    zenohd: PathBuf,
    plugin_dir: PathBuf,
    router_config: PathBuf,
    /// 生成 dataflow.yml 时写入的节点可执行路径。
    exe: String,
    /// 生成 dataflow.yml 时写入的构建命令。
    build: String,
    /// 生成 dataflow.yml 时附加显示节点，值为显示程序入口（如 `display-py/main.py`）。
    display: Option<PathBuf>,
    /// 显示节点使用的 Python 解释器（必须是装好 dora / PySide6 的环境）。
    python: PathBuf,
    /// 关掉控制接线（只桥接观测，不生成 cmd/cmd_token/cmd_ack）。
    no_control: bool,
    /// 运行期显式指定网页的运行前缀，跳过 `<prefix>/active` 发现。
    run_prefix: Option<String>,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            command: "run".into(),
            ports: PathBuf::from("ports.yaml"),
            dataflow: PathBuf::from("dataflow.yml"),
            zenohd: PathBuf::from("C:/tools/zenoh/zenohd.exe"),
            plugin_dir: PathBuf::from("C:/tools/zenoh-remote-api"),
            router_config: PathBuf::from("../integration/router.json5"),
            exe: "target/release/occsim-dora-rs.exe".into(),
            build: "cargo build --release".into(),
            display: None,
            python: PathBuf::from("D:/app/anaconda3/envs/occsim/python.exe"),
            no_control: false,
            run_prefix: None,
        }
    }
}

/// 取下一个参数作为路径值。
fn next_path(items: &mut impl Iterator<Item = String>, name: &str) -> Result<PathBuf> {
    items
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("{name} 缺少参数值"))
}

fn parse_args() -> Result<Args> {
    let mut args = Args::default();
    let mut items = std::env::args().skip(1);
    while let Some(item) = items.next() {
        match item.as_str() {
            "run" | "gen-dataflow" => args.command = item,
            "--ports" => args.ports = next_path(&mut items, "--ports")?,
            "--dataflow" => args.dataflow = next_path(&mut items, "--dataflow")?,
            "--zenohd" => args.zenohd = next_path(&mut items, "--zenohd")?,
            "--plugin-dir" => args.plugin_dir = next_path(&mut items, "--plugin-dir")?,
            "--router-config" => args.router_config = next_path(&mut items, "--router-config")?,
            "--exe" => {
                args.exe = items.next().ok_or_else(|| anyhow!("--exe 缺少参数值"))?;
            }
            "--build" => {
                args.build = items.next().ok_or_else(|| anyhow!("--build 缺少参数值"))?;
            }
            "--display" => args.display = Some(next_path(&mut items, "--display")?),
            "--python" => args.python = next_path(&mut items, "--python")?,
            "--no-control" => args.no_control = true,
            "--run-prefix" => {
                args.run_prefix = Some(
                    items
                        .next()
                        .ok_or_else(|| anyhow!("--run-prefix 缺少参数值"))?,
                );
            }
            "-h" | "--help" => {
                println!(
                    "用法: occsim-dora-rs [run|gen-dataflow] [--ports P] [--dataflow P]\n\
                    \x20                 [--zenohd P] [--plugin-dir P] [--router-config P]\n\
                    \x20                 [--exe P] [--build CMD]\n\
                    \x20                 [--display SCRIPT] [--python EXE]\n\
                    \x20                 [gen-dataflow] [--no-control]\n\
                    \x20                 [run] [--run-prefix usv/v1/...]\n\n\
                    gen-dataflow 传 --display 时会在同一份数据流里追加显示节点\n\
                    occsim-display，并按控制回路接线（cmd / cmd_token / cmd_ack）；\n\
                    --no-control 关掉控制接线。\n\
                    run 时控制链路常开：令牌由操控页通过 cmd_token 端口下发、只存内存，\n\
                    没收到令牌前所有指令都回 rejected；--run-prefix 可跳过自动发现。"
                );
                std::process::exit(0);
            }
            other => bail!("未知参数：{other}"),
        }
    }
    Ok(args)
}

/// zenoh 的错误是 `Box<dyn Error + Send + Sync>`，不实现 `StdError`，无法直接用
/// anyhow 的 `.context()`。
fn wrap<T>(result: zenoh::Result<T>, what: &str) -> Result<T> {
    result.map_err(|error| anyhow!("{what}: {error}"))
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,zenoh=warn".into()),
        )
        .init();

    let args = parse_args()?;
    let catalog = Catalog::load(&args.ports)?;

    if args.command == "gen-dataflow" {
        let python = args.python.to_string_lossy().into_owned();
        let script = args
            .display
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned());
        let display = script.as_deref().map(|script| dataflow_gen::DisplayOptions {
            python: &python,
            script,
        });
        // 控制接线只在带显示节点时有意义（指令的上游就是操控页）。
        let wired = display.is_some() && !args.no_control;
        let yaml = dataflow_gen::render(
            &catalog,
            &args.exe,
            &args.build,
            display.as_ref(),
            wired,
        );
        std::fs::write(&args.dataflow, &yaml)
            .map_err(|e| anyhow!("写入 {} 失败: {e}", args.dataflow.display()))?;
        let ports = if catalog.is_empty() {
            format!("类型端口 {} 路（ports.yaml 为空）", routing::TYPE_PORTS.len())
        } else {
            format!("实例端口 {} 路（ports.yaml 声明）", catalog.instance_ports().len())
        };
        let extra = match (script, wired) {
            (Some(script), true) => format!(
                "，含显示节点 occsim-display（{script}）与控制回路（令牌由操控页经 {} 下发）",
                control::TOKEN_PORT
            ),
            (Some(script), false) => format!("，含显示节点 occsim-display（{script}），已关闭控制接线"),
            (None, _) => String::new(),
        };
        println!("已生成 {}：{ports}{extra}", args.dataflow.display());
        return Ok(());
    }

    if catalog.is_empty() {
        tracing::info!("ports.yaml 为空，仅使用 8 路类型兜底端口");
    } else {
        tracing::info!(
            "仅发布 ports.yaml 声明的 {} 路实例端口",
            catalog.instance_ports().len()
        );
    }

    let _router = zenohd::ensure(&args.zenohd, &args.router_config, &args.plugin_dir)?;

    let mut config = zenoh::Config::default();
    wrap(config.insert_json5("mode", "\"client\""), "设置 client 模式失败")?;
    wrap(
        config.insert_json5(
            "connect/endpoints",
            &format!("[\"tcp/{}\"]", zenohd::ROUTER_ADDR),
        ),
        "设置连接端点失败",
    )?;
    wrap(
        config.insert_json5("scouting/multicast/enabled", "false"),
        "关闭组播发现失败",
    )?;
    let session = wrap(zenoh::open(config).await, "连接 zenoh 路由器失败")?;

    let (mut node, mut events) = DoraNode::init_from_env()
        .map_err(|e| anyhow!("dora 节点初始化失败（本程序需由 dora 数据流启动）: {e}"))?;

    // 控制链路常开：令牌由操控页通过 `cmd_token` 端口下发，桥接只存内存。
    // 没收到令牌前所有指令都回 rejected，观测照常。
    let control = control::start(session.clone(), args.run_prefix.clone());
    tracing::info!(
        "控制链路已启动：等待操控页下发临时令牌（{}）",
        control::TOKEN_PORT
    );

    pipeline::run(&session, &mut node, &mut events, &catalog, control).await?;
    Ok(())
}
