"""点云页：三维透视视图 + 轨道旋转 / 推拉 / 平移 + 高度或强度着色。"""

from __future__ import annotations

import time
from collections import deque

import numpy as np
from PySide6.QtCore import QPointF, QRectF, Qt, Signal
from PySide6.QtGui import QColor, QPainter, QPixmap
from PySide6.QtWidgets import (
    QComboBox,
    QHBoxLayout,
    QLabel,
    QVBoxLayout,
    QWidget,
)

from ..decode import CloudFrame
from ..render import DEFAULT_PITCH, DEFAULT_RANGE, DEFAULT_YAW, cloud_qimage, render_cloud
from ..theme import COLORS
from ..widgets import Card, fmt_number


class CloudView(QWidget):
    """只负责画位图与视角状态，不碰业务数据。

    左键拖拽旋转、滚轮推拉、Shift+滚轮改量程、右键拖拽平移、双击复位。
    """

    changed = Signal()
    range_changed = Signal(float)

    def __init__(self) -> None:
        super().__init__()
        self._pixmap: QPixmap | None = None
        self._origin: QPointF | None = None
        self._mode = "orbit"
        self._pan = QPointF(0.0, 0.0)
        self._yaw = DEFAULT_YAW
        self._pitch = DEFAULT_PITCH
        self._zoom = 1.0
        self._range = DEFAULT_RANGE
        self.setMinimumSize(360, 360)
        self.setCursor(Qt.CursorShape.OpenHandCursor)

    # -- 视角状态（面板渲染时读取） -----------------------------------
    @property
    def yaw(self) -> float:
        return self._yaw

    @property
    def pitch(self) -> float:
        return self._pitch

    @property
    def zoom(self) -> float:
        return self._zoom

    def set_pixmap(self, pixmap: QPixmap) -> None:
        self._pixmap = pixmap
        self.update()

    def reset_view(self) -> None:
        self._yaw, self._pitch, self._zoom = DEFAULT_YAW, DEFAULT_PITCH, 1.0
        self._pan = QPointF(0.0, 0.0)
        self.changed.emit()

    def paintEvent(self, event) -> None:  # noqa: N802
        painter = QPainter(self)
        painter.fillRect(self.rect(), QColor(COLORS["canvas"]))
        if self._pixmap is None:
            painter.setPen(QColor(COLORS["muted"]))
            painter.drawText(self.rect(), Qt.AlignmentFlag.AlignCenter, "等待点云数据…")
            return
        painter.setRenderHint(QPainter.RenderHint.SmoothPixmapTransform)
        area = QRectF(self.rect()).adjusted(1, 1, -1, -1)
        scaled = self._pixmap.size()
        scaled.scale(area.size().toSize(), Qt.AspectRatioMode.KeepAspectRatio)
        target = QRectF(0, 0, scaled.width(), scaled.height())
        target.moveCenter(area.center() + self._pan)
        painter.drawPixmap(target, self._pixmap, QRectF(self._pixmap.rect()))

    def wheelEvent(self, event) -> None:  # noqa: N802
        delta = event.angleDelta().y()
        if delta == 0:
            return
        step = 0.85 if delta > 0 else 1 / 0.85
        if event.modifiers() & Qt.KeyboardModifier.ShiftModifier:
            self._range = min(400.0, max(5.0, self._range * step))
            self.range_changed.emit(self._range)
        else:
            self._zoom = min(3.0, max(0.35, self._zoom / step))
            self.changed.emit()

    def mousePressEvent(self, event) -> None:  # noqa: N802
        self._origin = event.position()
        if event.button() == Qt.MouseButton.LeftButton:
            self._mode = "orbit"
            self.setCursor(Qt.CursorShape.ClosedHandCursor)

    def mouseMoveEvent(self, event) -> None:  # noqa: N802
        if self._origin is None:
            return
        delta = event.position() - self._origin
        self._origin = event.position()
        if self._mode == "orbit":
            self._yaw = (self._yaw - delta.x() * 0.4) % 360.0
            self._pitch = min(85.0, max(-85.0, self._pitch + delta.y() * 0.3))
        else:
            self._pan += delta
        self.changed.emit()

    def mouseReleaseEvent(self, event) -> None:  # noqa: N802
        self._origin = None
        self.setCursor(Qt.CursorShape.OpenHandCursor)

    def mouseDoubleClickEvent(self, event) -> None:  # noqa: N802
        self.reset_view()


class CloudPanel(QWidget):
    def __init__(self) -> None:
        super().__init__()
        layout = QHBoxLayout(self)
        layout.setContentsMargins(12, 12, 12, 12)
        layout.setSpacing(12)

        self.view = CloudView()
        self.view.changed.connect(self._render)
        self.view.range_changed.connect(self._on_range)
        layout.addWidget(self.view, 1)

        side = QVBoxLayout()
        side.setSpacing(10)
        self.card = Card("点云统计", "点云")
        self.total = self.card.add_row(0, "总点数")
        self.valid = self.card.add_row(1, "有效点")
        self.range_label = self.card.add_row(2, "量程", "m")
        self.level = self.card.add_row(3, "着色区间")

        mode_key = QLabel("着色")
        mode_key.setObjectName("cardKey")
        self.mode = QComboBox()
        self.mode.addItems(["按高度", "按强度"])
        self.mode.currentIndexChanged.connect(lambda _: self._render())
        self.card.body.addWidget(mode_key, 4, 0)
        self.card.body.addWidget(self.mode, 4, 1, 1, 2)

        self.fps_label = self.card.add_row(5, "帧率", "Hz")
        self.view_label = self.card.add_row(6, "视角")
        side.addWidget(self.card)
        side.addStretch(1)
        holder = QWidget()
        holder.setLayout(side)
        holder.setFixedWidth(230)
        layout.addWidget(holder)

        self._points: np.ndarray | None = None
        self._range = DEFAULT_RANGE
        self._stamps: deque[float] = deque(maxlen=60)

    # -- 数据入口 -----------------------------------------------------
    def update_cloud(self, frame: CloudFrame, sensor: str = "") -> None:
        self._points = frame.points
        self.card.badge.setText(sensor or "点云")
        self._stamps.append(time.monotonic())
        self._render()

    # -- 内部 ---------------------------------------------------------
    def _on_range(self, value: float) -> None:
        self._range = value
        self.range_label.setText(fmt_number(value, 0))
        self._render()

    def _fps(self) -> float:
        now = time.monotonic()
        return float(sum(1 for stamp in self._stamps if now - stamp <= 1.0))

    def _render(self) -> None:
        if self._points is None:
            return
        color_by = "height" if self.mode.currentIndex() == 0 else "intensity"
        result = render_cloud(
            self._points,
            self._range,
            color_by=color_by,
            yaw=self.view.yaw,
            pitch=self.view.pitch,
            zoom=self.view.zoom,
        )
        self.view.set_pixmap(QPixmap.fromImage(cloud_qimage(result)))
        self.total.setText(f"{result.total:,}")
        self.valid.setText(f"{result.valid:,}")
        self.range_label.setText(fmt_number(self._range, 0))
        unit = "m" if color_by == "height" else ""
        self.level.setText(f"{result.low:.1f} ~ {result.high:.1f} {unit}".strip())
        self.view_label.setText(f"{result.yaw:.0f}° / {result.pitch:.0f}°")
        self.fps_label.setText(f"{self._fps():.1f}")
