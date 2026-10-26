"""主窗口：顶部信息条 + 四个页签 + 底部状态栏。"""

from __future__ import annotations

import logging

from PySide6.QtCore import QTimer
from PySide6.QtWidgets import (
    QFrame,
    QHBoxLayout,
    QLabel,
    QMainWindow,
    QTabWidget,
    QVBoxLayout,
    QWidget,
)

from . import control
from .decode import (
    DecodeError,
    decode_image,
    decode_pointcloud,
    decode_sample,
    kind_of,
)
from .node_worker import SAMPLE_KINDS, NodeWorker
from .panels import CloudPanel, ControlPanel, ImagePanel, NavPanel, StatusPanel
from .theme import COLORS, STYLE_SHEET

#: UI 取帧节拍：比数据源慢时自动丢中间帧，只画最新一帧。
FRAME_INTERVAL_MS = 66


class MainWindow(QMainWindow):
    def __init__(self) -> None:
        super().__init__()
        self.setWindowTitle("OccSim 传感器显示")
        self.resize(1440, 900)
        self.setStyleSheet(STYLE_SHEET)

        # 工作线程先建好：操控页要往里塞指令（此时还没 start）。
        self.worker = NodeWorker(self)

        self.image_panel = ImagePanel()
        self.cloud_panel = CloudPanel()
        self.nav_panel = NavPanel()
        self.status_panel = StatusPanel()
        self.control_panel = ControlPanel(self.worker)

        central = QWidget()
        root = QVBoxLayout(central)
        root.setContentsMargins(0, 0, 0, 0)
        root.setSpacing(0)
        root.addWidget(self._build_header())

        self.banner = QLabel()
        self.banner.setWordWrap(True)
        self.banner.setVisible(False)
        self.banner.setStyleSheet(
            f"background: {COLORS['surface_alt']};"
            f" color: {COLORS['warn']};"
            f" border-bottom: 1px solid {COLORS['border']};"
            " padding: 8px 16px;"
        )
        root.addWidget(self.banner)

        self.tabs = QTabWidget()
        self.tabs.setDocumentMode(True)
        self.tabs.addTab(self.image_panel, "图像")
        self.tabs.addTab(self.cloud_panel, "点云")
        self.tabs.addTab(self.nav_panel, "导航")
        self.tabs.addTab(self.status_panel, "状态")
        self.tabs.addTab(self.control_panel, "操控")
        root.addWidget(self.tabs, 1)
        self.setCentralWidget(central)

        self.statusBar().showMessage("等待数据流…")

        self._error = False
        self._decode_warns = 0
        self._sim_time: float | None = None
        self.worker.stats.connect(self._on_stats)
        self.worker.attached.connect(self._on_started)
        self.worker.notice.connect(self._on_notice)
        self.worker.failed.connect(self._on_failed)

        # 取帧定时器跑在主线程：解码与绘制都在这里做，慢就跳帧，不会堆积。
        self.timer = QTimer(self)
        self.timer.setInterval(FRAME_INTERVAL_MS)
        self.timer.timeout.connect(self._drain)
        self.timer.start()

        QTimer.singleShot(0, self.worker.start)

    # -- 构建 ---------------------------------------------------------
    def _build_header(self) -> QWidget:
        header = QFrame()
        header.setObjectName("header")
        header.setFixedHeight(58)
        layout = QHBoxLayout(header)
        layout.setContentsMargins(16, 8, 16, 8)
        layout.setSpacing(14)

        title = QLabel("OccSim 传感器显示")
        title.setObjectName("product")
        layout.addWidget(title)

        self.subtitle = QLabel("未连接数据流")
        self.subtitle.setObjectName("mono")
        layout.addWidget(self.subtitle)
        layout.addStretch(1)

        self.header_ports = QLabel("端口 0")
        self.header_ports.setObjectName("mono")
        layout.addWidget(self.header_ports)

        self.header_fps = QLabel("0.0 Hz")
        self.header_fps.setObjectName("value")
        layout.addWidget(self.header_fps)

        self.header_dot = QLabel("●")
        self.header_dot.setObjectName("dot")
        self._set_dot("muted")
        layout.addWidget(self.header_dot)
        return header

    # -- 槽 -----------------------------------------------------------
    def _set_dot(self, tone: str) -> None:
        self.header_dot.setStyleSheet(f"color: {COLORS.get(tone, COLORS['muted'])}; font-size: 15px;")

    def _on_started(self, dataflow_id: str) -> None:
        short = dataflow_id[:8] if dataflow_id and dataflow_id != "?" else "未知"
        self.subtitle.setText(f"数据流 {short}")

    def _drain(self) -> None:
        """按固定节拍取走各端口的最新一帧，解码后分派给对应面板。"""
        for port, sensor, value, metadata in self.worker.take():
            # 回执端口不在八类观测里，先单独截下来，别掉进"未知端口"分支。
            if port == control.ACK_PORT:
                self.control_panel.show_ack(control.decode_ack(value))
                self.worker.note_rendered()
                continue
            kind = kind_of(port)
            try:
                if kind == "image":
                    self.image_panel.show_frame(decode_image(value, metadata), sensor)
                elif kind == "pointcloud":
                    self.cloud_panel.update_cloud(decode_pointcloud(value), sensor)
                elif kind in SAMPLE_KINDS:
                    self._on_sample(kind, sensor, decode_sample(value))
                else:
                    self.worker.note_invalid()
                    self._warn(f"未知端口 {port}：{kind} 不属于八类观测")
                    continue
            except (DecodeError, ValueError, TypeError) as error:
                self.worker.note_invalid()
                self._warn(f"解码失败 {port}：{error}")
                continue
            self.worker.note_rendered()
        # 时效按仿真时钟算，没收到新帧时也要跟着走。
        self.image_panel.tick(self._sim_time)

    def _warn(self, message: str) -> None:
        """同一类解码问题最多刷 5 条，避免 10 Hz 刷屏。"""
        self._decode_warns += 1
        if self._decode_warns <= 5:
            logging.warning(message)

    def _on_sample(self, kind: str, sensor: str, fields: dict) -> None:
        if kind in ("state", "health"):
            self.status_panel.update_sample(kind, fields)
            if kind == "state":
                # 图像浮层用同一个仿真时钟算时效，避免与墙钟混用。
                sim_time = fields.get("simTime")
                self._sim_time = float(sim_time) if isinstance(sim_time, (int, float)) else None
        else:
            self.nav_panel.update_sample(kind, sensor, fields)

    def _on_stats(self, stats: dict) -> None:
        self.status_panel.update_stats(stats)
        self.header_ports.setText(f"端口 {int(stats.get('ports', 0))}")
        self.header_fps.setText(f"{float(stats.get('fps', 0.0)):.1f} Hz")
        self.statusBar().showMessage(
            f"接收 {int(stats.get('received', 0)):,} 帧 · "
            f"渲染 {int(stats.get('rendered', 0)):,} 帧 · "
            f"无效 {int(stats.get('invalid', 0))} · "
            f"端口 {int(stats.get('ports', 0))} · "
            f"{float(stats.get('elapsed', 0.0)):.0f} s"
        )
        if not self._error and int(stats.get("received", 0)) > 0:
            self._set_dot("ok")

    def _on_notice(self, message: str) -> None:
        self.statusBar().showMessage(message)
        self._set_dot("muted")
        # 数据流已经结束，节点留着不退出会被 daemon 在宽限期后强杀，直接关窗。
        self.close()

    def _on_failed(self, message: str) -> None:
        self._error = True
        self.banner.setText(message)
        self.banner.setVisible(True)
        self._set_dot("error")
        self.statusBar().showMessage("未连接到数据流")

    # -- 退出 ---------------------------------------------------------
    def closeEvent(self, event) -> None:  # noqa: N802 - Qt 命名
        self.timer.stop()
        self.worker.stop()
        if not self.worker.wait(2000):
            self.worker.terminate()
        super().closeEvent(event)
