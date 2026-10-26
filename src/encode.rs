//! 解码结果 → 结构化 Arrow（单行 `StructArray`）。
//!
//! `image` 复用 dora 内置的 `std/media/v1/Image`；其余端口必须与
//! `types/usv/occsim/<kind>/v1.yml` 完全一致，否则 dora 的运行期类型检查会报
//! output type mismatch。所有字段都造为 nullable=true，与类型定义里的默认值对齐。

use std::sync::Arc;

use dora_node_api::arrow_v59::array::{
    ArrayRef, BooleanArray, Float64Array, LargeBinaryArray, StringArray, StructArray, UInt32Array,
    UInt64Array,
};
use dora_node_api::arrow_v59::datatypes::{DataType, Field, Fields};
use serde_json::Value;

use crate::decode::Body;

/// 按 kind 构造单行 StructArray；未知组合返回 `None`。
pub fn encode(kind: &str, body: &Body) -> Option<ArrayRef> {
    match body {
        Body::Image {
            width,
            height,
            encoding,
            pixels,
            ..
        } => Some(build(vec![
            field_u32("width", *width),
            field_u32("height", *height),
            field_utf8("encoding", encoding),
            field_binary("data", pixels),
        ])),
        Body::PointCloud {
            width,
            height,
            point_count,
            points,
            ..
        } => {
            let mut raw = Vec::with_capacity(points.len() * 4);
            for value in points {
                raw.extend_from_slice(&value.to_le_bytes());
            }
            Some(build(vec![
                field_u32("width", *width),
                field_u32("height", *height),
                field_utf8("encoding", "xyzi32f"),
                field_u64("pointCount", u64::from(*point_count)),
                field_binary("data", &raw),
            ]))
        }
        Body::Json { value, .. } => encode_json(kind, value),
    }
}

fn encode_json(kind: &str, value: &Value) -> Option<ArrayRef> {
    let columns = match kind {
        "gps" => vec![
            field_utf8("fix", &text(value, "fix")),
            field_f64("latitude", item(value, "coordinates", 0)),
            field_f64("longitude", item(value, "coordinates", 1)),
            field_f64("altitude", item(value, "coordinates", 2)),
            field_f64("vx", item(value, "velocity", 0)),
            field_f64("vy", item(value, "velocity", 1)),
            field_f64("vz", item(value, "velocity", 2)),
            field_utf8("quality", &text(value, "quality")),
            field_utf8("frameId", &text(value, "frameId")),
            field_u64("sequence", uint(value, "sequence")),
            field_f64("captureTime", number(value, "captureTime")),
        ],
        "imu" => vec![
            field_f64("fx", item(value, "specificForce", 0)),
            field_f64("fy", item(value, "specificForce", 1)),
            field_f64("fz", item(value, "specificForce", 2)),
            field_f64("wx", item(value, "angularVelocity", 0)),
            field_f64("wy", item(value, "angularVelocity", 1)),
            field_f64("wz", item(value, "angularVelocity", 2)),
            field_utf8("quality", &text(value, "quality")),
            field_utf8("source", &text(value, "source")),
            field_u64("sequence", uint(value, "sequence")),
            field_f64("captureTime", number(value, "captureTime")),
        ],
        "ins" => vec![
            field_f64("px", item(value, "position", 0)),
            field_f64("py", item(value, "position", 1)),
            field_f64("pz", item(value, "position", 2)),
            field_f64("vx", item(value, "velocity", 0)),
            field_f64("vy", item(value, "velocity", 1)),
            field_f64("vz", item(value, "velocity", 2)),
            field_f64("heading", number(value, "heading")),
            field_utf8("filter", &text(value, "filter")),
            field_f64("roll", number(value, "roll")),
            field_f64("pitch", number(value, "pitch")),
            field_utf8("quality", &text(value, "quality")),
            field_utf8("mode", &text(value, "mode")),
            field_u64("rejectedUpdates", uint(value, "rejectedUpdates")),
            field_f64("innovation", number(value, "innovation")),
            field_u64("sequence", uint(value, "sequence")),
            field_f64("captureTime", number(value, "captureTime")),
        ],
        "odom" => vec![
            field_f64("px", item(value, "position", 0)),
            field_f64("py", item(value, "position", 1)),
            field_f64("pz", item(value, "position", 2)),
            field_f64("vx", item(value, "twist", 0)),
            field_f64("vy", item(value, "twist", 1)),
            field_f64("vz", item(value, "twist", 2)),
            field_f64("heading", number(value, "heading")),
            field_f64("distance", number(value, "distance")),
            field_utf8("quality", &text(value, "quality")),
            field_utf8("source", &text(value, "source")),
            field_u64("sequence", uint(value, "sequence")),
            field_f64("captureTime", number(value, "captureTime")),
        ],
        // 网页把船体运动与推进放在 `state.state` 嵌套对象里，顶层是仿真元信息。
        "state" => {
            let hull = value.get("state");
            let hull_number = |key: &str| hull.map(|hull| number(hull, key)).unwrap_or(f64::NAN);
            // shafts[0] / shafts[1] 是左右两根轴的转速与推力明细
            let shaft = |index: usize, key: &str| {
                hull.and_then(|hull| hull.get("shafts"))
                    .and_then(|shafts| shafts.get(index))
                    .map(|shaft| number(shaft, key))
                    .unwrap_or(f64::NAN)
            };
            vec![
                field_f64("simTime", number(value, "simTime")),
                field_bool(
                    "paused",
                    value.get("paused").and_then(Value::as_bool).unwrap_or(false),
                ),
                field_utf8("controlMode", &text(value, "controlMode")),
                field_utf8("mission", &text(value, "mission")),
                field_f64("heading", hull_number("heading")),
                field_f64("roll", hull_number("roll")),
                field_f64("pitch", hull_number("pitch")),
                field_f64("n", hull_number("n")),
                field_f64("e", hull_number("e")),
                field_f64("d", hull_number("d")),
                field_f64("throttle", hull_number("throttle")),
                field_f64("steering", hull_number("steering")),
                field_f64("left", hull_number("left")),
                field_f64("right", hull_number("right")),
                field_f64("rpmPort", shaft(0, "rpm")),
                field_f64("rpmStarboard", shaft(1, "rpm")),
                field_f64("thrust", hull_number("thrust")),
            ]
        }
        "health" => vec![
            field_u64("sent", uint(value, "sent")),
            field_u64("dropped", uint(value, "dropped")),
            field_f64("simTime", number(value, "simTime")),
            field_f64("wallTime", number(value, "wallTime")),
            field_bool(
                "paused",
                value.get("paused").and_then(Value::as_bool).unwrap_or(false),
            ),
        ],
        _ => return None,
    };
    Some(build(columns))
}

/// 控制回执：`ControlAck` 的单行结构体，字段与
/// `types/usv/occsim/control/v1.yml` 的 `ControlAck` 逐列对齐。
///
/// 它不属于八个观测 kind，端口固定为 `cmd_ack`，不经过 `routing::route`。
pub fn encode_ack(ack: &crate::control::Ack) -> ArrayRef {
    build(vec![
        field_utf8("status", ack.status.as_str()),
        field_utf8("kind", ack.kind.as_str()),
        field_utf8("reason", &ack.reason),
    ])
}

fn build(columns: Vec<(Field, ArrayRef)>) -> ArrayRef {
    let (fields, arrays): (Vec<Field>, Vec<ArrayRef>) = columns.into_iter().unzip();
    Arc::new(StructArray::new(Fields::from(fields), arrays, None))
}

fn field_f64(name: &str, value: f64) -> (Field, ArrayRef) {
    (
        Field::new(name, DataType::Float64, true),
        Arc::new(Float64Array::from(vec![value])),
    )
}

fn field_u32(name: &str, value: u32) -> (Field, ArrayRef) {
    (
        Field::new(name, DataType::UInt32, true),
        Arc::new(UInt32Array::from(vec![value])),
    )
}

fn field_u64(name: &str, value: u64) -> (Field, ArrayRef) {
    (
        Field::new(name, DataType::UInt64, true),
        Arc::new(UInt64Array::from(vec![value])),
    )
}

fn field_bool(name: &str, value: bool) -> (Field, ArrayRef) {
    (
        Field::new(name, DataType::Boolean, true),
        Arc::new(BooleanArray::from(vec![value])),
    )
}

fn field_utf8(name: &str, value: &str) -> (Field, ArrayRef) {
    (
        Field::new(name, DataType::Utf8, true),
        Arc::new(StringArray::from(vec![value])),
    )
}

fn field_binary(name: &str, value: &[u8]) -> (Field, ArrayRef) {
    (
        Field::new(name, DataType::LargeBinary, true),
        Arc::new(LargeBinaryArray::from(vec![value])),
    )
}

/// 取顶层数值字段，缺失或非数值时返回 NaN。
fn number(value: &Value, key: &str) -> f64 {
    value.get(key).and_then(Value::as_f64).unwrap_or(f64::NAN)
}

/// 取数组字段的第 index 个元素，缺失时返回 NaN。
fn item(value: &Value, key: &str, index: usize) -> f64 {
    value
        .get(key)
        .and_then(|array| array.get(index))
        .and_then(Value::as_f64)
        .unwrap_or(f64::NAN)
}

fn text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn uint(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dora_node_api::arrow_v59::array::Array;
    use serde_json::json;

    fn field_names(array: &ArrayRef) -> Vec<String> {
        array
            .as_any()
            .downcast_ref::<StructArray>()
            .expect("应当是 StructArray")
            .fields()
            .iter()
            .map(|f| f.name().clone())
            .collect()
    }

    #[test]
    fn gps_maps_to_declared_fields() {
        let body = Body::Json {
            value: json!({"fix": "3D", "coordinates": [31.0, 121.0, 5.0], "velocity": [0.1, 0.2, 0.3]}),
            raw: b"{}".to_vec(),
        };
        let array = encode("gps", &body).unwrap();
        assert_eq!(
            field_names(&array),
            [
                "fix",
                "latitude",
                "longitude",
                "altitude",
                "vx",
                "vy",
                "vz",
                "quality",
                "frameId",
                "sequence",
                "captureTime"
            ]
        );
        assert_eq!(array.len(), 1);
    }

    #[test]
    fn missing_numbers_become_nan() {
        let body = Body::Json {
            value: json!({"coordinates": [1.0]}),
            raw: b"{}".to_vec(),
        };
        let array = encode("gps", &body).unwrap();
        let structure = array.as_any().downcast_ref::<StructArray>().unwrap();
        assert_eq!(structure.column(1).data_type(), &DataType::Float64);
        let latitude = structure
            .column(1)
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();
        let longitude = structure
            .column(2)
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();
        assert_eq!(latitude.value(0), 1.0);
        // 缺失项填 NaN 而不是 null：下游 to_numpy 直接得到 NaN，无需处理空值掩码。
        assert!(longitude.value(0).is_nan());
    }

    /// 网页把船体运动/推进放在 `state.state` 里，这里确认嵌套取值与 shafts 明细对得上。
    #[test]
    fn state_reads_nested_hull_fields() {
        let body = Body::Json {
            value: json!({
                "simTime": 12.5,
                "paused": false,
                "controlMode": "manual",
                "mission": "survey",
                "state": {
                    "heading": 1.25,
                    "roll": 0.01,
                    "pitch": -0.02,
                    "n": 10.0,
                    "e": 20.0,
                    "d": 0.5,
                    "throttle": 0.4,
                    "steering": -0.1,
                    "left": 0.45,
                    "right": 0.35,
                    "thrust": 120.0,
                    "shafts": [
                        {"rpm": 88.0, "thrust": 60.0},
                        {"rpm": 70.0, "thrust": 60.0}
                    ]
                }
            }),
            raw: b"{}".to_vec(),
        };
        let array = encode("state", &body).unwrap();
        let structure = array.as_any().downcast_ref::<StructArray>().unwrap();
        let column = |index: usize| {
            structure
                .column(index)
                .as_any()
                .downcast_ref::<Float64Array>()
                .unwrap()
                .value(0)
        };
        assert_eq!(column(0), 12.5); // simTime
        assert_eq!(column(4), 1.25); // heading
        assert_eq!(column(12), 0.45); // left
        assert_eq!(column(13), 0.35); // right
        assert_eq!(column(14), 88.0); // rpmPort
        assert_eq!(column(15), 70.0); // rpmStarboard
        assert_eq!(column(16), 120.0); // thrust
    }

    /// 网页没给 `state.state` 时不能 panic，整列退化成 NaN。
    #[test]
    fn state_without_hull_is_nan() {
        let body = Body::Json {
            value: json!({"simTime": 1.0}),
            raw: b"{}".to_vec(),
        };
        let array = encode("state", &body).unwrap();
        let structure = array.as_any().downcast_ref::<StructArray>().unwrap();
        let heading = structure
            .column(4)
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();
        assert!(heading.value(0).is_nan());
    }

    /// 协议文件的解析结果，只取本测试用到的部分。
    #[derive(serde::Deserialize)]
    struct Package {
        types: std::collections::BTreeMap<String, Declared>,
    }

    #[derive(serde::Deserialize)]
    struct Declared {
        arrow: String,
        #[serde(default)]
        fields: Vec<DeclaredField>,
    }

    #[derive(serde::Deserialize)]
    struct DeclaredField {
        name: String,
        r#type: String,
        #[serde(default = "default_true")]
        nullable: bool,
    }

    fn default_true() -> bool {
        true
    }

    fn arrow_name(data_type: &DataType) -> &'static str {
        match data_type {
            DataType::Float64 => "Float64",
            DataType::UInt32 => "UInt32",
            DataType::UInt64 => "UInt64",
            DataType::Boolean => "Boolean",
            DataType::Utf8 => "Utf8",
            DataType::LargeBinary => "LargeBinary",
            other => panic!("v1.yml 里没有该 Arrow 类型: {other:?}"),
        }
    }

    /// 合并各目录下的协议文件；同一类型名只应出现在一个文件里。
    ///
    /// `image` 不在其中：它复用 dora 内置的 `std/media/v1/Image`，由
    /// `image_matches_std_media_image` 单独校验。
    /// `control` 也不经过 `encode`（它是桥接的输入），放在这里只为共用解析逻辑，
    /// 由 `control_protocol_declares_expected_fields` 单独校验。
    fn declared_types() -> std::collections::BTreeMap<String, Declared> {
        let sources = [
            include_str!("../types/usv/occsim/pointcloud/v1.yml"),
            include_str!("../types/usv/occsim/gps/v1.yml"),
            include_str!("../types/usv/occsim/imu/v1.yml"),
            include_str!("../types/usv/occsim/ins/v1.yml"),
            include_str!("../types/usv/occsim/odom/v1.yml"),
            include_str!("../types/usv/occsim/state/v1.yml"),
            include_str!("../types/usv/occsim/health/v1.yml"),
            include_str!("../types/usv/occsim/control/v1.yml"),
        ];
        let mut merged = std::collections::BTreeMap::new();
        for (index, text) in sources.iter().enumerate() {
            let package: Package = serde_yaml::from_str(text)
                .unwrap_or_else(|error| panic!("第 {index} 个协议文件解析失败: {error}"));
            merged.extend(package.types);
        }
        merged
    }

    /// 自定义类型文件与编码实现是两份手写内容，这里逐字段比对，防止单边修改后运行期才报错。
    #[test]
    fn custom_kinds_match_declared_schema() {
        let declared_types = declared_types();
        let payload = json!({
            "fix": "3D",
            "coordinates": [1.0, 2.0, 3.0],
            "velocity": [1.0, 2.0, 3.0],
            "specificForce": [1.0, 2.0, 3.0],
            "angularVelocity": [1.0, 2.0, 3.0],
            "quality": "ok",
            "position": [1.0, 2.0, 3.0],
            "twist": [1.0, 2.0, 3.0],
            "heading": 0.5,
            "distance": 10.0,
            "filter": "ins",
            "simTime": 1.0,
            "paused": true,
            "controlMode": "manual",
            "mission": "survey",
            "sent": 3,
            "dropped": 0
        });
        let cases = [
            (
                "pointcloud",
                "PointCloud",
                Body::PointCloud {
                    width: 1,
                    height: 1,
                    point_count: 1,
                    points: vec![1.0, 2.0, 3.0, 0.5],
                    meta: crate::decode::ImageMeta::default(),
                },
            ),
            (
                "gps",
                "GpsFix",
                Body::Json {
                    value: payload.clone(),
                    raw: Vec::new(),
                },
            ),
            (
                "imu",
                "ImuSample",
                Body::Json {
                    value: payload.clone(),
                    raw: Vec::new(),
                },
            ),
            (
                "ins",
                "InsState",
                Body::Json {
                    value: payload.clone(),
                    raw: Vec::new(),
                },
            ),
            (
                "odom",
                "OdomState",
                Body::Json {
                    value: payload.clone(),
                    raw: Vec::new(),
                },
            ),
            (
                "state",
                "SimState",
                Body::Json {
                    value: payload.clone(),
                    raw: Vec::new(),
                },
            ),
            (
                "health",
                "Health",
                Body::Json {
                    value: payload,
                    raw: Vec::new(),
                },
            ),
        ];

        for (kind, type_name, body) in &cases {
            let declared = declared_types
                .get(*type_name)
                .unwrap_or_else(|| panic!("types/ 里缺少 {type_name}"));
            assert_eq!(declared.arrow, "Struct", "{type_name} 必须是 Struct");

            let array = encode(kind, body).unwrap();
            let actual = array.as_any().downcast_ref::<StructArray>().unwrap();
            assert_eq!(
                actual.fields().len(),
                declared.fields.len(),
                "{kind}: 字段数不一致"
            );
            for (index, expected) in declared.fields.iter().enumerate() {
                let field = &actual.fields()[index];
                assert_eq!(field.name(), &expected.name, "{kind}: 第 {index} 个字段名");
                assert_eq!(
                    arrow_name(field.data_type()),
                    expected.r#type,
                    "{kind}.{}: Arrow 类型",
                    expected.name
                );
                assert_eq!(
                    field.is_nullable(),
                    expected.nullable,
                    "{kind}.{}: 可空性",
                    expected.name
                );
            }
        }
    }

    /// `image` 复用 dora 内置的 `std/media/v1/Image`，这里按内置定义硬编码校验，
    /// 内置定义一旦变更，这个测试会先失败。
    #[test]
    fn image_matches_std_media_image() {
        let array = encode(
            "image",
            &Body::Image {
                width: 2,
                height: 1,
                channels: 1,
                encoding: "mono8".into(),
                pixels: vec![0, 255],
                meta: crate::decode::ImageMeta::default(),
            },
        )
        .unwrap();
        let structure = array.as_any().downcast_ref::<StructArray>().unwrap();
        let expected = [
            ("width", DataType::UInt32),
            ("height", DataType::UInt32),
            ("encoding", DataType::Utf8),
            ("data", DataType::LargeBinary),
        ];
        assert_eq!(structure.fields().len(), expected.len());
        for (index, (name, data_type)) in expected.iter().enumerate() {
            let field = &structure.fields()[index];
            assert_eq!(field.name(), name);
            assert_eq!(field.data_type(), data_type);
            assert!(field.is_nullable());
        }
    }

    /// 控制协议是桥接的**输入**类型，不经过 `encode`，所以按声明逐字段核对。
    #[test]
    fn control_protocol_declares_expected_fields() {
        let declared = declared_types();

        let command = declared.get("ControlCmd").expect("types/ 里缺少 ControlCmd");
        assert_eq!(command.arrow, "Struct");
        let expected = [
            ("kind", "Utf8"),
            ("throttle", "Float64"),
            ("steering", "Float64"),
            ("port", "Float64"),
            ("starboard", "Float64"),
        ];
        assert_eq!(command.fields.len(), expected.len(), "ControlCmd: 字段数");
        for (field, (name, type_name)) in command.fields.iter().zip(expected) {
            // 顺序也钉住：下游按列号或者按名取，改顺序必须先改协议。
            assert_eq!(field.name, name, "ControlCmd 字段顺序");
            assert_eq!(field.r#type, type_name, "ControlCmd.{name} 的 Arrow 类型");
            assert!(
                field.nullable,
                "ControlCmd.{name} 必须可空：缺省值用 NaN 表达，null 也要能被接受"
            );
        }

        let ack = declared.get("ControlAck").expect("types/ 里缺少 ControlAck");
        assert_eq!(ack.arrow, "Struct");
        let names: Vec<&str> = ack.fields.iter().map(|field| field.name.as_str()).collect();
        assert_eq!(names, ["status", "kind", "reason"]);
        assert!(
            ack.fields.iter().all(|field| field.r#type == "Utf8"),
            "ControlAck 的字段全是字符串"
        );
    }

    /// 回执是桥接的输出，必须与 `ControlAck` 声明逐列一致（名称、类型、可空）。
    #[test]
    fn ack_matches_declared_schema() {
        let array = encode_ack(&crate::control::Ack::new(
            crate::control::Status::Rejected,
            crate::control::Kind::Control,
            "超出 [-1, 1]",
        ));
        let structure = array.as_any().downcast_ref::<StructArray>().unwrap();
        let declared = declared_types();
        let ack = declared.get("ControlAck").expect("types/ 里缺少 ControlAck");

        assert_eq!(structure.len(), 1);
        assert_eq!(structure.fields().len(), ack.fields.len());
        for (index, declared_field) in ack.fields.iter().enumerate() {
            let field = &structure.fields()[index];
            assert_eq!(field.name(), &declared_field.name);
            assert_eq!(arrow_name(field.data_type()), declared_field.r#type);
            assert_eq!(field.is_nullable(), declared_field.nullable);
        }
    }

    #[test]
    fn unknown_kind_returns_none() {
        let body = Body::Json {
            value: json!({}),
            raw: b"{}".to_vec(),
        };
        assert!(encode("bogus", &body).is_none());
    }
}
