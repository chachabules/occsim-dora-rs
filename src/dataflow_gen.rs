//! 从实例清单生成 `dataflow.yml`。
//!
//! 端口必须在 YAML 里静态声明（topic 列表由声明的 outputs 推导），
//! 所以清单一变就必须重新生成并重启数据流。

use crate::catalog::Catalog;
use crate::routing::TYPE_PORTS;

/// 桥接与显示节点的固定 id。
const BRIDGE_ID: &str = "occsim-bridge";
const DISPLAY_ID: &str = "occsim-display";

/// 控制链路的端口名与类型 URN；端口名不带 `__`，不会被当成观测端口。
const CMD_PORT: &str = "cmd";
const TOKEN_PORT: &str = "cmd_token";
const ACK_PORT: &str = "cmd_ack";
const CONTROL_CMD_URN: &str = "usv/occsim/control/v1/ControlCmd";
const CONTROL_TOKEN_URN: &str = "usv/occsim/control/v1/ControlToken";
const CONTROL_ACK_URN: &str = "usv/occsim/control/v1/ControlAck";

/// 每个 kind 对应的输出类型 URN。
///
/// 自定义类型按传感器分目录：`types/usv/occsim/<kind>/v1.yml`，URN 即 `usv/occsim/<kind>/v1/<Type>`；
/// `image` 直接复用 dora 内置的 `std/media/v1/Image`（字段与该端口发送的完全一致），
/// 这样 dora-yolo / dora-rerun 等现成节点能直连，且 `DORA_RUNTIME_TYPE_CHECK`
/// 只在 std 类型上生效（节点侧注册表只有 std）。
/// dora 在 `dora validate` / `dora build` 时按数据流所在目录的 `types/` 解析这些 URN；
/// 类型文件与 `src/encode.rs` 的一致性由 `encode` 里的契约测试保证，改字段时两处都要动。
pub fn type_urn(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "image" => "std/media/v1/Image",
        "pointcloud" => "usv/occsim/pointcloud/v1/PointCloud",
        "gps" => "usv/occsim/gps/v1/GpsFix",
        "imu" => "usv/occsim/imu/v1/ImuSample",
        "ins" => "usv/occsim/ins/v1/InsState",
        "odom" => "usv/occsim/odom/v1/OdomState",
        "state" => "usv/occsim/state/v1/SimState",
        "health" => "usv/occsim/health/v1/Health",
        _ => return None,
    })
}

/// 端口名 → 输出类型 URN；实例端口形如 `{sensor}__{kind}`。
fn port_type(port: &str) -> Option<&'static str> {
    port.rsplit("__").next().and_then(type_urn)
}

/// 显示节点的可选配置。`None` 时生成的 YAML 与未引入显示节点时完全一致。
pub struct DisplayOptions<'a> {
    /// Python 解释器路径，写进节点的 `path`。
    pub python: &'a str,
    /// 显示程序入口（`main.py`）路径，写进节点的 `args`。
    pub script: &'a str,
}

/// Windows 路径统一转正斜杠，避免 YAML 双引号里出现 `\` 转义问题。
fn slash(path: &str) -> String {
    path.replace('\\', "/")
}

/// 渲染数据流定义。`exe` 是可执行文件相对/绝对路径，`build` 是构建命令；
/// `display` 为 `Some` 时在同一份数据流里追加显示节点，输入逐条映射到桥接输出。
/// `control` 为真时在两者之间接上 `cmd` / `cmd_token` / `cmd_ack` 三个固定端口。
pub fn render(
    catalog: &Catalog,
    exe: &str,
    build: &str,
    display: Option<&DisplayOptions<'_>>,
    control: bool,
) -> String {
    // 控制的上游就是显示节点：没有它就没有人发 `cmd`，接线会变成悬空输入。
    let control = control && display.is_some();
    let mut out = String::new();
    out.push_str("# 本文件由 `occsim-dora-rs gen-dataflow` 生成，请勿手改。\n");
    out.push_str("# 实例端口来自 ports.yaml；新增传感器后重跑生成器并重启数据流。\n");
    // 打开调试检查，才能用 `dora topic echo/hz` 观察端口
    out.push_str("debug:\n  enable_debug_inspection: true\n");
    out.push_str(&format!("nodes:\n  - id: {BRIDGE_ID}\n"));
    out.push_str(&format!("    path: \"{exe}\"\n"));
    out.push_str(&format!("    build: \"{build}\"\n"));
    out.push_str("    inputs:\n      tick: dora/timer/millis/50\n");
    if control {
        // 令牌走 `cmd_token` 端口、只存桥接内存：不写进 YAML、不落盘、不进日志。
        out.push_str(&format!("      {CMD_PORT}: {DISPLAY_ID}/{CMD_PORT}\n"));
        out.push_str(&format!("      {TOKEN_PORT}: {DISPLAY_ID}/{TOKEN_PORT}\n"));
        out.push_str("    input_types:\n");
        out.push_str(&format!("      {CMD_PORT}: {CONTROL_CMD_URN}\n"));
        out.push_str(&format!("      {TOKEN_PORT}: {CONTROL_TOKEN_URN}\n"));
    }
    // 清单非空时只声明实例端口：不再生成 8 路类型兜底端口，
    // 未声明的传感器会被节点丢弃并告警，而不是静默落到别的端口上。
    let instance_ports = catalog.instance_ports();
    let ports: Vec<String> = if instance_ports.is_empty() {
        TYPE_PORTS.iter().map(|port| port.to_string()).collect()
    } else {
        instance_ports
    };
    out.push_str("    outputs:\n");
    for port in &ports {
        out.push_str(&format!("      - {port}\n"));
    }
    if control {
        out.push_str(&format!("      - {ACK_PORT}\n"));
    }
    out.push_str("    output_types:\n");
    for port in &ports {
        let urn = port_type(port).expect("端口 kind 必须在八类之内");
        out.push_str(&format!("      {port}: {urn}\n"));
    }
    if control {
        out.push_str(&format!("      {ACK_PORT}: {CONTROL_ACK_URN}\n"));
    }
    if let Some(display) = display {
        // 显示节点没有 build：Python 脚本不需要构建；输入名沿用端口名，
        // 显示端按 `__` 后缀识别 kind，传感器名从 metadata 里读。
        out.push_str(&format!("  - id: {DISPLAY_ID}\n"));
        out.push_str(&format!("    path: \"{}\"\n", slash(display.python)));
        // dora 1.1 的 args 是单个字符串（按命令行拆词），不是 YAML 序列。
        out.push_str(&format!("    args: \"-u {}\"\n", slash(display.script)));
        out.push_str("    inputs:\n");
        for port in &ports {
            out.push_str(&format!("      {port}: {BRIDGE_ID}/{port}\n"));
        }
        if control {
            // 操控页发出的指令/令牌 + 桥接回传的回执，构成控制闭环。
            out.push_str(&format!("      {ACK_PORT}: {BRIDGE_ID}/{ACK_PORT}\n"));
            out.push_str("    outputs:\n");
            out.push_str(&format!("      - {CMD_PORT}\n"));
            out.push_str(&format!("      - {TOKEN_PORT}\n"));
            out.push_str("    output_types:\n");
            out.push_str(&format!("      {CMD_PORT}: {CONTROL_CMD_URN}\n"));
            out.push_str(&format!("      {TOKEN_PORT}: {CONTROL_TOKEN_URN}\n"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_type_ports_even_with_empty_catalog() {
        let yaml = render(
            &Catalog::empty(),
            "target/release/occsim-dora-rs.exe",
            "cargo build --release",
            None,
            false,
        );
        assert!(yaml.contains("      - image\n"));
        assert!(yaml.contains("      - health\n"));
        assert!(yaml.contains("      image: std/media/v1/Image\n"));
        assert!(yaml.contains("      gps: usv/occsim/gps/v1/GpsFix\n"));
        assert!(yaml.contains("      health: usv/occsim/health/v1/Health\n"));
        assert!(yaml.contains("tick: dora/timer/millis/50"));
        assert!(!yaml.contains("__"));
    }

    #[test]
    fn instance_ports_reuse_their_kind_type() {
        assert_eq!(port_type("gps-mast__gps"), Some("usv/occsim/gps/v1/GpsFix"));
        assert_eq!(port_type("cam-bow__image"), Some("std/media/v1/Image"));
        assert_eq!(port_type("bogus__thing"), None);
    }

    #[test]
    fn declared_instances_replace_type_ports() {
        let catalog = Catalog::for_test(&[("gps-mast", "gps"), ("cam-bow", "image")]);
        let yaml = render(
            &catalog,
            "target/release/occsim-dora-rs.exe",
            "cargo build --release",
            None,
            false,
        );
        assert!(yaml.contains("      - gps-mast__gps\n"));
        assert!(yaml.contains("      - cam-bow__image\n"));
        assert!(yaml.contains("      gps-mast__gps: usv/occsim/gps/v1/GpsFix\n"));
        assert!(yaml.contains("      cam-bow__image: std/media/v1/Image\n"));
        // 清单非空后不再生成 8 路类型兜底端口。
        assert!(!yaml.contains("      - image\n"));
        assert!(!yaml.contains("      - health\n"));
        assert!(!yaml.contains("      image: "));
    }

    fn display() -> DisplayOptions<'static> {
        DisplayOptions {
            python: "D:/app/anaconda3/envs/occsim/python.exe",
            script: "display-py/main.py",
        }
    }

    #[test]
    fn display_node_maps_every_output() {
        let catalog = Catalog::for_test(&[("gps-mast", "gps"), ("rgb-bow", "image")]);
        let yaml = render(&catalog, "exe", "build", Some(&display()), false);
        assert!(yaml.contains("  - id: occsim-display\n"));
        assert!(yaml.contains("    path: \"D:/app/anaconda3/envs/occsim/python.exe\"\n"));
        assert!(yaml.contains("    args: \"-u display-py/main.py\"\n"));
        assert!(yaml.contains("      gps-mast__gps: occsim-bridge/gps-mast__gps\n"));
        assert!(yaml.contains("      rgb-bow__image: occsim-bridge/rgb-bow__image\n"));
        // 显示节点不带 build，避免 dora 对 Python 节点执行 cargo 构建。
        let display_section = yaml.split("  - id: occsim-display").nth(1).unwrap();
        assert!(!display_section.contains("build:"));
    }

    #[test]
    fn display_is_absent_by_default() {
        let yaml = render(&Catalog::empty(), "exe", "build", None, false);
        assert!(!yaml.contains("occsim-display"));
    }

    #[test]
    fn display_paths_use_forward_slashes() {
        let options = DisplayOptions {
            python: r"C:\tools\python.exe",
            script: r"display-py\main.py",
        };
        let yaml = render(&Catalog::empty(), "exe", "build", Some(&options), false);
        assert!(yaml.contains("    path: \"C:/tools/python.exe\"\n"));
        assert!(yaml.contains("    args: \"-u display-py/main.py\"\n"));
    }

    /// 控制接线：桥接吃 `cmd`/`cmd_token`、吐 `cmd_ack`，显示端反向发 `cmd`/`cmd_token`。
    #[test]
    fn control_wires_command_token_and_ack() {
        let catalog = Catalog::for_test(&[("gps-mast", "gps")]);
        let yaml = render(&catalog, "exe", "build", Some(&display()), true);
        assert!(yaml.contains("      cmd: occsim-display/cmd\n"));
        assert!(yaml.contains("      cmd_token: occsim-display/cmd_token\n"));
        assert!(yaml.contains("    input_types:\n"));
        assert!(yaml.contains("      cmd: usv/occsim/control/v1/ControlCmd\n"));
        assert!(yaml.contains("      cmd_token: usv/occsim/control/v1/ControlToken\n"));
        assert!(yaml.contains("      - cmd_ack\n"));
        assert!(yaml.contains("      cmd_ack: usv/occsim/control/v1/ControlAck\n"));
        assert!(yaml.contains("      cmd_ack: occsim-bridge/cmd_ack\n"));
        assert!(yaml.contains("      - cmd\n      - cmd_token\n"));
        assert!(yaml.contains("      cmd_token: usv/occsim/control/v1/ControlToken\n"));
    }

    /// 令牌由操控页下发、只存桥接内存：不能写进 YAML。
    #[test]
    fn token_never_lands_in_yaml() {
        let yaml = render(&Catalog::empty(), "exe", "build", Some(&display()), true);
        assert!(!yaml.contains("env:"));
        assert!(!yaml.contains("pairToken"));
        assert!(!yaml.contains("secret"));
    }

    /// 没有显示节点就没人发 `cmd`：控制接线整体忽略，不能留悬空输入。
    #[test]
    fn control_without_display_is_ignored() {
        let yaml = render(&Catalog::empty(), "exe", "build", None, true);
        assert!(!yaml.contains("cmd"));
    }
}
