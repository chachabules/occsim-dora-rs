"""后台线程：只管从 dora 取事件、保留每路最新一帧，解码与绘制全部交给 UI 线程。

dora 的 Python 侧是有界队列，回调里做重活（8 MB 图像取值、1920×1080 平滑缩放）
会把队列顶满，daemon 随后直接丢弃后续事件（日志里的
`Discarding event for input ... due to queue size limit`），界面表现就是长时间不刷新。
所以这里 `next()` 只把最新一帧放进缓冲、覆盖旧帧，UI 按自己的节奏取用：
慢的时候自然丢帧，而不是越积越多、越显示越滞后。
"""

from __future__ import annotations

import queue
import re
import threading
import time
from collections import deque

from PySide6.QtCore import QThread, Signal

from .control import CMD_PORT, TOKEN_PORT
from .decode import KINDS, kind_of, metadata_text

#: 需要展示成数值表的 kind。
SAMPLE_KINDS = KINDS[2:]

#: 持续控制的发送节拍：网页单包只活 500 ms，10 Hz 留 400 ms 余量。
#: 放在本线程而不是 UI 定时器里，界面重绘/点击卡顿才不会把控制流打断。
CONTROL_INTERVAL = 0.1

#: dora 1.1 用 ERROR 事件表示 `next(timeout=...)` 超时，命中即视为「本轮空转」。
TIMEOUT_MARKERS = re.compile(r"timed out|timeout", re.IGNORECASE)


class NodeWorker(QThread):
    """dora 事件循环。所有 Qt 控件都只在主线程被触碰。"""

    stats = Signal(object)  # dict
    attached = Signal(str)  # dataflow id
    notice = Signal(str)  # 正常结束/提示
    failed = Signal(str)  # 初始化失败或异常

    def __init__(self, parent=None) -> None:
        super().__init__(parent)
        self._running = True
        self._lock = threading.Lock()
        self._latest: dict[str, tuple[str, object, dict]] = {}
        self._dirty: set[str] = set()
        self._received = 0
        self._rendered = 0
        self._invalid = 0
        self._ports: set[str] = set()
        self._logged: set[str] = set()
        self._stamps: deque[float] = deque(maxlen=600)
        self._started_at = time.monotonic()
        # 控制指令：UI 线程只写"当前目标"，由本线程按固定节拍持续下发。
        # 一次性包（令牌/急停/交还）仍走队列。
        self._control_lock = threading.Lock()
        self._control: tuple[str, object] | None = None
        self._control_next = 0.0
        self._outbox: queue.SimpleQueue = queue.SimpleQueue()
        self._sent = 0
        self._send_errors = 0

    # -- UI 线程调用 ---------------------------------------------------
    def stop(self) -> None:
        self._running = False

    def take(self) -> list[tuple[str, str, object, dict]]:
        """取走有更新的端口及其最新一帧（端口, 传感器, Arrow 数据, metadata）。

        同一端口的多帧只保留最后一帧；metadata 一并带出，图像面板要用其中的
        雷达极坐标参数。
        """
        with self._lock:
            ports = sorted(self._dirty)
            self._dirty.clear()
            frames = [(port, *self._latest[port]) for port in ports if port in self._latest]
        return frames

    def note_rendered(self) -> None:
        self._rendered += 1

    def note_invalid(self, count: int = 1) -> None:
        self._invalid += count

    def set_control(self, array) -> None:
        """UI 线程调用：更新"当前目标"，由工作线程按 10 Hz 持续下发。

        这里刻意不做"入队一包"：界面卡一下（点控件、重绘大图）就会让定时器
        错过几十上百毫秒，直接入队等于控制流跟着界面一起抖，而网页 350 ms
        收不到指令就把推进清零。
        """
        with self._control_lock:
            self._control = (CMD_PORT, array)

    def stop_control(self) -> None:
        """UI 线程调用：停止持续下发（急停、交还控制权、清除令牌之后）。"""
        with self._control_lock:
            self._control = None

    def send_command(self, array) -> None:
        """UI 线程调用：一次性指令（急停/交还），只入队。"""
        self._outbox.put((CMD_PORT, array))

    def send_token(self, array) -> None:
        """UI 线程调用：下发临时令牌（`cmd_token`），同样只入队。"""
        self._outbox.put((TOKEN_PORT, array))

    @property
    def sent_commands(self) -> int:
        return self._sent

    # -- 统计 ----------------------------------------------------------
    def _line(self, prefix: str) -> str:
        return (
            f"{prefix}：接收 {self._received} 帧，渲染 {self._rendered} 帧，"
            f"无效 {self._invalid} 帧，端口 {len(self._ports)} 路"
        )

    def _report(self) -> None:
        now = time.monotonic()
        recent = sum(1 for stamp in self._stamps if now - stamp <= 1.0)
        self.stats.emit(
            {
                "received": self._received,
                "rendered": self._rendered,
                "invalid": self._invalid,
                "fps": float(recent),
                "ports": len(self._ports),
                "elapsed": now - self._started_at,
            }
        )

    def _send(self, node, port: str, payload) -> None:
        try:
            node.send_output(port, payload)
            if port == CMD_PORT:
                self._sent += 1
        except Exception as error:  # noqa: BLE001 - 端口未声明、编码不符等
            self._send_errors += 1
            if self._send_errors <= 3:
                log = getattr(node, "log_error", None) or node.log_info
                log(f"{port} 发送失败：{error}")

    def _drain_outbox(self, node) -> bool:
        """发一次性包，并按节拍持续下发当前控制目标；返回是否还有待办。"""
        while True:
            try:
                port, payload = self._outbox.get_nowait()
            except queue.Empty:
                break
            self._send(node, port, payload)

        with self._control_lock:
            control = self._control
        if control is None:
            return False
        now = time.monotonic()
        if now >= self._control_next:
            self._control_next = now + CONTROL_INTERVAL
            self._send(node, *control)
        return True

    # -- 线程入口 ------------------------------------------------------
    def run(self) -> None:  # noqa: D401 - QThread 入口
        try:
            from dora import Node
        except ImportError as error:  # pragma: no cover - 环境问题
            self.failed.emit(f"导入 dora 失败：{error}")
            return

        try:
            node = Node()
        except Exception as error:  # noqa: BLE001 - 需要把原因原样展示
            self.failed.emit(
                "dora 节点初始化失败："
                f"{error}\n本程序必须由 dora 数据流启动（dora run dataflow.yml）。"
            )
            return

        self.attached.emit(str(getattr(node, "dataflow_id", "?")))
        stop_reason = "线程已停止"
        last_report = last_log = time.monotonic()
        while self._running:
            pending = self._drain_outbox(node)
            # 有待发的指令就把轮询缩短：控制包只活 500 ms，别让它压在队列里。
            event = node.next(timeout=0.05 if pending else 0.1)
            if not self._running:
                break
            stamp = time.monotonic()
            if event is None:
                if stamp - last_report > 0.25:
                    last_report = stamp
                    self._report()
                continue

            event_type = event.get("type")
            if event_type == "STOP":
                stop_reason = "收到 STOP"
                break
            if event_type == "ERROR":
                detail = str(event.get("error", "未知错误"))
                if TIMEOUT_MARKERS.search(detail):
                    # dora 1.1 把「本轮没有数据」也报成 ERROR 事件，不能当致命错误，
                    # 否则启动时没有数据就会让接收线程立刻退出，之后再也收不到。
                    if stamp - last_report > 0.25:
                        last_report = stamp
                        self._report()
                    continue
                self.failed.emit(f"dora 事件错误：{detail}")
                return
            if event_type != "INPUT":
                continue

            port = str(event.get("id", ""))
            metadata = event.get("metadata") or {}
            sensor = metadata_text(metadata, "sensor") or port.split("__")[0]
            with self._lock:
                self._latest[port] = (sensor, event.get("value"), metadata)
                self._dirty.add(port)

            self._received += 1
            self._ports.add(port)
            self._stamps.append(stamp)
            if port not in self._logged:
                # 每个端口只在首帧打一次：确认「收到了」以及「收到的是谁」。
                self._logged.add(port)
                node.log_info(f"首帧 {port}（kind={kind_of(port)}，sensor={sensor}）")

            if stamp - last_report > 0.25:
                last_report = stamp
                self._report()
            if stamp - last_log > 5.0:
                last_log = stamp
                node.log_info(self._line("运行中"))

        node.log_info(self._line(stop_reason))
        with self._lock:
            self._latest.clear()
            self._dirty.clear()
        self._report()
        self.notice.emit(f"{stop_reason}，数据流已停止")
