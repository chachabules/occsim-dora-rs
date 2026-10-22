//! 传感器实例清单（`ports.yaml`）的加载与展开。
//!
//! 由于 dora 的端口必须在 dataflow.yml 里静态声明，实例端口只能来自这份清单；
//! 清单与 `gen-dataflow` 生成的 YAML 必须保持一致，否则数据会「发出去没人订阅」。

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::routing::{TYPE_PORTS, normalize};

#[derive(Debug, Default, Deserialize)]
struct PortsFile {
    #[serde(default)]
    instances: Vec<PortEntry>,
}

#[derive(Debug, Deserialize)]
struct PortEntry {
    /// 传感器 id，例如 `gps-mast`、`lidar-bow`。
    sensor: String,
    /// 观测类型，必须是 `KINDS` 之一。
    kind: String,
}

#[derive(Debug, Default)]
pub struct Catalog {
    /// (normalized sensor, kind)
    entries: Vec<(String, String)>,
}

impl Catalog {
    pub fn empty() -> Self {
        Self::default()
    }

    /// 读取清单；文件不存在时返回空清单（只用类型兜底端口）。
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::empty());
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("读取清单失败: {}", path.display()))?;
        let file: PortsFile = serde_yaml::from_str(&text)
            .with_context(|| format!("解析清单失败: {}", path.display()))?;

        let mut entries = Vec::new();
        let mut seen = BTreeSet::new();
        for entry in file.instances {
            let kind = entry.kind.trim().to_ascii_lowercase();
            if !TYPE_PORTS.contains(&kind.as_str()) {
                tracing::warn!("清单里的 kind 不在八类之内，已忽略: {}", entry.kind);
                continue;
            }
            let sensor = normalize(&entry.sensor);
            if sensor.is_empty() {
                tracing::warn!("清单里的 sensor 为空或非法，已忽略（kind={kind}）");
                continue;
            }
            if !seen.insert((sensor.clone(), kind.clone())) {
                tracing::warn!("清单里有重复项，已去重: {sensor} / {kind}");
                continue;
            }
            entries.push((sensor, kind));
        }
        Ok(Self { entries })
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 清单里是否声明了该实例端口。
    pub fn declares(&self, kind: &str, sensor: &str) -> bool {
        self.entries
            .iter()
            .any(|(s, k)| s == sensor && k == kind)
    }

    /// 展开后的实例端口名，供生成 dataflow.yml 使用。
    pub fn instance_ports(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|(sensor, kind)| format!("{sensor}__{kind}"))
            .collect()
    }

    /// 测试用：直接构造清单，跳过 YAML 解析。
    #[cfg(test)]
    pub fn for_test(pairs: &[(&str, &str)]) -> Self {
        Self {
            entries: pairs
                .iter()
                .map(|(sensor, kind)| (sensor.to_string(), kind.to_string()))
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_deduplicates() {
        let dir = std::env::temp_dir().join("occsim-catalog-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ports.yaml");
        std::fs::write(
            &path,
            "instances:\n  - sensor: GPS-Mast\n    kind: gps\n  - sensor: gps-mast\n    kind: gps\n  - sensor: x\n    kind: bogus\n",
        )
        .unwrap();
        let catalog = Catalog::load(&path).unwrap();
        assert_eq!(catalog.instance_ports(), vec!["gps-mast__gps".to_string()]);
        assert!(catalog.declares("gps", "gps-mast"));
        assert!(!catalog.declares("imu", "gps-mast"));
    }
}
