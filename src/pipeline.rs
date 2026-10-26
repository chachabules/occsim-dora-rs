//! 订阅 → 解码 → Arrow → dora 的主链路。
//!
//! 线程模型：zenoh 回调跑在它自己的线程里，只做解码与入队，绝不碰 dora 句柄；
//! 主循环持有 `DoraNode`，统一负责 `send_output`，并按 50ms 节拍检查 STOP。

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{anyhow, Result};
use crossbeam_channel::{Receiver, Sender, TrySendError};
use dora_node_api::dora_core::config::DataId;
use dora_node_api::{DoraArray, DoraNode, Event, EventStream, MetadataParameters, Parameter, TryRecvError};

use crate::catalog::Catalog;
use crate::control::{self, Ack, Kind, Status};
use crate::decode::{Body, DecodeError, ImageMeta, Record, decode};
use crate::encode::{encode, encode_ack};
use crate::routing::route;

/// 队列上限：点云单帧约 1.8MB，256 帧约 460MB 封顶，够用且不会打爆内存。
const QUEUE_LIMIT: usize = 256;
/// 无事件时的轮询间隔。
const POLL_INTERVAL: Duration = Duration::from_millis(2);
/// 运行期统计日志的打印间隔。
const REPORT_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Default)]
pub struct Stats {
    pub messages: AtomicU64,
    pub invalid: AtomicU64,
    pub dropped: AtomicU64,
    pub sent: AtomicU64,
}

/// 订阅 `usv/v1/**`，返回的句柄必须由调用方持有，Drop 即取消订阅。
pub async fn subscribe(
    session: &zenoh::Session,
    tx: Sender<Record>,
    rx: Receiver<Record>,
    stats: Arc<Stats>,
) -> Result<zenoh::pubsub::Subscriber<()>> {
    let dropped = stats.clone();
    let subscriber = session
        .declare_subscriber("usv/v1/**")
        .callback(move |sample| {
            let key = sample.key_expr().to_string();
            let payload = sample.payload().to_bytes();
            match decode(&key, &payload) {
                Ok(record) => {
                    dropped.messages.fetch_add(1, Ordering::Relaxed);
                    match tx.try_send(record) {
                        Ok(()) => {}
                        Err(TrySendError::Full(record)) => {
                            // 背压：丢最旧一帧，保证回调线程永不阻塞
                            let _ = rx.try_recv();
                            let _ = tx.try_send(record);
                            dropped.dropped.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(TrySendError::Disconnected(_)) => {}
                    }
                }
                Err(DecodeError::NotObservation) => {}
                Err(error) => {
                    dropped.invalid.fetch_add(1, Ordering::Relaxed);
                    tracing::debug!("丢弃无效帧 {key}: {error}");
                }
            }
        })
        .await
        .map_err(|e| anyhow!("声明订阅失败: {e}"))?;
    Ok(subscriber)
}

/// 主循环：排空队列并发送，直到 dora 发出 STOP 或事件流出错。
///
/// 注意：dora 1.1.0 的 `recv_timeout` 在超时时抛的是 `Event::Error`
/// （`Timeout event stream error: Receiver timed out`）而不是返回 `None`，
/// 用它做空转会让节点启动即退出。这里改用 `try_recv`，`Empty` 表示正常空转。
pub async fn run(
    session: &zenoh::Session,
    node: &mut DoraNode,
    events: &mut EventStream,
    catalog: &Catalog,
    control: control::Handle,
) -> Result<()> {
    let (tx, rx) = crossbeam_channel::bounded(QUEUE_LIMIT);
    let stats = Arc::new(Stats::default());
    let _subscriber = subscribe(session, tx, rx.clone(), stats.clone()).await?;
    let mut last_report = std::time::Instant::now();
    let mut unrouted: BTreeSet<(String, String)> = BTreeSet::new();
    // 10 Hz 上游同一条错误只打一次，避免刷屏；换原因时再打。
    let mut command_error: Option<String> = None;

    let stop_reason = 'outer: loop {
        loop {
            match events.try_recv() {
                Ok(Event::Stop(cause)) => break 'outer format!("收到 STOP（{cause:?}）"),
                Ok(Event::Error(error)) => {
                    tracing::error!("dora 事件流错误：{error}");
                    break 'outer "事件流错误".to_string();
                }
                Ok(Event::Input { id, data, .. }) => {
                    // 两个控制输入都不含 `__`，不会被当成观测端口。
                    if id.to_string() == control::CMD_PORT {
                        submit_command(&control, node, &data, &mut command_error);
                    } else if id.to_string() == control::TOKEN_PORT {
                        submit_token(&control, node, &data);
                    }
                }
                Ok(_) => {}
                Err(TryRecvError::Closed) => break 'outer "事件流关闭".to_string(),
                Err(TryRecvError::Empty) => break,
            }
        }
        // 控制回执：worker 里等完 ACK 才回一条，这里只负责转成 dora 输出。
        while let Some(ack) = control.poll() {
            if let Err(error) = send_ack(node, &ack) {
                tracing::warn!("控制回执发送失败：{error}");
            }
        }
        while let Ok(record) = rx.try_recv() {
            let kind = record.parts.kind.as_str();
            let Some(port) = route(kind, &record.parts.sensor, catalog) else {
                stats.invalid.fetch_add(1, Ordering::Relaxed);
                // 每个未声明的 (sensor, kind) 只告警一次，避免 10Hz 刷屏。
                if unrouted.insert((record.parts.sensor.clone(), kind.to_string())) {
                    tracing::warn!(
                        "传感器未在 ports.yaml 声明，整类丢弃：{}/{}（补进清单后重跑 gen-dataflow 并重启数据流）",
                        record.parts.sensor,
                        kind
                    );
                }
                continue;
            };
            if let Err(error) = send_record(node, &record, &port) {
                tracing::warn!("发送失败（{}）：{error}", record.key);
            } else {
                stats.sent.fetch_add(1, Ordering::Relaxed);
            }
        }
        if last_report.elapsed() >= REPORT_INTERVAL {
            last_report = std::time::Instant::now();
            tracing::info!(
                "运行中：接收 {} 帧，发送 {} 帧，丢弃 {} 帧，无效 {} 帧",
                stats.messages.load(Ordering::Relaxed),
                stats.sent.load(Ordering::Relaxed),
                stats.dropped.load(Ordering::Relaxed),
                stats.invalid.load(Ordering::Relaxed),
            );
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    };

    // 尽力交还控制权：网页侧 350 ms 也会兜底，这里只是收个尾。
    control.shutdown().await;

    tracing::info!(
        "{}：接收 {} 帧，发送 {} 帧，丢弃 {} 帧，无效 {} 帧",
        stop_reason,
        stats.messages.load(Ordering::Relaxed),
        stats.sent.load(Ordering::Relaxed),
        stats.dropped.load(Ordering::Relaxed),
        stats.invalid.load(Ordering::Relaxed),
    );
    Ok(())
}

/// dora 输入 → 控制命令。解析失败、队列满都立刻回 rejected：
/// 让上游知道"这一帧没执行"，而不是以为艇动了。
fn submit_command(
    handle: &control::Handle,
    node: &mut DoraNode,
    data: &DoraArray,
    last_error: &mut Option<String>,
) {
    match control::parse(data.as_array()) {
        Ok(command) => {
            *last_error = None;
            if !handle.submit(command.clone()) {
                reject(node, command.kind, "控制队列积压，丢弃这一帧".into());
            }
        }
        // kind 都没读出来时只能用 control 占位
        Err(reason) => {
            if last_error.as_deref() != Some(reason.as_str()) {
                tracing::warn!("控制指令被丢弃：{reason}");
                *last_error = Some(reason.clone());
            }
            reject(node, Kind::Control, reason);
        }
    }
}

/// dora 输入 → 临时令牌。空串表示清除令牌，桥接会交还控制权并断开。
fn submit_token(handle: &control::Handle, node: &mut DoraNode, data: &DoraArray) {
    match control::parse_token(data.as_array()) {
        Ok(token) => {
            if !handle.submit_token(token) {
                reject(node, Kind::Token, "控制队列积压，令牌未下发".into());
            }
        }
        Err(reason) => {
            tracing::warn!("令牌被丢弃：{reason}");
            reject(node, Kind::Token, reason);
        }
    }
}

/// 立刻回一条 rejected：让上游知道「这一帧没执行」，而不是以为艇动了。
fn reject(node: &mut DoraNode, kind: Kind, reason: String) {
    if let Err(error) = send_ack(node, &Ack::new(Status::Rejected, kind, reason)) {
        tracing::debug!("控制回执发送失败（端口可能未声明）：{error}");
    }
}

fn send_ack(node: &mut DoraNode, ack: &Ack) -> Result<()> {
    let mut params = MetadataParameters::default();
    params.insert("source".into(), Parameter::String("occsim".into()));
    params.insert("kind".into(), Parameter::String("ack".into()));
    params.insert(
        "status".into(),
        Parameter::String(ack.status.as_str().to_string()),
    );
    params.insert(
        "command".into(),
        Parameter::String(ack.kind.as_str().to_string()),
    );
    node.send_output(
        DataId::from(control::ACK_PORT.to_string()),
        params,
        encode_ack(ack),
    )
    .map_err(|e| anyhow!("send_output({}): {e}", control::ACK_PORT))
}

/// 按 kind 编码成单行 `StructArray`，选好端口后连同 metadata 一并发出。
fn send_record(node: &mut DoraNode, record: &Record, port: &str) -> Result<()> {
    let kind = record.parts.kind.as_str();

    let mut params = MetadataParameters::default();
    params.insert("source".into(), Parameter::String("occsim".into()));
    params.insert("key".into(), Parameter::String(record.key.clone()));
    params.insert("kind".into(), Parameter::String(kind.to_string()));
    params.insert("sensor".into(), Parameter::String(record.parts.sensor.clone()));
    params.insert("instance".into(), Parameter::String(record.parts.instance.clone()));
    params.insert("run_id".into(), Parameter::String(record.parts.run_id.clone()));
    params.insert("size".into(), Parameter::Integer(record.size as i64));

    match &record.body {
        Body::Image {
            channels,
            encoding,
            meta,
            ..
        } => add_image_params(&mut params, *channels, encoding, meta),
        // 点云同样带雷达几何量；width/height/pointCount 已在结构体里
        Body::PointCloud { meta, .. } => put_scan_params(&mut params, meta),
        Body::Json { raw, .. } => {
            // 原始 JSON 一并带上，便于下游按需取结构体里没有的字段。
            params.insert(
                "json".into(),
                Parameter::String(String::from_utf8_lossy(raw).into_owned()),
            );
        }
    }

    let array = encode(kind, &record.body)
        .ok_or_else(|| anyhow!("{kind} 没有对应的输出类型定义"))?;
    node.send_output(DataId::from(port.to_string()), params, array)
        .map_err(|e| anyhow!("send_output({port}): {e}"))
}

/// 图像专属 metadata：通道/编码总是带上，雷达极坐标只在雷达帧上带。
///
/// width/height/encoding 已在 Arrow 结构体里，`image` 又要与内置的
/// `std/media/v1/Image` 保持一致不能加列，所以雷达的量只能走 Parameter。
fn add_image_params(
    params: &mut MetadataParameters,
    channels: u8,
    encoding: &str,
    meta: &ImageMeta,
) {
    params.insert("channels".into(), Parameter::Integer(channels as i64));
    params.insert("encoding".into(), Parameter::String(encoding.to_string()));
    put_scan_params(params, meta);
}

/// 图像与点云共用：采集时刻 + 雷达极坐标（有量程且知道天线指向才算雷达帧）。
///
/// 这些量不进 Arrow 结构体：`image` 必须与内置 `std/media/v1/Image` 一致，
/// 点云的 width/height/pointCount 也已经在列里，几何参数只能走 Parameter。
fn put_scan_params(params: &mut MetadataParameters, meta: &ImageMeta) {
    put_float(params, "captureTime", meta.capture_time);
    if !meta.is_radar() {
        return;
    }
    put_float(params, "range", meta.range);
    put_float(params, "azimuth", meta.azimuth);
    put_float(params, "antennaHeading", meta.antenna_heading);
    put_float(params, "rotationRpm", meta.rotation_rpm);
    if let Some(heading_up) = meta.heading_up {
        params.insert("headingUp".into(), Parameter::Bool(heading_up));
    }
}

/// 缺失或非有限值就不写这个键，避免下游拿到 NaN 还要自己判断。
fn put_float(params: &mut MetadataParameters, key: &str, value: Option<f64>) {
    if let Some(value) = value.filter(|value| value.is_finite()) {
        params.insert(key.into(), Parameter::Float(value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn radar_meta() -> ImageMeta {
        ImageMeta {
            capture_time: Some(550.04),
            range: Some(926.0),
            azimuth: Some(3.2289),
            antenna_heading: Some(-3.099),
            rotation_rpm: Some(24.0),
            heading_up: Some(true),
        }
    }

    #[test]
    fn radar_image_params_carry_polar_fields() {
        let mut params = MetadataParameters::default();
        add_image_params(&mut params, 1, "mono8", &radar_meta());

        assert_eq!(params.get("range"), Some(&Parameter::Float(926.0)));
        assert_eq!(params.get("azimuth"), Some(&Parameter::Float(3.2289)));
        assert_eq!(params.get("antennaHeading"), Some(&Parameter::Float(-3.099)));
        assert_eq!(params.get("rotationRpm"), Some(&Parameter::Float(24.0)));
        assert_eq!(params.get("captureTime"), Some(&Parameter::Float(550.04)));
        assert_eq!(params.get("headingUp"), Some(&Parameter::Bool(true)));
        // 结构体里已有的量照旧走 Parameter，不重复加列。
        assert_eq!(params.get("channels"), Some(&Parameter::Integer(1)));
        assert_eq!(params.get("encoding"), Some(&Parameter::String("mono8".into())));
    }

    /// 相机没有雷达字段：不能凭空写 range/rpm，但采集时刻要有。
    #[test]
    fn camera_image_params_skip_radar_fields() {
        let meta = ImageMeta {
            capture_time: Some(592.68),
            ..ImageMeta::default()
        };
        let mut params = MetadataParameters::default();
        add_image_params(&mut params, 4, "rgba8", &meta);

        for key in ["range", "azimuth", "antennaHeading", "rotationRpm", "headingUp"] {
            assert!(!params.contains_key(key), "{key} 不该出现在相机帧上");
        }
        assert_eq!(params.get("captureTime"), Some(&Parameter::Float(592.68)));
    }

    /// 量程/方位缺失时不算雷达帧，也不会写出半套参数。
    #[test]
    fn range_only_is_not_radar() {
        let meta = ImageMeta {
            range: Some(926.0),
            ..ImageMeta::default()
        };
        assert!(!meta.is_radar());
        let mut params = MetadataParameters::default();
        add_image_params(&mut params, 1, "mono8", &meta);
        assert!(!params.contains_key("range"));
    }
}
