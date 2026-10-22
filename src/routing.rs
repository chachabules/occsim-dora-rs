//! 端口路由：清单为空时走 8 路类型端口，清单非空时只认已声明的实例端口。

use crate::catalog::Catalog;

/// 固定 8 路类型兜底端口，必须与 dataflow.yml 的 outputs 完全一致，
/// 否则 Rust 侧 `send_output` 会被静默忽略（dora 对未声明端口返回 Ok 但不发送）。
pub const TYPE_PORTS: [&str; 8] = [
    "image",
    "pointcloud",
    "gps",
    "imu",
    "ins",
    "odom",
    "state",
    "health",
];

/// 规范化传感器 id：小写、非字母数字转 '-'、合并连续分隔符并去掉首尾。
pub fn normalize(sensor: &str) -> String {
    let mut out = String::with_capacity(sensor.len());
    let mut last_dash = false;
    for ch in sensor.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// 选择端口。
///
/// - 清单为空：`dataflow.yml` 里只有 8 路类型端口，全部发到 `kind` 同名端口；
/// - 清单非空：`dataflow.yml` 里只声明实例端口，因此只有 `{sensor}__{kind}` 可用，
///   未声明的传感器返回 `None`，由调用方丢弃并告警（不许发到未声明端口）。
pub fn route(kind: &str, sensor: &str, catalog: &Catalog) -> Option<String> {
    if catalog.is_empty() {
        return Some(kind.to_string());
    }
    let id = normalize(sensor);
    if !id.is_empty() && catalog.declares(kind, &id) {
        Some(format!("{id}__{kind}"))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_handles_separators_and_case() {
        assert_eq!(normalize("GPS-Mast"), "gps-mast");
        assert_eq!(normalize("lidar_bow / 01"), "lidar-bow-01");
        assert_eq!(normalize("--x--"), "x");
        assert_eq!(normalize(""), "");
    }

    #[test]
    fn route_falls_back_to_type_port_only_when_catalog_is_empty() {
        let catalog = Catalog::empty();
        assert_eq!(route("gps", "gps-mast", &catalog).as_deref(), Some("gps"));
    }

    #[test]
    fn route_uses_declared_instance_port_and_rejects_the_rest() {
        let catalog = Catalog::for_test(&[("gps-mast", "gps")]);
        assert_eq!(
            route("gps", "GPS-Mast", &catalog).as_deref(),
            Some("gps-mast__gps")
        );
        // 清单非空后，未声明的实例不再回落到类型端口。
        assert_eq!(route("imu", "imu-main", &catalog), None);
        assert_eq!(route("gps", "gps-bow", &catalog), None);
    }
}
