//! 从实例清单生成 `dataflow.yml`。
//!
//! 端口必须在 YAML 里静态声明（topic 列表由声明的 outputs 推导），
//! 所以清单一变就必须重新生成并重启数据流。

use crate::catalog::Catalog;
use crate::routing::TYPE_PORTS;

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

/// 渲染数据流定义。`exe` 是可执行文件相对/绝对路径，`build` 是构建命令。
pub fn render(catalog: &Catalog, exe: &str, build: &str) -> String {
    let mut out = String::new();
    out.push_str("# 本文件由 `occsim-dora-rs gen-dataflow` 生成，请勿手改。\n");
    out.push_str("# 实例端口来自 ports.yaml；新增传感器后重跑生成器并重启数据流。\n");
    // 打开调试检查，才能用 `dora topic echo/hz` 观察端口
    out.push_str("debug:\n  enable_debug_inspection: true\n");
    out.push_str("nodes:\n  - id: occsim-bridge\n");
    out.push_str(&format!("    path: \"{exe}\"\n"));
    out.push_str(&format!("    build: \"{build}\"\n"));
    out.push_str("    inputs:\n      tick: dora/timer/millis/50\n");
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
    out.push_str("    output_types:\n");
    for port in &ports {
        let urn = port_type(port).expect("端口 kind 必须在八类之内");
        out.push_str(&format!("      {port}: {urn}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_type_ports_even_with_empty_catalog() {
        let yaml = render(&Catalog::empty(), "target/release/occsim-dora-rs.exe", "cargo build --release");
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
        let yaml = render(&catalog, "target/release/occsim-dora-rs.exe", "cargo build --release");
        assert!(yaml.contains("      - gps-mast__gps\n"));
        assert!(yaml.contains("      - cam-bow__image\n"));
        assert!(yaml.contains("      gps-mast__gps: usv/occsim/gps/v1/GpsFix\n"));
        assert!(yaml.contains("      cam-bow__image: std/media/v1/Image\n"));
        // 清单非空后不再生成 8 路类型兜底端口。
        assert!(!yaml.contains("      - image\n"));
        assert!(!yaml.contains("      - health\n"));
        assert!(!yaml.contains("      image: "));
    }
}
