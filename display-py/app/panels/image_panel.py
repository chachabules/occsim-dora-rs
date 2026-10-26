"""图像页：按传感器分格显示，每格等比缩放 + 左上角信息浮层。

桥上可能有多路图像（相机、雷达 PPI、红外……），每一路按 `sensor` 各占一格：
1 路占满整页，2–4 路 2 列，更多则 3 列。没有数据时显示占位提示。
雷达格额外叠加量程环 + 方位刻度，浮层里补上量程、转速与时效。
"""

from __future__ import annotations

import time
from collections import deque

from PySide6.QtCore import QRectF, Qt
from PySide6.QtGui import QImage, QPainter, QPixmap
from PySide6.QtWidgets import QFrame, QGridLayout, QLabel, QVBoxLayout, QWidget

from ..decode import ImageFrame
from ..render import draw_radar_overlay, qimage_from_frame
from ..theme import COLORS, FONT_MONO
from ..widgets import fmt_number

_OVERLAY_STYLE = (
    "background: rgba(15, 20, 25, 0.82);"
    f" border: 1px solid {COLORS['border']};"
    " border-radius: 6px;"
    " padding: 5px 9px;"
    f" color: {COLORS['text']};"
    f" font-family: '{FONT_MONO}';"
    " font-size: 11px;"
)


class _Canvas(QLabel):
    """画布：QLabel 先把等比缩放后的图铺好，再在同一区域叠加雷达网格。

    网格必须跟着 QLabel 里实际显示的图走：`KeepAspectRatio` 之后图会在控件里
    居中留边，把网格画到外层控件上会随窗口比例漂移。
    """

    def __init__(self, parent: QWidget | None = None) -> None:
        super().__init__(parent)
        self._radar: tuple[float, float | None] | None = None

    def set_radar(self, spec: tuple[float, float | None] | None) -> None:
        """`spec` 为 (量程 m, 天线相对方位角°)；None 表示这一格不是雷达。"""
        self._radar = spec
        self.update()

    def paintEvent(self, event) -> None:  # noqa: N802 - Qt 命名
        super().paintEvent(event)
        if self._radar is None:
            return
        pixmap = self.pixmap()
        if pixmap is None or pixmap.isNull():
            return
        rect = QRectF(pixmap.rect())
        rect.moveCenter(QRectF(self.contentsRect()).center())
        range_m, bearing = self._radar
        painter = QPainter(self)
        draw_radar_overlay(painter, rect, range_m=range_m, bearing=bearing)
        painter.end()


class ImageTile(QFrame):
    """一路图像的一格画布。"""

    def __init__(self, sensor: str, parent: QWidget | None = None) -> None:
        super().__init__(parent)
        self.sensor = sensor
        self.setMinimumSize(280, 200)

        layout = QVBoxLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)

        self.canvas = _Canvas(self)
        self.canvas.setAlignment(Qt.AlignmentFlag.AlignCenter)
        self.canvas.setText(f"{sensor}\n等待图像数据…")
        self.canvas.setStyleSheet(
            f"background: {COLORS['canvas']};"
            f" border: 1px solid {COLORS['border']};"
            " border-radius: 8px;"
            f" color: {COLORS['muted']};"
        )
        layout.addWidget(self.canvas)

        self.overlay = QLabel(self.canvas)
        self.overlay.setStyleSheet(_OVERLAY_STYLE)
        self.overlay.hide()

        self._image: QImage | None = None
        self._frame: ImageFrame | None = None
        self._received_at = 0.0
        self._sim_time: float | None = None
        self._stamps: deque[float] = deque(maxlen=120)

    def show_frame(self, frame: ImageFrame) -> None:
        self._frame = frame
        # 雷达是单通道回波强度，铺成伪彩比灰度更容易分强弱回波。
        self._image = qimage_from_frame(frame, colorize=frame.is_radar)
        self._received_at = time.monotonic()
        self._stamps.append(self._received_at)
        self.canvas.setText("")
        self.canvas.set_radar((frame.range_m, frame.bearing()) if frame.is_radar else None)
        self.overlay.show()
        self._update_overlay()
        self._refresh()
        self._place_overlay()

    def set_sim_time(self, sim_now: float | None) -> None:
        """仿真时钟推进时刷新时效，不必等下一帧。"""
        if self._sim_time == sim_now:
            return
        self._sim_time = sim_now
        if self._frame is not None and self._frame.is_radar:
            self._update_overlay()
            self._place_overlay()

    def fps(self) -> float:
        now = time.monotonic()
        return float(sum(1 for stamp in self._stamps if now - stamp <= 1.0))

    def _update_overlay(self) -> None:
        frame = self._frame
        if frame is None:
            return
        lines = [self.sensor, f"{frame.encoding} · {frame.width}×{frame.height}"]
        tail = f"{self.fps():.1f} Hz"
        if frame.is_radar:
            lines.append(
                f"range {fmt_number(frame.range_m, 0)} m · "
                f"{fmt_number(frame.rotation_rpm, 0)} rpm"
            )
            tail += f" · 时效 {self._age_text(frame)}"
        lines.append(tail)
        self.overlay.setText("\n".join(lines))

    def _age_text(self, frame: ImageFrame) -> str:
        age = frame.age(self._sim_time)
        if age is None:
            # 仿真时钟还没到（state 尚未发布）时退化成「这一帧在界面上放了多久」。
            age = time.monotonic() - self._received_at
        return f"{age:.2f} s"

    def _refresh(self) -> None:
        if self._image is None:
            return
        pixmap = QPixmap.fromImage(self._image).scaled(
            self.canvas.size(),
            Qt.AspectRatioMode.KeepAspectRatio,
            Qt.TransformationMode.SmoothTransformation,
        )
        self.canvas.setPixmap(pixmap)

    def _place_overlay(self) -> None:
        if not self.overlay.isVisible():
            return
        self.overlay.adjustSize()
        self.overlay.move(12, 12)

    def resizeEvent(self, event) -> None:  # noqa: N802 - Qt 命名
        super().resizeEvent(event)
        self._refresh()
        self._place_overlay()


class ImagePanel(QWidget):
    """多路图像容器：按传感器自动分格。"""

    def __init__(self) -> None:
        super().__init__()
        self._grid = QGridLayout(self)
        self._grid.setContentsMargins(12, 12, 12, 12)
        self._grid.setSpacing(10)

        self._placeholder = QLabel("等待图像数据…", self)
        self._placeholder.setAlignment(Qt.AlignmentFlag.AlignCenter)
        self._placeholder.setMinimumSize(320, 240)
        self._placeholder.setStyleSheet(
            f"background: {COLORS['canvas']};"
            f" border: 1px solid {COLORS['border']};"
            " border-radius: 8px;"
            f" color: {COLORS['muted']};"
        )
        self._grid.addWidget(self._placeholder, 0, 0)

        self._tiles: dict[str, ImageTile] = {}
        self._active: ImageTile | None = None

    # -- 兼容旧调用方/自检脚本 -----------------------------------------
    @property
    def canvas(self) -> QLabel:
        """最近更新那一格的画布；还没有数据时返回占位提示。"""
        return self._active.canvas if self._active is not None else self._placeholder

    @property
    def tiles(self) -> dict[str, ImageTile]:
        return self._tiles

    # -- 数据入口 -----------------------------------------------------
    def show_frame(self, frame: ImageFrame, sensor: str = "") -> None:
        name = sensor or "—"
        tile = self._tiles.get(name)
        if tile is None:
            tile = ImageTile(name, self)
            self._tiles[name] = tile
            self._reflow()
        tile.show_frame(frame)
        self._active = tile

    def tick(self, sim_time: float | None) -> None:
        """主循环的固定节拍：把最新仿真时刻推给每一格，刷新雷达时效。"""
        for tile in self._tiles.values():
            tile.set_sim_time(sim_time)

    # -- 内部 ---------------------------------------------------------
    def _reflow(self) -> None:
        while self._grid.count():
            self._grid.takeAt(0)
        for column in range(4):
            self._grid.setColumnStretch(column, 0)
        for row in range(4):
            self._grid.setRowStretch(row, 0)

        if not self._tiles:
            self._placeholder.show()
            self._grid.addWidget(self._placeholder, 0, 0)
            return

        self._placeholder.hide()
        tiles = list(self._tiles.values())
        columns = 1 if len(tiles) == 1 else (2 if len(tiles) <= 4 else 3)
        rows = (len(tiles) + columns - 1) // columns
        for index, tile in enumerate(tiles):
            row, column = divmod(index, columns)
            # 末行只剩一格时让它横跨整行，避免右侧留下大片空白。
            span = columns - column if row == rows - 1 and len(tiles) - row * columns == 1 else 1
            self._grid.addWidget(tile, row, column, 1, span)
        for column in range(columns):
            self._grid.setColumnStretch(column, 1)
        for row in range(rows):
            self._grid.setRowStretch(row, 1)
