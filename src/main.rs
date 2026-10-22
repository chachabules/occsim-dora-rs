//! OccSim → dora 桥接节点。
//!
//! 链路：网页(UE) --WS 10000--> zenohd(remote-api 插件) --7447--> 本节点
//!       --Arrow--> dora daemon --> 下游节点
//!
//! 用法：
//!   occsim-dora-rs                     在 dora 数据流内运行（由 daemon 拉起）
//!   occsim-dora-rs gen-dataflow        按 ports.yaml 生成 dataflow.yml

mod catalog;
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
            "-h" | "--help" => {
                println!(
                    "用法: occsim-dora-rs [run|gen-dataflow] [--ports P] [--dataflow P]\n\
                     \x20                 [--zenohd P] [--plugin-dir P] [--router-config P]\n\
                     \x20                 [--exe P] [--build CMD]"
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
        let yaml = dataflow_gen::render(&catalog, &args.exe, &args.build);
        std::fs::write(&args.dataflow, &yaml)
            .map_err(|e| anyhow!("写入 {} 失败: {e}", args.dataflow.display()))?;
        if catalog.is_empty() {
            println!(
                "已生成 {}：ports.yaml 为空，仅类型端口 {} 路",
                args.dataflow.display(),
                routing::TYPE_PORTS.len()
            );
        } else {
            println!(
                "已生成 {}：实例端口 {} 路（ports.yaml 声明）",
                args.dataflow.display(),
                catalog.instance_ports().len()
            );
        }
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

    pipeline::run(&session, &mut node, &mut events, &catalog).await?;
    Ok(())
}
