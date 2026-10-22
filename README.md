# occsim-dora-rs

把 OccSim 无人艇仿真器的传感器数据接入 [dora-rs](https://github.com/dora-rs/dora) 数据流。

![rust](https://img.shields.io/badge/rust-1.85%2B-orange?logo=rust&logoColor=white)
![edition](https://img.shields.io/badge/edition-2024-blue)
![dora](https://img.shields.io/badge/dora-1.1-blueviolet)
![zenoh](https://img.shields.io/badge/zenoh-1.10-green)
![license](https://img.shields.io/badge/license-MIT-blue)

```text
OccSim 网页(UE) --WS 10000--> zenohd(remote-api) --tcp 7447--> occsim-dora-rs --Arrow--> dora --> 下游节点
```

一个 Rust 进程：订阅 zenoh 主题 `usv/v1/**`，按协议解码图像、点云与 JSON，编码为 Arrow 后按端口发布到 dora。浏览器接入用的 10000 端口由 `zenoh-plugin-remote-api` 提供，节点启动时自行拉起或复用 `zenohd`，整条链路不依赖 `sensors.py`，也没有 Tk 窗口。

## 目录

- [特性](#特性)
- [环境要求](#环境要求)
- [安装](#安装)
- [快速开始](#快速开始)
- [端口配置](#端口配置)
- [数据格式](#数据格式)
- [命令行参数](#命令行参数)
- [观察数据](#观察数据)
- [开发与测试](#开发与测试)
- [排错](#排错)
- [设计说明](#设计说明)
- [许可证](#许可证)

## 特性

- **单进程桥接**：zenoh 订阅、协议解码、Arrow 编码、dora 发布都在一个节点内完成，少一跳、少一套 Python 运行时。
- **零配置路由器**：启动时探测 `7447`，已在监听就直接复用，否则拉起 `zenohd` 子进程并等待端口就绪，节点退出时自动回收。
- **端口即配置**：端口名由 `ports.yaml` 决定，`gen-dataflow` 据此生成 `dataflow.yml`，无需手写 YAML。
- **背压保护**：接收侧有界队列满时丢最旧帧，回调线程永不阻塞；丢弃与解码失败分别计数，每 5 秒打印一次统计。
- **恒定列类型**：JSON 字段缺失时用 `NaN` / 空串 / 0 / `false` 填充，下游拿到的列类型不会随帧变化。
- **可对拍**：解码逻辑与 Python 实现共用同一批负载做一致性测试，行为等价有据可查。

## 环境要求

| 依赖 | 版本 | 说明 |
| --- | --- | --- |
| Rust | ≥ 1.85（edition 2024） | 编译本节点 |
| `dora-cli` | 1.1.0 | 与 `Cargo.toml` 中的 `dora-node-api 1.1` 配套 |
| `zenohd` + remote-api 插件 | 1.10.1 | 必须同源构建，否则插件加载报 `Plugin compatibility mismatch` |
| Python | 3.10+ | 可选，仅 Python 对拍测试需要 |

> `zenohd` 与 remote-api 插件需要来自同一份官方 release，混用不同版本会在启动时直接失败。

## 安装

```bash
# 1. dora 命令行
cargo install dora-cli --version 1.1.0 --locked

# 2. 编译节点
git clone <this-repo> occsim-dora-rs
cd occsim-dora-rs
cargo build --release
```

将 zenoh standalone 包解压到任意目录，例如：

```text
C:\tools\zenoh\zenohd.exe
C:\tools\zenoh-remote-api\zenoh_plugin_remote_api.dll
```

路径与默认值不同时，用 `--zenohd` / `--plugin-dir` 指定。自检：

```powershell
dora --version                                                   # dora-cli 1.1.0
Test-Path C:\tools\zenoh\zenohd.exe                              # True
Test-Path C:\tools\zenoh-remote-api\zenoh_plugin_remote_api.dll  # True
```

路由器配置默认读取 `../integration/router.json5`，用 `--router-config` 指向自己的文件即可：

```json5
{
  mode: "router",
  listen: { endpoints: ["tcp/127.0.0.1:7447"] },
  scouting: { multicast: { enabled: false } },
  adminspace: { enabled: false },
  plugins_loading: { enabled: true },
  plugins: {
    remote_api: { __required__: true, websocket_port: "127.0.0.1:10000" },
  },
}
```

## 快速开始

```bash
# 按 ports.yaml 生成 dataflow.yml（生成物，不要手改）
cargo run --release -- gen-dataflow

# 校验接线与输出类型
dora validate dataflow.yml

# 启动数据流
dora run dataflow.yml
```

随后在 OccSim 网页中点击「接入程序」，连接 `ws://127.0.0.1:10000` 并开始仿真。终端每 5 秒打印一行统计：

```text
运行中：接收 8373 帧，发送 8373 帧，丢弃 0 帧，无效 0 帧
```

- **丢弃**：队列背压，点云突发时丢最旧帧。
- **无效**：解码失败，或来自未在 `ports.yaml` 声明的传感器。

### 两种运行方式

| 方式 | 命令 | 特点 |
| --- | --- | --- |
| 本地隔离（日常使用） | `dora run dataflow.yml` | 不启动 coordinator，`dora list` / `top` / `topic` 不可见；观察终端输出与 `out/<id>/log_occsim-bridge.jsonl` |
| 协调器模式 | `dora up` + `dora start dataflow.yml` | `dora list` / `logs` / `topic hz` / `top` / `stop` 均可用 |

```bash
dora up
dora start dataflow.yml     # 后台启动，打印 dataflow id
dora list
dora logs <dataflow-id>
dora stop <dataflow-id>
dora down
```

两种方式二选一：同时运行会订阅同一批数据、重复解码。

## 端口配置

dora 的端口必须在 `dataflow.yml` 中静态声明，未声明端口的 `send_output` 会被静默忽略。生成规则只看 `ports.yaml`：

| `ports.yaml` | 生成的端口 |
| --- | --- |
| 空 / 不存在 | 8 路类型端口：`image` `pointcloud` `gps` `imu` `ins` `odom` `state` `health` |
| 有实例 | 只生成 `{sensor}__{kind}`，不再生成类型端口 |

```yaml
instances:
  - sensor: gps-mast
    kind: gps
  - sensor: rgb-bow
    kind: image
```

- `sensor` 即主题第 8 段：`usv/v1/{实例}/{场景}/{艇}/{运行}/sensors/{sensor}/{kind}`。匹配前会规范化（转小写、非字母数字转 `-`），因此 `GPS Mast` 与 `gps-mast` 等价。
- 清单非空时，未声明的传感器整类丢弃，日志按 `(sensor, kind)` 只告警一次。新增或改名传感器后，补清单 → 重跑 `gen-dataflow` → 重启数据流。
- `state` / `health` 主题没有传感器维度，如需发布也要写进清单，端口名固定为 `state__state`、`health__health`。
- 同一传感器可以有多类数据（例如 `ins-main` 同时有 `ins` 与 `odom`），按组合各占一路。

## 数据格式

每路输出都是一行 `StructArray`。`image` 复用 dora 内置的 `std/media/v1/Image`，其余类型定义在 `types/usv/occsim/<kind>/v1.yml`，URN 为 `usv/occsim/<kind>/v1/<Type>`。

| 端口 | 类型 URN | 字段 |
| --- | --- | --- |
| image | `std/media/v1/Image` | `width: UInt32` `height: UInt32` `encoding: Utf8` `data: LargeBinary` |
| pointcloud | `usv/occsim/pointcloud/v1/PointCloud` | `width: UInt32` `height: UInt32` `encoding: Utf8` `pointCount: UInt64` `data: LargeBinary` |
| gps | `usv/occsim/gps/v1/GpsFix` | `fix: Utf8`，`latitude` `longitude` `altitude` `vx` `vy` `vz`: Float64 |
| imu | `usv/occsim/imu/v1/ImuSample` | `fx` `fy` `fz` `wx` `wy` `wz`: Float64，`quality: Utf8` |
| ins | `usv/occsim/ins/v1/InsState` | `px` `py` `pz` `vx` `vy` `vz` `heading`: Float64，`filter: Utf8` |
| odom | `usv/occsim/odom/v1/OdomState` | `px` `py` `pz` `vx` `vy` `vz` `heading` `distance`: Float64 |
| state | `usv/occsim/state/v1/SimState` | `simTime: Float64` `paused: Boolean` `controlMode: Utf8` `mission: Utf8` |
| health | `usv/occsim/health/v1/Health` | `sent: UInt64` `dropped: UInt64` `simTime: Float64` |

- 图像 `data` 为行优先扁平像素，长度 `width * height * channels`。
- 点云 `data` 为 `pointCount` 组小端 `float32` 的 x/y/z/intensity，`encoding` 为 `xyzi32f`。
- 所有帧的 metadata 均含 `source`、`key`、`kind`、`sensor`、`instance`、`run_id`、`size`；JSON 类额外带原文 `json`，图像额外带 `channels`、`encoding`。

下游读取示例：

```python
value = event["value"]                 # 单行 StructArray
value.field("latitude")[0].as_py()

import numpy as np
cloud = np.frombuffer(value.field("data")[0].as_py(), np.float32).reshape(-1, 4)  # x/y/z/i
```

`image` 使用内置 URN，`dora-yolo`、`dora-rerun` 等现成节点可直接接入。修改字段时需同步更新 `types/` 与 `src/encode.rs`，然后执行一次 `dora validate`。

## 命令行参数

```text
occsim-dora-rs [run|gen-dataflow]
  --ports P           实例清单，默认 ports.yaml
  --dataflow P        输出/运行的 YAML，默认 dataflow.yml
  --exe P             生成时写入的可执行路径
  --build CMD         生成时写入的构建命令
  --zenohd P          zenohd 路径
  --plugin-dir P      remote-api 插件目录
  --router-config P   路由器配置 json5
```

## 观察数据

```powershell
# 本地隔离模式：读取最新落盘的日志
$id = Get-ChildItem out -Directory | Sort-Object LastWriteTime -Descending |
      Select-Object -First 1 -ExpandProperty Name
Get-Content "out\$id\log_occsim-bridge.jsonl" -Encoding UTF8 -Tail 5

# 协调器模式
dora topic hz   <dataflow-id>/gps-mast__gps
dora topic echo <dataflow-id>/rgb-bow__image
```

端口名以 `ports.yaml` 为准。`RUST_LOG=debug` 可看到每一帧被丢弃的主题与原因。

## 开发与测试

```bash
cargo build --release
cargo test
```

`cargo test` 覆盖解码、路由、dataflow 生成、Arrow 编码，以及 `types/` 与 `encode.rs` 的字段契约。`tests/decode_parity.rs` 会把同一批负载（含真实点云样本）同时喂给 Rust 解码器与 Python 侧的 `parse_observation`，要求两边结论完全一致；缺少 Python 或样本时自动跳过，不会让测试变红。

## 排错

| 现象 | 处理 |
| --- | --- |
| `dora list` 是空表 | 当前用的是 `dora run`，改用 `dora up` + `dora start` |
| 一直「接收 0 帧」 | 网页未连 10000、未开始仿真，或传感器未启用 / 未打开发布 / 未应用配置 |
| `zenohd 15 秒内未监听 7447` | `7447` 或 `10000` 被占用，先关掉 `sensors.py` 或其它 zenoh 实例 |
| `dora 节点初始化失败` | 不能裸跑可执行文件，必须由 dora 启动 |
| 端口有数据但下游收不到 | 下游 `inputs` 未声明该端口，端口名以 `dataflow.yml` 为准 |
| `Plugin compatibility mismatch` | zenohd 与 remote-api 插件版本不一致，换用同源构建 |

## 设计说明

zenoh 插件以动态库加载，加载时会校验 rustc 版本、zenoh 版本与 feature 集合，把 remote-api 直接嵌进本进程极易因版本不一致被拒。因此节点改为拉起同源预编译的 `zenohd` 子进程、自己以 client 身份连 `7447`，数据面行为完全一致，且不受插件 ABI 约束。

需要与 `sensors.py` 并存时，两边复用同一个 `7447` 路由器，但不能同时占用 `10000`。需要 Tk 显示窗口时，先启动 `sensors.py`，再启动数据流。

## 许可证

本项目基于 [MIT License](LICENSE) 开源，可自由使用、修改与分发，请保留版权声明与许可声明。
