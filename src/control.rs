//! 控制出口：dora 输入 → OccSim 网页的 Zenoh 租约协议。
//!
//! 网页侧的约定与 `../integration/udp_bridge.py` 完全一致：
//!
//! ```text
//! 订阅  <prefix>/state, <prefix>/ack/<clientId>
//! 发布  <prefix>/cmd/{lease,control,estop,release}
//! ```
//!
//! 一条指令要活下来必须同时满足：有临时令牌、state 0.7 s 内更新过、仿真未暂停、
//! 且 `control` 带 `validUntil = simTime + 0.5`。所以这里**只转发不保持**：
//! 上游持续发才有用，桥接不做"记住最后一条指令反复重发"，否则上游一崩艇会继续跑。
//!
//! 令牌（pairToken）不在 dataflow.yml 里：它由操控页通过 `cmd_token` 端口下发，
//! 桥接只存在内存里。拿到令牌才去发现运行前缀、订阅 state/ack；换令牌会交还旧
//! 租约并重新连接（网页复位后前缀会变，正好靠这条路恢复）。
//!
//! 线程模型：网络等待（最长 0.8 s 的 ACK 超时）全在 worker 任务里，主循环只
//! 做 `try_send` / `try_recv`，观测链路永远不会被控制链路拖住。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Result};
use crossbeam_channel::{Receiver, Sender};
use dora_node_api::arrow_v59::array::{Array, ArrayRef, Float64Array, StringArray, StructArray};
use serde_json::{Map, Value, json};
use tokio::sync::{mpsc, oneshot};
use zenoh::Session;

/// 控制指令输入端口。
pub const CMD_PORT: &str = "cmd";
/// 临时令牌输入端口。
pub const TOKEN_PORT: &str = "cmd_token";
/// 回执输出端口。三个端口都不含 `__`，不会被当成 `{sensor}__{kind}` 观测端口。
pub const ACK_PORT: &str = "cmd_ack";

/// 没有令牌时的拒绝原因（面板会原样显示）。
pub const REASON_NO_TOKEN: &str = "控制未启用：先在操控页填入临时令牌";

/// 单包 control 的有效期：`validUntil = simTime + HOLD_SECONDS`。
///
/// 网页拿它当租约的存活期：一旦过期，紧接着的 control 会被拒为
/// 「外部控制需要有效唯一租约」，并清零推进。所以这个窗口必须**大于**
/// `IDLE_RELEASE`——只要还没到主动交还的时机，租约就不能先一步过期。
/// 曾经取 0.25 s，实测上游抖到 212 ms 时上一包刚好过期，下一包就被拒。
pub const HOLD_SECONDS: f64 = 0.5;
/// 上游静默多久就主动交还控制权（网页自身 350 ms 兜底，这里略早收尾）。
const IDLE_RELEASE: Duration = Duration::from_millis(400);
/// 等浏览器 ACK 的上限。
const ACK_TIMEOUT: Duration = Duration::from_millis(800);
/// 网页 state 的新鲜度上限。
const STATE_TTL: Duration = Duration::from_millis(700);
/// 同值指令的最小重发间隔；远低于 500 ms 有效期，又不会被 20 Hz 上游打爆。
const MIN_RESEND: Duration = Duration::from_millis(90);
/// 回执队列上限；主循环每 2 ms 取一次，正常不会满。
const REPORT_LIMIT: usize = 64;
/// 发现网页实例的查询超时。
const DISCOVER_TIMEOUT: Duration = Duration::from_secs(3);
/// 连接失败的退避间隔。
const RETRY: Duration = Duration::from_secs(2);

/// 指令种类，取值与 dora 里的 `kind` 字符串一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Control,
    Estop,
    Release,
    /// 不是发给网页的指令，只用于标记"令牌下发"的回执。
    Token,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Control => "control",
            Kind::Estop => "estop",
            Kind::Release => "release",
            Kind::Token => "token",
        }
    }
}

/// 回执状态；网页只会给 accepted / rejected，`released` 是本进程补的静默释放。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Accepted,
    Rejected,
    Released,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Accepted => "accepted",
            Status::Rejected => "rejected",
            Status::Released => "released",
        }
    }
}

/// 一条回执，主循环会把它编码成 `ControlAck` 发给下游。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ack {
    pub status: Status,
    pub kind: Kind,
    pub reason: String,
}

impl Ack {
    pub fn new(status: Status, kind: Kind, reason: impl Into<String>) -> Self {
        Self {
            status,
            kind,
            reason: reason.into(),
        }
    }
}

/// 一次控制意图。`None` 表示这个量上游没给（dora 里用 NaN 表达）。
#[derive(Debug, Clone, PartialEq)]
pub struct Command {
    pub kind: Kind,
    pub throttle: Option<f64>,
    pub steering: Option<f64>,
    pub port: Option<f64>,
    pub starboard: Option<f64>,
}

impl Command {
    /// 协议校验：范围、推进二选一、`control` 必须有推进量。
    fn validate(&self) -> Result<(), String> {
        for (name, value) in [
            ("throttle", self.throttle),
            ("steering", self.steering),
            ("port", self.port),
            ("starboard", self.starboard),
        ] {
            if let Some(value) = value.filter(|value| !(-1.0..=1.0).contains(value)) {
                return Err(format!("{name}={value} 超出 [-1, 1]"));
            }
        }
        if self.kind != Kind::Control {
            return Ok(());
        }
        match (self.throttle, self.port, self.starboard) {
            (Some(_), None, None) => Ok(()),
            (None, Some(_), Some(_)) => Ok(()),
            (Some(_), _, _) => Err("throttle 与 port/starboard 不能同时给".into()),
            (None, Some(_), None) | (None, None, Some(_)) => {
                Err("port 与 starboard 必须成对下发".into())
            }
            (None, None, None) => Err("kind=control 但没有推进量".into()),
        }
    }
}

/// 把 dora 输入的一行 `ControlCmd` 读成命令；非法组合返回原因，直接回 rejected。
///
/// 一批多行时只认最后一行：后面的是更新的指令。
pub fn parse(array: &ArrayRef) -> Result<Command, String> {
    let rows = structure(array, "cmd")?;
    let row = rows
        .len()
        .checked_sub(1)
        .ok_or_else(|| "cmd 是空批次".to_string())?;

    let text = column_string(rows, "kind", row)?;
    let kind = match text.trim() {
        "control" => Kind::Control,
        "estop" => Kind::Estop,
        "release" => Kind::Release,
        other => return Err(format!("kind 只能是 control/estop/release，收到 {other:?}")),
    };
    let command = Command {
        kind,
        throttle: column_number(rows, "throttle", row)?,
        steering: column_number(rows, "steering", row)?,
        port: column_number(rows, "port", row)?,
        starboard: column_number(rows, "starboard", row)?,
    };
    command.validate()?;
    Ok(command)
}

/// 把 dora 输入的一行 `ControlToken` 读成令牌字符串（会 trim）。
pub fn parse_token(array: &ArrayRef) -> Result<String, String> {
    let rows = structure(array, TOKEN_PORT)?;
    let row = rows
        .len()
        .checked_sub(1)
        .ok_or_else(|| format!("{TOKEN_PORT} 是空批次"))?;
    Ok(column_string(rows, "token", row)?.trim().to_string())
}

fn structure<'a>(array: &'a ArrayRef, port: &str) -> Result<&'a StructArray, String> {
    array
        .as_any()
        .downcast_ref::<StructArray>()
        .ok_or_else(|| format!("{port} 必须是结构体"))
}

fn column_string(rows: &StructArray, name: &str, index: usize) -> Result<String, String> {
    let column = rows
        .column_by_name(name)
        .ok_or_else(|| format!("缺少列 {name}"))?;
    let values = column
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| format!("{name} 必须是 Utf8"))?;
    if values.is_null(index) {
        return Err(format!("{name} 不能为空"));
    }
    Ok(values.value(index).to_string())
}

/// 缺失的列、null、NaN 一律当作「没给」：0 是有效指令，不能拿来当缺省。
fn column_number(rows: &StructArray, name: &str, index: usize) -> Result<Option<f64>, String> {
    let Some(column) = rows.column_by_name(name) else {
        return Ok(None);
    };
    let values = column
        .as_any()
        .downcast_ref::<Float64Array>()
        .ok_or_else(|| format!("{name} 必须是 Float64"))?;
    if values.is_null(index) {
        return Ok(None);
    }
    let value = values.value(index);
    Ok(value.is_finite().then_some(value))
}

/// 令牌指纹：日志与错误信息里只出现这个，明文一律不出进程。
pub fn fingerprint(token: &str) -> String {
    if token.is_empty() {
        return "未配置".into();
    }
    let head: String = token.chars().take(4).collect();
    format!("{head}…（共 {} 位）", token.chars().count())
}

/// 网页 state 的极简视图，只留控制要用的几个量。
///
/// 刻意不缓存 `lease` 字段：网页作废租约后仍会保留旧 clientId / expires，
/// 存下来只会误导"还持有控制权"的判断（详见 `Link::apply`）。
#[derive(Debug, Default)]
struct State {
    sim_time: Option<f64>,
    paused: bool,
    seen_at: Option<Instant>,
}

type Waiters = Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>>;
type SharedState = Arc<Mutex<State>>;

/// 与网页之间的一条控制链路（一个连接周期内有效）。
struct Link {
    session: Session,
    prefix: String,
    instance: String,
    run_id: String,
    client_id: String,
    lease_id: String,
    token: String,
    state: SharedState,
    waiters: Waiters,
    leased: bool,
}

impl Link {
    /// 前置检查并返回当前仿真时刻。
    fn check_fresh(&self, kind: Kind) -> Result<f64, String> {
        let state = self.state.lock().unwrap();
        let Some(seen) = state.seen_at else {
            return Err("还没收到网页 state：确认网页已连接、运行前缀正确".into());
        };
        if seen.elapsed() > STATE_TTL {
            return Err("网页 state 已过期（>0.7 s）：网页可能已断开或复位，换新令牌可重连".into());
        }
        if kind == Kind::Control && state.paused {
            return Err("仿真已暂停：先在网页点「开始仿真」".into());
        }
        state
            .sim_time
            .ok_or_else(|| "网页 state 里没有 simTime".to_string())
    }

    /// 执行一条指令：租约 → 下发 → 解析 ACK，必要时清理控制权。
    async fn apply(&mut self, command: Command) -> Ack {
        let kind = command.kind;
        if self.token.is_empty() {
            return Ack::new(Status::Rejected, kind, REASON_NO_TOKEN);
        }
        if kind == Kind::Release && !self.leased {
            return Ack::new(Status::Accepted, kind, "当前没有控制权，无需释放");
        }
        // 1) 本地能判的先判掉，省一次 0.8 s 的 ACK 超时
        if let Err(reason) = self.check_fresh(kind) {
            return Ack::new(Status::Rejected, kind, reason);
        }
        // 2) 每条指令前都重新取一次租约。
        //
        //    网页的租约很短，而且控制流一断（约 350 ms）就自行作废并清零推进，
        //    但 `state.lease` 字段仍然留着旧 clientId 与未来的 expires，没法据此
        //    判断"还有没有控制权"。只要中间断过一帧，跳过租约就会收到
        //    "外部控制需要有效唯一租约"。所以照 `../integration/udp_bridge.py`
        //    的做法，不缓存、不推断，每条都先租。
        //
        //    release 不需要租约：走到这里的 release 一定还持有控制权。
        if matches!(kind, Kind::Control | Kind::Estop) {
            match self.issue("lease", Map::new()).await {
                Ok(value) if !is_rejected(&value) => self.leased = true,
                Ok(value) => return Ack::new(Status::Rejected, kind, refused(kind, &value)),
                Err(reason) => return Ack::new(Status::Rejected, kind, reason),
            }
        }
        // 3) 组控制量；validUntil 必须用刚读到的 simTime
        let mut extra = Map::new();
        if kind == Kind::Control {
            let sim_time = match self.check_fresh(kind) {
                Ok(sim_time) => sim_time,
                Err(reason) => {
                    self.release().await;
                    return Ack::new(Status::Rejected, kind, reason);
                }
            };
            put_number(&mut extra, "throttle", command.throttle);
            put_number(&mut extra, "steering", command.steering);
            put_number(&mut extra, "port", command.port);
            put_number(&mut extra, "starboard", command.starboard);
            extra.insert("validUntil".into(), json!(sim_time + HOLD_SECONDS));
        }

        match self.issue(kind.as_str(), extra).await {
            Ok(value) if !is_rejected(&value) => {
                if kind != Kind::Control {
                    // estop 与 release 之后网页都不再认旧租约
                    self.leased = false;
                }
                Ack::new(Status::Accepted, kind, applied(kind, &value))
            }
            Ok(value) => {
                if kind == Kind::Control {
                    self.release().await;
                }
                self.leased = false;
                Ack::new(Status::Rejected, kind, refused(kind, &value))
            }
            Err(reason) => {
                if kind == Kind::Control {
                    self.release().await;
                }
                Ack::new(Status::Rejected, kind, reason)
            }
        }
    }

    /// 交还控制权；没有控制权时直接返回成功（幂等）。
    async fn release(&mut self) -> Ack {
        if !self.leased {
            return Ack::new(Status::Accepted, Kind::Release, "当前没有控制权，无需释放");
        }
        self.leased = false;
        match self.issue("release", Map::new()).await {
            Ok(value) if !is_rejected(&value) => {
                Ack::new(Status::Accepted, Kind::Release, "已交还控制权")
            }
            Ok(value) => Ack::new(Status::Rejected, Kind::Release, refused(Kind::Release, &value)),
            Err(reason) => Ack::new(Status::Rejected, Kind::Release, reason),
        }
    }

    /// 发一条 `cmd/<kind>` 并等对应 requestId 的 ACK。
    async fn issue(&self, kind: &str, extra: Map<String, Value>) -> Result<Value, String> {
        let request = next_id();
        let (tx, rx) = oneshot::channel();
        self.waiters.lock().unwrap().insert(request.clone(), tx);

        let mut body = Map::new();
        body.insert("kind".into(), json!(kind));
        body.insert("requestId".into(), json!(request));
        body.insert("instanceId".into(), json!(self.instance));
        body.insert("runId".into(), json!(self.run_id));
        body.insert("clientId".into(), json!(self.client_id));
        body.insert("leaseId".into(), json!(self.lease_id));
        body.insert("pairToken".into(), json!(self.token));
        body.extend(extra);

        let key = format!("{}/cmd/{kind}", self.prefix);
        let payload = match serde_json::to_vec(&Value::Object(body)) {
            Ok(payload) => payload,
            Err(error) => {
                self.waiters.lock().unwrap().remove(&request);
                return Err(format!("编码 {kind} 失败: {error}"));
            }
        };
        // 默认编码（zenoh/bytes）：`encoding()` 挂在 zenoh 的 unstable 特性上，
        // 网页按 JSON 文本解析，不依赖 encoding 元数据。
        if let Err(error) = self.session.put(key, payload).await {
            self.waiters.lock().unwrap().remove(&request);
            return Err(format!("下发 {kind} 失败: {error}"));
        }
        match tokio::time::timeout(ACK_TIMEOUT, rx).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(_)) => Err("ACK 等待通道已关闭".into()),
            Err(_) => {
                self.waiters.lock().unwrap().remove(&request);
                Err("等浏览器 ACK 超时：确认网页已连接、令牌有效".into())
            }
        }
    }
}

/// 网页成功时回 `status = "applied"`（个别路径回 `accepted`），只有 `rejected` 才是失败。
///
/// 判据与 `../integration/udp_bridge.py` 一致：只把 `rejected` 当失败。缺 `status`
/// 的畸形回执同样按失败处理，避免把解析错误当成"已应用"。
fn is_rejected(value: &Value) -> bool {
    match value.get("status").and_then(Value::as_str) {
        Some("rejected") | None => true,
        Some(_) => false,
    }
}

fn explain(value: &Value) -> String {
    value
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// 成功回执通常不带 `reason`，给面板一句能读的话。
fn applied(kind: Kind, value: &Value) -> String {
    let reason = explain(value);
    if reason.is_empty() {
        format!("{} 已应用", kind.as_str())
    } else {
        reason
    }
}

/// 拒绝回执一般带 `reason`；没带也要说清楚，别让面板空着一条横杠。
fn refused(kind: Kind, value: &Value) -> String {
    let reason = explain(value);
    if reason.is_empty() {
        format!("网页拒绝了{}（未给原因）", kind.as_str())
    } else {
        reason
    }
}

fn put_number(map: &mut Map<String, Value>, key: &str, value: Option<f64>) {
    if let Some(value) = value {
        map.insert(key.to_string(), json!(value));
    }
}

/// 主循环持有的控制句柄：提交指令/令牌与取回执都不阻塞。
pub struct Handle {
    jobs: mpsc::Sender<Job>,
    reports: Receiver<Ack>,
    done: oneshot::Receiver<()>,
}

impl Handle {
    /// 提交一条控制指令；队列满（正常不会）说明 worker 卡住了。
    pub fn submit(&self, command: Command) -> bool {
        self.jobs.try_send(Job::Run(command)).is_ok()
    }

    /// 下发令牌；空串表示清除令牌并交还控制权。
    pub fn submit_token(&self, token: String) -> bool {
        self.jobs.try_send(Job::Token(token)).is_ok()
    }

    /// 取一条回执，非阻塞。
    pub fn poll(&self) -> Option<Ack> {
        self.reports.try_recv().ok()
    }

    /// 收尾：让 worker 尽力 release 后退出。超时也不阻塞数据流退出。
    pub async fn shutdown(self) {
        let _ = self.jobs.send(Job::Stop).await;
        let _ = tokio::time::timeout(Duration::from_millis(1500), self.done).await;
    }
}

enum Job {
    Run(Command),
    Token(String),
    Stop,
}

/// 连接周期内的一步：要么继续（重连），要么整体收工。
enum Step {
    Reconnect,
    Stop,
}

/// 起控制链路。不需要令牌就能起：拿到令牌后才会去发现前缀并连接网页。
pub fn start(session: Session, prefix_override: Option<String>) -> Handle {
    let (jobs_tx, jobs_rx) = mpsc::channel(32);
    let (report_tx, report_rx) = crossbeam_channel::bounded(REPORT_LIMIT);
    let (done_tx, done_rx) = oneshot::channel();

    tokio::spawn(async move {
        work(session, prefix_override, jobs_rx, report_tx).await;
        let _ = done_tx.send(());
    });

    Handle {
        jobs: jobs_tx,
        reports: report_rx,
        done: done_rx,
    }
}

/// worker：没令牌就等令牌；有令牌就进入一个连接周期，换令牌/断开后重来。
async fn work(
    session: Session,
    prefix_override: Option<String>,
    mut jobs: mpsc::Receiver<Job>,
    reports: Sender<Ack>,
) {
    let mut token = String::new();
    loop {
        if token.trim().is_empty() {
            match jobs.recv().await {
                None | Some(Job::Stop) => break,
                Some(Job::Token(next)) => token = next,
                // 没令牌时的控制指令立刻拒绝，别让面板以为在动
                Some(Job::Run(command)) => {
                    let _ = reports.try_send(Ack::new(Status::Rejected, command.kind, REASON_NO_TOKEN));
                }
            }
            continue;
        }
        if let Step::Stop = connected(&session, prefix_override.as_deref(), &mut token, &mut jobs, &reports).await {
            break;
        }
    }
}

/// 一个连接周期：发现前缀 → 订阅 state/ack → 收发指令，直到换令牌或收工。
///
/// 订阅句柄只在本次调用里活着，函数返回即撤销。
async fn connected(
    session: &Session,
    prefix_override: Option<&str>,
    token: &mut String,
    jobs: &mut mpsc::Receiver<Job>,
    reports: &Sender<Ack>,
) -> Step {
    // 1) 发现运行前缀：网页还没连上就每 2 s 重试，期间照常收令牌/停止
    let mut warned = false;
    let prefix = loop {
        match discover_prefix(session, prefix_override).await {
            Ok(prefix) => break prefix,
            Err(error) => {
                if !warned {
                    warned = true;
                    tracing::warn!("控制链路等待网页：{error}");
                }
                let reason = error.to_string();
                tokio::select! {
                    _ = tokio::time::sleep(RETRY) => {}
                    job = jobs.recv() => match job {
                        None | Some(Job::Stop) => return Step::Stop,
                        Some(Job::Token(next)) => {
                            if next != *token {
                                *token = next;
                                return Step::Reconnect;
                            }
                        }
                        Some(Job::Run(command)) => {
                            let _ = reports.try_send(Ack::new(Status::Rejected, command.kind, reason));
                        }
                    },
                }
            }
        }
    };

    let instance = prefix.split('/').nth(2).unwrap_or_default().to_string();
    let run_id = prefix.rsplit('/').next().unwrap_or_default().to_string();
    let client_id = format!("dora-{}", next_id());
    let lease_id = next_id();

    let state: SharedState = Arc::new(Mutex::new(State::default()));
    let waiters: Waiters = Arc::new(Mutex::new(HashMap::new()));

    let seen = state.clone();
    let expected_run = run_id.clone();
    let state_sub = match session
        .declare_subscriber(format!("{prefix}/state"))
        .callback(move |sample| on_state(&seen, &expected_run, &sample))
        .await
    {
        Ok(subscriber) => subscriber,
        Err(error) => {
            tracing::warn!("订阅 {prefix}/state 失败：{error}");
            tokio::time::sleep(RETRY).await;
            return Step::Reconnect;
        }
    };

    let waiting = waiters.clone();
    let ack_key = format!("{prefix}/ack/{client_id}");
    let ack_sub = match session
        .declare_subscriber(ack_key)
        .callback(move |sample| on_ack(&waiting, &sample))
        .await
    {
        Ok(subscriber) => subscriber,
        Err(error) => {
            tracing::warn!("订阅 {prefix}/ack 失败：{error}");
            tokio::time::sleep(RETRY).await;
            return Step::Reconnect;
        }
    };

    tracing::info!(
        "控制链路已连接：{prefix}，令牌 {}",
        fingerprint(token.trim())
    );
    // 令牌生效：面板据此从「控制未启用」切到可操作
    let _ = reports.try_send(Ack::new(
        Status::Accepted,
        Kind::Token,
        format!("已连接网页：{prefix}"),
    ));

    let mut link = Link {
        session: session.clone(),
        prefix,
        instance,
        run_id,
        client_id,
        lease_id,
        token: token.clone(),
        state,
        waiters,
        leased: false,
    };
    // 订阅随本次连接存活，返回时自动撤销
    let _subscriptions = (state_sub, ack_sub);

    let mut previous: Option<(Command, Instant)> = None;
    // 只在回执状态翻转时落一行日志，10 Hz 不会刷屏
    let mut last_status: Option<Status> = None;
    loop {
        let timeout = if link.leased {
            IDLE_RELEASE
        } else {
            Duration::from_secs(30)
        };
        match tokio::time::timeout(timeout, jobs.recv()).await {
            Err(_) => {
                if link.leased {
                    link.release().await;
                    tracing::info!(
                        "上游静默 {} ms，已释放控制权",
                        IDLE_RELEASE.as_millis()
                    );
                    let _ = reports.try_send(Ack::new(
                        Status::Released,
                        Kind::Release,
                        format!("上游静默 {} ms，已释放控制权", IDLE_RELEASE.as_millis()),
                    ));
                }
                previous = None;
            }
            Ok(None) | Ok(Some(Job::Stop)) => {
                link.release().await;
                return Step::Stop;
            }
            Ok(Some(Job::Token(next))) => {
                if next == *token {
                    let _ = reports.try_send(Ack::new(
                        Status::Accepted,
                        Kind::Token,
                        "令牌未变化，无需重连",
                    ));
                    continue;
                }
                link.release().await;
                let cleared = next.trim().is_empty();
                *token = next;
                let _ = reports.try_send(Ack::new(
                    Status::Accepted,
                    Kind::Token,
                    if cleared {
                        "令牌已清除，控制关闭"
                    } else {
                        "令牌已更新，正在重连网页"
                    },
                ));
                return Step::Reconnect;
            }
            Ok(Some(Job::Run(command))) => {
                // 同值指令在 90 ms 内不重发：10 Hz 上游不会被吞，20 Hz 也不会打爆网页
                if previous
                    .as_ref()
                    .is_some_and(|(last, at)| *last == command && at.elapsed() < MIN_RESEND)
                {
                    continue;
                }
                let ack = link.apply(command.clone()).await;
                previous = Some((command, Instant::now()));
                if last_status != Some(ack.status) {
                    last_status = Some(ack.status);
                    match ack.status {
                        Status::Accepted => {
                            tracing::info!("控制已受理（{}）", ack.kind.as_str())
                        }
                        _ => tracing::warn!(
                            "控制{}（{}）：{}",
                            ack.status.as_str(),
                            ack.kind.as_str(),
                            ack.reason
                        ),
                    }
                }
                let _ = reports.try_send(ack);
            }
        }
    }
}

fn on_state(shared: &SharedState, expected_run: &str, sample: &zenoh::sample::Sample) {
    let Ok(value) = serde_json::from_slice::<Value>(&sample.payload().to_bytes()) else {
        return;
    };
    // 网页复位后旧前缀的 state 不再采信
    if value
        .get("runId")
        .and_then(Value::as_str)
        .is_some_and(|run| run != expected_run)
    {
        return;
    }
    let mut state = shared.lock().unwrap();
    state.sim_time = value.get("simTime").and_then(Value::as_f64);
    state.paused = value.get("paused").and_then(Value::as_bool).unwrap_or(false);
    state.seen_at = Some(Instant::now());
}

fn on_ack(waiters: &Waiters, sample: &zenoh::sample::Sample) {
    let Ok(value) = serde_json::from_slice::<Value>(&sample.payload().to_bytes()) else {
        return;
    };
    let Some(request) = value.get("requestId").and_then(Value::as_str) else {
        return;
    };
    let sender = waiters.lock().unwrap().remove(request);
    if let Some(sender) = sender {
        let _ = sender.send(value);
    }
}

/// 从 `<prefix>/active` 发现运行前缀；显式给定则只做格式校验。
pub async fn discover_prefix(session: &Session, given: Option<&str>) -> Result<String> {
    let prefix = match given {
        Some(prefix) => prefix.to_string(),
        None => {
            let query = async {
                let replies = session
                    .get("usv/v1/*/*/*/active")
                    .await
                    .map_err(|error| anyhow!("查询 active 失败: {error}"))?;
                let mut found: Vec<String> = Vec::new();
                while let Ok(reply) = replies.recv_async().await {
                    let Ok(sample) = reply.result() else {
                        continue;
                    };
                    let Ok(value) = serde_json::from_slice::<Value>(&sample.payload().to_bytes())
                    else {
                        continue;
                    };
                    if let Some(prefix) = value.get("prefix").and_then(Value::as_str) {
                        found.push(prefix.to_string());
                        if found.len() > 1 {
                            break;
                        }
                    }
                }
                Ok::<Vec<String>, anyhow::Error>(found)
            };
            let found = match tokio::time::timeout(DISCOVER_TIMEOUT, query).await {
                Ok(Ok(found)) => found,
                Ok(Err(error)) => return Err(error),
                Err(_) => bail!("查询 active 超时（{DISCOVER_TIMEOUT:?}）"),
            };
            match found.len() {
                1 => found.into_iter().next().unwrap_or_default(),
                0 => bail!("没找到网页实例：先把 OccSim 网页连上，或用 --run-prefix 指定"),
                _ => bail!("发现多个网页实例：用 --run-prefix 指定要控制哪一个"),
            }
        }
    };
    validate_prefix(&prefix)?;
    Ok(prefix)
}

/// `usv/v1/<实例>/<场景>/<艇>/<运行>`，四段标识都只允许 `[A-Za-z0-9_-]{1,64}`。
fn validate_prefix(prefix: &str) -> Result<()> {
    let parts: Vec<&str> = prefix.split('/').collect();
    if parts.len() != 6 || parts[0] != "usv" || parts[1] != "v1" {
        bail!("运行前缀格式不对：应为 usv/v1/<实例>/<场景>/<艇>/<运行>，收到 {prefix}");
    }
    for part in &parts[2..] {
        if part.is_empty()
            || part.len() > 64
            || !part
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
        {
            bail!("运行前缀里的标识只能含字母数字/下划线/连字符（1..64）：{part}");
        }
    }
    Ok(())
}

/// 12 位十六进制短 id：requestId / clientId / leaseId 共用。
fn next_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos() as u64)
        .unwrap_or(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mixed = nanos
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .rotate_left(17)
        ^ counter;
    format!("{mixed:016x}")[..12].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use dora_node_api::arrow_v59::datatypes::{DataType, Field};

    fn command_row(command: &Command) -> ArrayRef {
        let text = |value: &str| {
            std::sync::Arc::new(StringArray::from(vec![value.to_string()])) as ArrayRef
        };
        let number = |value: Option<f64>| {
            std::sync::Arc::new(Float64Array::from(vec![value.unwrap_or(f64::NAN)])) as ArrayRef
        };
        let fields = vec![
            Field::new("kind", DataType::Utf8, true),
            Field::new("throttle", DataType::Float64, true),
            Field::new("steering", DataType::Float64, true),
            Field::new("port", DataType::Float64, true),
            Field::new("starboard", DataType::Float64, true),
        ];
        std::sync::Arc::new(StructArray::new(
            fields.into(),
            vec![
                text(command.kind.as_str()),
                number(command.throttle),
                number(command.steering),
                number(command.port),
                number(command.starboard),
            ],
            None,
        ))
    }

    fn token_row(token: &str) -> ArrayRef {
        std::sync::Arc::new(StructArray::new(
            vec![Field::new("token", DataType::Utf8, true)].into(),
            vec![std::sync::Arc::new(StringArray::from(vec![token.to_string()])) as ArrayRef],
            None,
        ))
    }

    #[test]
    fn parses_throttle_mode() {
        let command = Command {
            kind: Kind::Control,
            throttle: Some(0.25),
            steering: Some(0.0),
            port: None,
            starboard: None,
        };
        let parsed = parse(&command_row(&command)).unwrap();
        assert_eq!(parsed, command);
    }

    #[test]
    fn nan_means_not_provided() {
        let command = Command {
            kind: Kind::Control,
            throttle: None,
            steering: None,
            port: Some(0.4),
            starboard: Some(-0.4),
        };
        let parsed = parse(&command_row(&command)).unwrap();
        assert_eq!(parsed.throttle, None);
        assert_eq!(parsed.port, Some(0.4));
    }

    #[test]
    fn rejects_illegal_combinations() {
        let mixed = Command {
            kind: Kind::Control,
            throttle: Some(0.2),
            steering: None,
            port: Some(0.2),
            starboard: Some(0.2),
        };
        assert!(parse(&command_row(&mixed)).unwrap_err().contains("不能同时给"));

        let half = Command {
            kind: Kind::Control,
            throttle: None,
            steering: None,
            port: Some(0.4),
            starboard: None,
        };
        assert!(parse(&command_row(&half)).unwrap_err().contains("必须成对"));

        let empty = Command {
            kind: Kind::Control,
            throttle: None,
            steering: None,
            port: None,
            starboard: None,
        };
        assert!(parse(&command_row(&empty)).unwrap_err().contains("没有推进量"));

        let out_of_range = Command {
            kind: Kind::Control,
            throttle: Some(1.5),
            steering: None,
            port: None,
            starboard: None,
        };
        assert!(parse(&command_row(&out_of_range)).unwrap_err().contains("超出"));
    }

    #[test]
    fn estop_needs_no_propulsion() {
        let command = Command {
            kind: Kind::Estop,
            throttle: None,
            steering: None,
            port: None,
            starboard: None,
        };
        assert_eq!(parse(&command_row(&command)).unwrap().kind, Kind::Estop);
    }

    #[test]
    fn unknown_kind_is_rejected() {
        let command = Command {
            kind: Kind::Control,
            throttle: Some(0.1),
            steering: None,
            port: None,
            starboard: None,
        };
        let row = command_row(&command);
        let rows = row.as_any().downcast_ref::<StructArray>().unwrap();
        let fields = rows.fields().clone();
        let columns: Vec<ArrayRef> = vec![
            std::sync::Arc::new(StringArray::from(vec!["hover".to_string()])),
            rows.column(1).clone(),
            rows.column(2).clone(),
            rows.column(3).clone(),
            rows.column(4).clone(),
        ];
        let row: ArrayRef = std::sync::Arc::new(StructArray::new(fields, columns, None));
        assert!(parse(&row).unwrap_err().contains("kind 只能是"));
    }

    #[test]
    fn parses_token_and_trims() {
        assert_eq!(parse_token(&token_row("  tok-abc  ")).unwrap(), "tok-abc");
        // 空串是合法输入：表示清除令牌
        assert_eq!(parse_token(&token_row("")).unwrap(), "");
        assert!(parse_token(&token_row("x")).is_ok());
    }

    /// 网页成功时回的是 `applied`：以前只认 `accepted`，导致每条指令都被当失败。
    #[test]
    fn applied_counts_as_success() {
        let value = serde_json::json!({"status": "applied", "simTime": 358.03});
        assert!(!is_rejected(&value));
        assert_eq!(applied(Kind::Control, &value), "control 已应用");

        let accepted = serde_json::json!({"status": "accepted", "reason": "ok"});
        assert!(!is_rejected(&accepted));
        assert_eq!(applied(Kind::Estop, &accepted), "ok");
    }

    #[test]
    fn rejected_and_malformed_are_failures() {
        let refused_json = serde_json::json!({"status": "rejected", "reason": "lease expired"});
        assert!(is_rejected(&refused_json));
        assert_eq!(refused(Kind::Control, &refused_json), "lease expired");

        // 畸形回执（没有 status）不能当成"已应用"
        assert!(is_rejected(&serde_json::json!({"requestId": "x"})));
        assert!(refused(Kind::Estop, &serde_json::json!({"status": "rejected"})).contains("未给原因"));
    }

    /// 不变式：控制包的有效期必须盖住「上游静默到主动交还」这段窗口，
    /// 否则网页会先按 `validUntil` 作废租约，下一条指令被判无效。
    #[test]
    fn hold_window_outlives_idle_release() {
        assert!(
            HOLD_SECONDS > IDLE_RELEASE.as_secs_f64(),
            "HOLD_SECONDS={HOLD_SECONDS} 必须大于 IDLE_RELEASE"
        );
    }

    #[test]
    fn prefix_rules() {
        assert!(validate_prefix("usv/v1/inst/scene/boat/run1").is_ok());
        assert!(validate_prefix("usv/v1/inst/scene/boat").is_err());
        assert!(validate_prefix("usv/v2/inst/scene/boat/run1").is_err());
        assert!(validate_prefix("usv/v1/in st/scene/boat/run1").is_err());
    }

    #[test]
    fn fingerprint_hides_token() {
        let text = fingerprint("abcd1234efgh");
        assert!(text.starts_with("abcd"));
        assert!(!text.contains("1234efgh"));
        assert_eq!(fingerprint(""), "未配置");
    }
}
