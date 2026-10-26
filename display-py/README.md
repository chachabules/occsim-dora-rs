# display-py

OccSim 传感器显示程序：与 `occsim-bridge` 处在同一条 dora 数据流里，接收桥接节点发布的
Arrow 消息，用 PySide6 实时展示图像、点云、导航数值与仿真状态。

```
网页(UE) -> zenohd -> occsim-bridge -> dora -> occsim-display（本程序）
```

## 界面

| 页签 | 内容 |
| --- | --- |
| 图像 | 按传感器自动分格（1 路占满整页，2–4 路 2 列，更多 3 列），每格等比缩放，左上角浮层给出传感器、编码、分辨率与帧率；雷达回波按强度上伪彩（深蓝 → 青 → 绿 → 黄 → 红），并叠加量程环 + 每 30° 方位刻度 + 天线方位线，浮层补上量程、转速与时效（相对仿真时刻） |
| 点云 | 三维透视视图（800×800，默认量程 80 m，默认视角 200°/22°）：深度缓冲遮挡、远近雾化，地面叠 10/20/40/80 m 距离环与十字轴线；右侧为总点数、有效点、量程、着色区间、帧率与视角 |
| 导航 | GPS / IMU / 组合导航 / 里程计数值卡片，航向自动转成度，缺失值显示「—」 |
| 状态 | 运行/暂停指示、仿真时刻、控制模式、任务、健康度（发送/丢弃/丢弃率）与本机接收统计 |
| 操控 | 油门+舵 / 左右桨差动两套推杆，停车、急停、交还控制权按钮；右侧显示桥接回传的状态（已受理 / 被拒绝）、拒绝原因、发送计数与距上次受理的秒数；10 Hz 持续下发，急停或释放后停止下发，需「重新接管」才恢复 |

## 运行

本程序**不单独启动**，由 dora 数据流拉起。在桥接项目里生成带显示节点的数据流：

```powershell
cd d:\code\ue\occsim-python\occsim-dora-rs

# --display 会把 occsim-display 节点写进同一份 dataflow.yml
cargo run --release -- gen-dataflow --display display-py/main.py
dora validate dataflow.yml
dora run dataflow.yml
```

显示节点默认使用 `D:/app/anaconda3/envs/occsim/python.exe`，换环境时加 `--python <解释器>`。
生成后 `dataflow.yml` 里会出现：

```yaml
  - id: occsim-display
    path: "D:/app/anaconda3/envs/occsim/python.exe"
    args: "-u display-py/main.py"
    inputs:
      rgb-bow__image: occsim-bridge/rgb-bow__image
      lidar-mast__pointcloud: occsim-bridge/lidar-mast__pointcloud
      # …其余端口同理
```

输入名直接沿用桥接端口名，因此 `ports.yaml` 改了之后重跑一次 `gen-dataflow` 即可同步。

## 交互

- 点云页：左键拖拽旋转视角（方位/俯仰 ±85°），滚轮推拉远近，Shift+滚轮改量程（5–400 m），右键拖拽平移，双击复位；右侧「着色」可切换按高度/按强度。
- 操控页：顶部先填令牌并「下发令牌」，之后推杆即发，100 ms 一包（网页侧每包只活 500 ms）；松手不会自动停船，要停请点「停车」。
  急停/交还控制权发一次即时生效，之后停止下发，按钮变「重新接管」。
- 页签切换即时生效，不重建控件；窗口缩放时图像与点云等比适配。
- 关闭窗口时后台线程会在 2 秒内退出，dora 数据流随之结束。

### 控制回路

```
操控页 --cmd: ControlCmd 10 Hz--> occsim-bridge --Zenoh cmd/control--> 网页
       --cmd_token: token------->                --Zenoh cmd/lease---->
       <--cmd_ack: ControlAck----                <--Zenoh ack----------
```

- 令牌在这一页配：顶部「接入令牌」填网页 >「接入程序」>「允许外部控制」给的临时令牌，点「下发令牌」，
  它经 `cmd_token` 端口交给桥接、只存内存（不写进 `dataflow.yml`）。没下发时面板显示「控制未启用：先在操控页填入临时令牌」，观测照常。
- 网页复位或换运行前缀后重填一次即可；点「清除」会下发空串，桥接随即交还控制权并断开。
- 回执是**异步**的：界面显示「已受理」才代表网页真的执行了；只看到发送计数在涨不算成功。
- 桥接在令牌无效、仿真暂停、state 超过 0.7 s 未更新、被别的客户端占着控制权、或等 ACK 超时（0.8 s）时回 `rejected`，
  原因原样显示在面板上。

## 环境

```powershell
cd d:\code\ue\occsim-python\occsim-dora-rs\display-py
D:\app\anaconda3\envs\occsim\python.exe -m pip install -r requirements.txt
```

依赖：`dora-rs 1.1.0`、`PySide6`、`numpy`、`pyarrow`。

不需要 dora 数据流也能自检解码、渲染与窗口构建：

```powershell
D:\app\anaconda3\envs\occsim\python.exe display-py\tests\smoke_test.py
# image ok / cloud ok / sample ok / radar ok / window ok / control ok
```

## 日志

日志落在数据流目录的 `out/<dataflow-id>/log_occsim-display.jsonl`，每 5 秒一行：

```text
首帧 rgb-bow__image（kind=image，sensor=rgb-bow）
运行中：接收 3323 帧，渲染 1043 帧，无效 0 帧，端口 8 路
```

- **接收**：dora 送进来的事件数；**渲染**：真正画到界面上的帧数，比接收少说明界面按自己的节拍跳过了旧帧（正常）。
- **无效**：解码失败或端口不属于八类观测，前 5 条会带原因。
- 完全没有「首帧」行 = 一条数据都没收到，先查桥接侧与网页推流。

## 排错

| 现象 | 处理 |
| --- | --- |
| 顶部横幅提示「dora 节点初始化失败」 | 直接运行 `python main.py` 会如此；必须由 `dora run dataflow.yml` 启动 |
| 窗口开着但一直是「等待…」 | 看 `out/<id>/log_occsim-display.jsonl`：没有「首帧」说明数据没进来（查桥接统计与网页推流）；有「首帧」但接收不涨，重启数据流 |
| 某个页签一直显示「等待…」 | `ports.yaml` 里没有该 kind 的传感器，或网页未推流；补清单后重跑 `gen-dataflow` |
| 点云几乎全黑 | 有效回波少或量程过小，滚轮放大；也可切到「按强度」看回波强度 |
| 数值全是「—」 | 对应字段缺失或为 NaN（桥接对缺失数值填 NaN，界面统一显示横杠） |
| 中文字体异常 / 无字体警告 | 无头模式下 Qt 会提示找不到字体目录，正常桌面运行不受影响 |
| 操控页一直是「控制未启用」 | 操控页顶部的令牌没填/没点「下发令牌」，或数据流是旧的（`dataflow.yml` 里缺 `cmd_token` 输入，重跑 `gen-dataflow` 并重启） |
| 操控页「被拒绝：等浏览器 ACK 超时」 | 网页没连上 / 前缀不匹配（多实例时用 `--run-prefix`）/ 令牌过期，重新在网页「接入程序」配对 |
| 推杆在动但艇不动 | 看回执而不是发送计数；「被拒绝：仿真已暂停」要先点网页的「开始仿真」 |
