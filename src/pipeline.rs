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
use dora_node_api::{DoraNode, Event, EventStream, MetadataParameters, Parameter, TryRecvError};

use crate::catalog::Catalog;
use crate::decode::{Body, DecodeError, Record, decode};
use crate::encode::encode;
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
) -> Result<()> {
    let (tx, rx) = crossbeam_channel::bounded(QUEUE_LIMIT);
    let stats = Arc::new(Stats::default());
    let _subscriber = subscribe(session, tx, rx.clone(), stats.clone()).await?;
    let mut last_report = std::time::Instant::now();
    let mut unrouted: BTreeSet<(String, String)> = BTreeSet::new();

    let stop_reason = 'outer: loop {
        loop {
            match events.try_recv() {
                Ok(Event::Stop(cause)) => break 'outer format!("收到 STOP（{cause:?}）"),
                Ok(Event::Error(error)) => {
                    tracing::error!("dora 事件流错误：{error}");
                    break 'outer "事件流错误".to_string();
                }
                Ok(_) => {}
                Err(TryRecvError::Closed) => break 'outer "事件流关闭".to_string(),
                Err(TryRecvError::Empty) => break,
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
            channels, encoding, ..
        } => {
            // width/height/encoding 已在 Arrow 结构体里，这里只补一个结构体没有的量。
            params.insert("channels".into(), Parameter::Integer(*channels as i64));
            params.insert("encoding".into(), Parameter::String(encoding.clone()));
        }
        Body::PointCloud { .. } => {}
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
