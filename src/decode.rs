//! OccSim 主题与负载解码。
//!
//! 规格逐条对齐 `integration/common.py` 的 `split_key` / `decode_image` /
//! `decode_pointcloud` / `parse_observation`，包括各级上限与白名单，确保
//! Rust 侧与 Python 侧对同一帧给出完全一致的结论（见 `tests/decode_parity.rs`）。
//!
//! 错误一律返回 `DecodeError`，由调用方计数丢弃，绝不 panic。

use std::fmt;

use serde_json::Value;

/// 八类观测主题（供外部工具/文档引用，运行期主要用下面的 `OBSERVATION`）。
#[allow(dead_code)]
pub const KINDS: [&str; 8] = [
    "image",
    "pointcloud",
    "gps",
    "imu",
    "ins",
    "odom",
    "state",
    "health",
];

/// 走 `sensors/{id}/{kind}` 九段主题的六类（state/health 走七段）。
const OBSERVATION: [&str; 6] = ["image", "pointcloud", "gps", "imu", "ins", "odom"];

/// JSON 类观测的负载上限，与 Python 的 `1048576` 对齐。
const JSON_LIMIT: usize = 1_048_576;
/// 图像/点云头部长度上限。
const HEADER_LIMIT: u32 = 65_536;
/// 图像像素总数上限。
const IMAGE_PIXEL_LIMIT: u64 = 16_777_216;
/// 点云组织规格上限。
const CLOUD_WIDTH_LIMIT: u64 = 7_200;
const CLOUD_HEIGHT_LIMIT: u64 = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyParts {
    pub kind: String,
    pub sensor: String,
    pub instance: String,
    pub run_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Body {
    Image {
        width: u32,
        height: u32,
        channels: u8,
        encoding: String,
        pixels: Vec<u8>,
    },
    PointCloud {
        width: u32,
        height: u32,
        point_count: u32,
        /// 扁平 x/y/z/intensity，长度 = point_count * 4。
        points: Vec<f32>,
    },
    Json {
        value: Value,
        /// 原始 UTF-8 字节，便于下游自行解析。
        raw: Vec<u8>,
    },
}

#[derive(Debug, Clone)]
pub struct Record {
    pub key: String,
    pub parts: KeyParts,
    /// 完整负载字节数（含头部），与 Python 的 `size` 一致。
    pub size: usize,
    pub body: Body,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// 主题不属于八类观测（例如 `usv/v1/.../active`），应当静默跳过。
    NotObservation,
    HeaderMissing,
    HeaderTooLong,
    JsonInvalid(String),
    UnsupportedEncoding,
    DimensionsInvalid,
    PayloadLengthMismatch { expected: usize, actual: usize },
    TooLarge,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotObservation => write!(f, "主题不是观测"),
            Self::HeaderMissing => write!(f, "缺少 4 字节头部长度"),
            Self::HeaderTooLong => write!(f, "头部长度越界"),
            Self::JsonInvalid(detail) => write!(f, "JSON 非法: {detail}"),
            Self::UnsupportedEncoding => write!(f, "编码或维度不受支持"),
            Self::DimensionsInvalid => write!(f, "维度不一致"),
            Self::PayloadLengthMismatch { expected, actual } => {
                write!(f, "负载长度不匹配: 期望 {expected}，实际 {actual}")
            }
            Self::TooLarge => write!(f, "负载超过上限"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// 解析主题。返回 `None` 表示不是观测主题。
pub fn split_key(key: &str) -> Option<KeyParts> {
    let parts: Vec<&str> = key.split('/').collect();
    if parts.len() < 2 || parts[0] != "usv" || parts[1] != "v1" {
        return None;
    }
    let (kind, sensor) = match parts.len() {
        7 if parts[6] == "state" || parts[6] == "health" => (parts[6].to_string(), parts[6].to_string()),
        9 if parts[6] == "sensors" && OBSERVATION.contains(&parts[8]) => {
            (parts[8].to_string(), parts[7].to_string())
        }
        _ => return None,
    };
    Some(KeyParts {
        kind,
        sensor,
        instance: parts[2..5].join("/"),
        run_id: parts[5].to_string(),
    })
}

/// 解码一条观测。非观测主题返回 `NotObservation`，调用方据此区分「跳过」与「无效」。
pub fn decode(key: &str, payload: &[u8]) -> Result<Record, DecodeError> {
    let parts = split_key(key).ok_or(DecodeError::NotObservation)?;
    let body = match parts.kind.as_str() {
        "image" => decode_image(payload)?,
        "pointcloud" => decode_pointcloud(payload)?,
        _ => decode_json(payload)?,
    };
    Ok(Record {
        key: key.to_string(),
        parts,
        size: payload.len(),
        body,
    })
}

/// 读 4 字节小端头长度并切出 JSON 头与剩余负载。
fn split_header(payload: &[u8]) -> Result<(Value, &[u8]), DecodeError> {
    if payload.len() < 4 {
        return Err(DecodeError::HeaderMissing);
    }
    let length = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
    if length > HEADER_LIMIT || 4 + length as usize > payload.len() {
        return Err(DecodeError::HeaderTooLong);
    }
    let head = &payload[4..4 + length as usize];
    let meta: Value =
        serde_json::from_slice(head).map_err(|e| DecodeError::JsonInvalid(e.to_string()))?;
    if !meta.is_object() {
        return Err(DecodeError::JsonInvalid("头部必须是 JSON 对象".into()));
    }
    Ok((meta, &payload[4 + length as usize..]))
}

fn decode_image(payload: &[u8]) -> Result<Body, DecodeError> {
    let (meta, pixels) = split_header(payload)?;
    let encoding = meta
        .get("encoding")
        .and_then(Value::as_str)
        .ok_or(DecodeError::UnsupportedEncoding)?;
    let channels = match encoding {
        "rgba8" => 4u8,
        "mono8" => 1u8,
        _ => return Err(DecodeError::UnsupportedEncoding),
    };
    let width = meta.get("width").and_then(Value::as_u64).unwrap_or(0);
    let height = meta.get("height").and_then(Value::as_u64).unwrap_or(0);
    if width < 1 || height < 1 || width * height > IMAGE_PIXEL_LIMIT {
        return Err(DecodeError::DimensionsInvalid);
    }
    let expected = (width * height * channels as u64) as usize;
    if pixels.len() != expected {
        return Err(DecodeError::PayloadLengthMismatch {
            expected,
            actual: pixels.len(),
        });
    }
    Ok(Body::Image {
        width: width as u32,
        height: height as u32,
        channels,
        encoding: encoding.to_string(),
        pixels: pixels.to_vec(),
    })
}

fn decode_pointcloud(payload: &[u8]) -> Result<Body, DecodeError> {
    let (meta, points) = split_header(payload)?;
    if meta.get("encoding").and_then(Value::as_str) != Some("xyzi32f")
        || meta.get("pointStride").and_then(Value::as_u64) != Some(16)
    {
        return Err(DecodeError::UnsupportedEncoding);
    }
    let width = meta.get("width").and_then(Value::as_u64).unwrap_or(0);
    let height = meta.get("height").and_then(Value::as_u64).unwrap_or(0);
    let declared = meta.get("pointCount").and_then(Value::as_u64);
    if width < 1
        || width > CLOUD_WIDTH_LIMIT
        || height < 1
        || height > CLOUD_HEIGHT_LIMIT
        || declared != Some(width * height)
    {
        return Err(DecodeError::DimensionsInvalid);
    }
    let count = (width * height) as usize;
    if points.len() != count * 16 {
        return Err(DecodeError::PayloadLengthMismatch {
            expected: count * 16,
            actual: points.len(),
        });
    }
    let mut flat = Vec::with_capacity(count * 4);
    for chunk in points.chunks_exact(16) {
        for i in 0..4 {
            let offset = i * 4;
            flat.push(f32::from_le_bytes([
                chunk[offset],
                chunk[offset + 1],
                chunk[offset + 2],
                chunk[offset + 3],
            ]));
        }
    }
    Ok(Body::PointCloud {
        width: width as u32,
        height: height as u32,
        point_count: count as u32,
        points: flat,
    })
}

fn decode_json(payload: &[u8]) -> Result<Body, DecodeError> {
    if payload.len() > JSON_LIMIT {
        return Err(DecodeError::TooLarge);
    }
    let value: Value =
        serde_json::from_slice(payload).map_err(|e| DecodeError::JsonInvalid(e.to_string()))?;
    if !value.is_object() {
        return Err(DecodeError::JsonInvalid("负载必须是 JSON 对象".into()));
    }
    Ok(Body::Json {
        value,
        raw: payload.to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_header(meta: &str, body: &[u8]) -> Vec<u8> {
        let mut out = (meta.len() as u32).to_le_bytes().to_vec();
        out.extend_from_slice(meta.as_bytes());
        out.extend_from_slice(body);
        out
    }

    #[test]
    fn split_key_accepts_seven_and_nine_segments() {
        let state = split_key("usv/v1/inst/scene/boat/run/state").unwrap();
        assert_eq!(state.kind, "state");
        assert_eq!(state.sensor, "state");
        assert_eq!(state.instance, "inst/scene/boat");
        assert_eq!(state.run_id, "run");

        let gps = split_key("usv/v1/inst/scene/boat/run/sensors/gps-mast/gps").unwrap();
        assert_eq!(gps.kind, "gps");
        assert_eq!(gps.sensor, "gps-mast");

        assert!(split_key("usv/v1/inst/scene/boat/run").is_none());
        assert!(split_key("usv/v2/inst/scene/boat/run/state").is_none());
        assert!(split_key("usv/v1/a/b/c/d/sensors/x/notakind").is_none());
    }

    #[test]
    fn decode_image_roundtrip() {
        let meta = r#"{"encoding":"mono8","width":2,"height":2}"#;
        let payload = with_header(meta, &[1, 2, 3, 4]);
        let record = decode("usv/v1/a/b/c/d/sensors/cam/image", &payload).unwrap();
        assert_eq!(record.size, payload.len());
        match record.body {
            Body::Image {
                width,
                height,
                channels,
                pixels,
                ..
            } => {
                assert_eq!((width, height, channels), (2, 2, 1));
                assert_eq!(pixels, vec![1, 2, 3, 4]);
            }
            other => panic!("unexpected body: {other:?}"),
        }
    }

    #[test]
    fn decode_image_rejects_bad_length_and_encoding() {
        let meta = r#"{"encoding":"mono8","width":2,"height":2}"#;
        let payload = with_header(meta, &[1, 2, 3]);
        assert!(matches!(
            decode("usv/v1/a/b/c/d/sensors/cam/image", &payload),
            Err(DecodeError::PayloadLengthMismatch { .. })
        ));

        let meta = r#"{"encoding":"rgb8","width":2,"height":2}"#;
        let payload = with_header(meta, &[0; 12]);
        assert!(matches!(
            decode("usv/v1/a/b/c/d/sensors/cam/image", &payload),
            Err(DecodeError::UnsupportedEncoding)
        ));
    }

    #[test]
    fn decode_pointcloud_roundtrip() {
        let meta = r#"{"encoding":"xyzi32f","pointStride":16,"width":2,"height":1,"pointCount":2}"#;
        let mut body = Vec::new();
        for value in [1.0f32, 2.0, 3.0, 0.5, -1.0, -2.0, -3.0, 1.0] {
            body.extend_from_slice(&value.to_le_bytes());
        }
        let payload = with_header(meta, &body);
        let record = decode("usv/v1/a/b/c/d/sensors/lidar/pointcloud", &payload).unwrap();
        match record.body {
            Body::PointCloud {
                point_count,
                points,
                ..
            } => {
                assert_eq!(point_count, 2);
                assert_eq!(points.len(), 8);
                assert_eq!(points[4], -1.0);
            }
            other => panic!("unexpected body: {other:?}"),
        }
    }

    #[test]
    fn decode_json_and_not_observation() {
        let payload = br#"{"simTime":1.5,"paused":false}"#;
        let record = decode("usv/v1/a/b/c/d/state", payload).unwrap();
        assert!(matches!(record.body, Body::Json { .. }));

        assert!(matches!(
            decode("usv/v1/a/b/c/d/active", payload),
            Err(DecodeError::NotObservation)
        ));
        assert!(matches!(
            decode("usv/v1/a/b/c/d/state", b"[1,2]"),
            Err(DecodeError::JsonInvalid(_))
        ));
    }
}
