//! 与 Python 侧 `integration/common.py` 的对拍测试。
//!
//! 本 crate 是二进制目标、没有 lib，这里用 `#[path]` 把 `src/decode.rs` 直接编进测试，
//! 保证被测的就是线上运行的那一份源文件。
//!
//! Python 侧调用的是 `parse_observation`，因此主题解析（`split_key`）与负载解码一起对拍：
//! 同一份 payload 两边必须得出同样的结论——要么都成功且 kind/sensor/instance/runId、
//! 通道数、点数、有效点数一致，要么都判无效，要么都判定不是观测主题。
//!
//! 机器上没有 python 或缺少 `integration/common.py` / 真实样本时整测跳过（打印提示），
//! 不会让 `cargo test` 因环境缺失而变红。

#[path = "../src/decode.rs"]
mod decode;

use std::path::{Path, PathBuf};
use std::process::Command;

use decode::{Body, DecodeError, decode};
use serde_json::{Value, json};

/// `occsim-dora-rs` 目录。
const ROOT: &str = env!("CARGO_MANIFEST_DIR");
/// 真实点云样本：只含点数据（115200 点 × 16 字节），头部在测试里按尺寸补出来。
const SAMPLE: &str = "occsim-output/82bace6a64afe4f5.bin";

struct Case {
    name: &'static str,
    key: &'static str,
    payload: Vec<u8>,
}

fn with_header(meta: &Value, body: &[u8]) -> Vec<u8> {
    let text = meta.to_string();
    let mut out = (text.len() as u32).to_le_bytes().to_vec();
    out.extend_from_slice(text.as_bytes());
    out.extend_from_slice(body);
    out
}

/// Rust 侧结论，字段与 Python 侧输出一一对应，方便整体比较。
fn rust_outcome(case: &Case) -> Value {
    let record = match decode(case.key, &case.payload) {
        Ok(record) => record,
        Err(DecodeError::NotObservation) => return json!({"state": "not_observation"}),
        Err(_) => return json!({"state": "invalid"}),
    };
    let mut value = json!({
        "state": "ok",
        "key": record.key,
        "kind": record.parts.kind,
        "sensor": record.parts.sensor,
        "instance": record.parts.instance,
        "runId": record.parts.run_id,
        "size": record.size,
        "channels": Value::Null,
        "points": Value::Null,
        "finite": Value::Null,
        "values": Value::Null,
    });
    match &record.body {
        Body::Image { channels, .. } => value["channels"] = json!(channels),
        Body::PointCloud { points, .. } => {
            let count = points.len() / 4;
            let finite = points
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|point| point[..3].iter().all(|value| value.is_finite()))
                .count();
            value["points"] = json!(count);
            value["finite"] = json!(finite);
            if count <= 4 {
                let bits: Vec<u32> = points.iter().map(|value| value.to_bits()).collect();
                value["values"] = json!(bits);
            }
        }
        Body::Json { .. } => {}
    }
    value
}

fn python() -> Option<&'static str> {
    ["python", "python3"].into_iter().find(|name| {
        Command::new(name)
            .arg("-c")
            .arg("import struct, json")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    })
}

fn build_cases(sample: &Path) -> Vec<Case> {
    let mut cases = vec![
        Case {
            name: "image-mono8",
            key: "usv/v1/a/b/c/d/sensors/cam/image",
            payload: with_header(
                &json!({"encoding": "mono8", "width": 2, "height": 2}),
                &[1, 2, 3, 4],
            ),
        },
        Case {
            name: "image-length-mismatch",
            key: "usv/v1/a/b/c/d/sensors/cam/image",
            payload: with_header(
                &json!({"encoding": "mono8", "width": 2, "height": 2}),
                &[1, 2, 3],
            ),
        },
        Case {
            name: "image-unknown-encoding",
            key: "usv/v1/a/b/c/d/sensors/cam/image",
            payload: with_header(
                &json!({"encoding": "rgb8", "width": 2, "height": 2}),
                &[0; 12],
            ),
        },
        Case {
            name: "image-header-truncated",
            key: "usv/v1/a/b/c/d/sensors/cam/image",
            payload: vec![16, 0, 0],
        },
        Case {
            name: "pointcloud-synthetic",
            key: "usv/v1/a/b/c/d/sensors/lidar/pointcloud",
            payload: with_header(
                &json!({
                    "encoding": "xyzi32f",
                    "pointStride": 16,
                    "width": 2,
                    "height": 1,
                    "pointCount": 2
                }),
                &[1.0f32, -2.5, 3.25, 0.5, 0.0, 0.0, 0.0, 1.0]
                    .iter()
                    .flat_map(|value| value.to_le_bytes())
                    .collect::<Vec<u8>>(),
            ),
        },
        Case {
            name: "pointcloud-count-mismatch",
            key: "usv/v1/a/b/c/d/sensors/lidar/pointcloud",
            payload: with_header(
                &json!({
                    "encoding": "xyzi32f",
                    "pointStride": 16,
                    "width": 2,
                    "height": 1,
                    "pointCount": 3
                }),
                &[0u8; 32],
            ),
        },
        Case {
            name: "gps-json",
            key: "usv/v1/a/b/c/d/sensors/gps-mast/gps",
            payload: br#"{"fix":"3D","coordinates":[31.0,121.0,5.0],"velocity":[0.1,0.2,0.3]}"#
                .to_vec(),
        },
        Case {
            name: "state-json",
            key: "usv/v1/a/b/c/d/state",
            payload: br#"{"simTime":1.5,"paused":false,"controlMode":"manual"}"#.to_vec(),
        },
        Case {
            name: "health-json",
            key: "usv/v1/a/b/c/d/health",
            payload: br#"{"sent":12,"dropped":0,"simTime":1.5}"#.to_vec(),
        },
        Case {
            name: "json-not-object",
            key: "usv/v1/a/b/c/d/state",
            payload: b"[1,2]".to_vec(),
        },
        Case {
            name: "not-observation",
            key: "usv/v1/a/b/c/d/active",
            payload: br#"{"prefix":"usv/v1/a/b/c/d"}"#.to_vec(),
        },
        Case {
            name: "wrong-prefix",
            key: "usv/v2/a/b/c/d/state",
            payload: br#"{"simTime":1.0}"#.to_vec(),
        },
    ];
    // 超过 1 MiB 的 JSON 观测：两边都应在解析前按大小拒掉。
    cases.push(Case {
        name: "json-too-large",
        key: "usv/v1/a/b/c/d/state",
        payload: vec![b'a'; 1_048_577],
    });

    // 真实点云帧：头部按样本尺寸补出来（7200 列 × N 行，每点 16 字节）。
    if let Ok(points) = std::fs::read(sample) {
        let count = points.len() / 16;
        let width = 7200;
        let height = count / width;
        if count > 0 && height > 0 && count == width * height {
            cases.push(Case {
                name: "pointcloud-real-sample",
                key: "usv/v1/a/b/c/d/sensors/lidar/pointcloud",
                payload: with_header(
                    &json!({
                        "encoding": "xyzi32f",
                        "pointStride": 16,
                        "width": width,
                        "height": height,
                        "pointCount": count
                    }),
                    &points,
                ),
            });
        } else {
            eprintln!("跳过真实点云样本：尺寸 {} 无法按 7200 列整除", points.len());
        }
    } else {
        eprintln!("跳过真实点云样本：找不到 {}", sample.display());
    }
    cases
}

#[test]
fn decode_matches_python_common_py() {
    let Some(python) = python() else {
        eprintln!("跳过对拍：PATH 上没有可用的 python（需要 3.10+）");
        return;
    };
    let integration = Path::new(ROOT).join("..").join("integration");
    if !integration.join("common.py").is_file() {
        eprintln!("跳过对拍：找不到 {}", integration.join("common.py").display());
        return;
    }

    let cases = build_cases(&Path::new(ROOT).join("..").join(SAMPLE));
    let workdir = std::env::temp_dir().join(format!("occsim-decode-parity-{}", std::process::id()));
    std::fs::create_dir_all(&workdir).expect("创建临时目录失败");

    let mut spec = Vec::new();
    for (index, case) in cases.iter().enumerate() {
        let file = workdir.join(format!("{index}.bin"));
        std::fs::write(&file, &case.payload).expect("写入夹具失败");
        spec.push(json!({"key": case.key, "file": file.to_string_lossy()}));
    }
    let spec_path = workdir.join("cases.json");
    let result_path = workdir.join("result.json");
    std::fs::write(&spec_path, json!(spec).to_string()).expect("写入夹具清单失败");

    let script = r#"
import json, math, struct, sys
sys.path.insert(0, sys.argv[1])
import common

cases = json.load(open(sys.argv[2], encoding='utf-8'))
out = []
for case in cases:
    payload = open(case['file'], 'rb').read()
    try:
        record = common.parse_observation(case['key'], payload)
    except Exception:
        out.append({'state': 'invalid'})
        continue
    if record is None:
        out.append({'state': 'not_observation'})
        continue
    item = {
        'state': 'ok',
        'key': record['key'],
        'kind': record['kind'],
        'sensor': record['sensor'],
        'instance': record['instance'],
        'runId': record['runId'],
        'size': record['size'],
        'channels': record['channels'],
        'points': None,
        'finite': None,
        'values': None,
    }
    if record['kind'] == 'pointcloud':
        points = list(struct.iter_unpack('<ffff', record['payload']))
        item['points'] = len(points)
        item['finite'] = sum(1 for p in points if all(math.isfinite(v) for v in p[:3]))
        if len(points) <= 4:
            item['values'] = [struct.unpack('<I', struct.pack('<f', v))[0] for p in points for v in p]
    out.append(item)
json.dump(out, open(sys.argv[3], 'w', encoding='utf-8'))
"#;

    let output = Command::new(python)
        .arg("-c")
        .arg(script)
        .arg(&integration)
        .arg(&spec_path)
        .arg(&result_path)
        .output()
        .expect("启动 python 失败");
    assert!(
        output.status.success(),
        "python 对拍脚本失败：{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let expected: Vec<Value> =
        serde_json::from_slice(&std::fs::read(&result_path).expect("读取 python 结果失败"))
            .expect("python 输出不是合法 JSON");
    assert_eq!(expected.len(), cases.len(), "案例数量不一致");

    for (case, expected) in cases.iter().zip(expected) {
        let actual = rust_outcome(case);
        assert_eq!(
            actual, expected,
            "{} 与 Python 结论不一致\n  rust:   {actual}\n  python: {expected}",
            case.name
        );
    }

    let _ = std::fs::remove_dir_all(&workdir);
}

/// 真实样本自带的上限与头部规则，单独钉一下，避免只依赖对拍。
#[test]
fn real_pointcloud_sample_decodes_to_declared_size() {
    let sample = PathBuf::from(ROOT).join("..").join(SAMPLE);
    let Ok(points) = std::fs::read(&sample) else {
        eprintln!("跳过：找不到 {}", sample.display());
        return;
    };
    let count = points.len() / 16;
    let (width, height) = (7200, count / 7200);
    assert!(height > 0 && count == width * height, "样本尺寸不符合 7200 列布局");

    let payload = with_header(
        &json!({
            "encoding": "xyzi32f",
            "pointStride": 16,
            "width": width,
            "height": height,
            "pointCount": count
        }),
        &points,
    );
    let record = decode("usv/v1/a/b/c/d/sensors/lidar/pointcloud", &payload).unwrap();
    match record.body {
        Body::PointCloud {
            point_count, points, ..
        } => {
            assert_eq!(point_count as usize, count);
            assert_eq!(points.len(), count * 4);
            // 首点通常是无效回波（NaN），强度为 0。
            assert!(points[0].is_nan());
            assert_eq!(points[3], 0.0);
        }
        other => panic!("应当是点云，实际 {other:?}"),
    }
}
