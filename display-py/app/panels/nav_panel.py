"""导航页：GPS / IMU / 组合导航 / 里程计的数值卡片。"""

from __future__ import annotations

import math
from dataclasses import dataclass

from PySide6.QtWidgets import QGridLayout, QScrollArea, QVBoxLayout, QWidget

from ..widgets import Card, flash


@dataclass(frozen=True)
class Row:
    key: str
    label: str
    unit: str = ""
    digits: int = 2
    scale: float = 1.0
    wrap_deg: bool = False


DEG = 180.0 / math.pi

SPECS: dict[str, tuple[str, tuple[Row, ...]]] = {
    "gps": (
        "GPS",
        (
            Row("fix", "定位状态"),
            Row("latitude", "纬度", "°", 7),
            Row("longitude", "经度", "°", 7),
            Row("altitude", "高度", "m", 2),
            Row("vx", "东向速度", "m/s", 2),
            Row("vy", "北向速度", "m/s", 2),
            Row("vz", "垂向速度", "m/s", 2),
        ),
    ),
    "imu": (
        "IMU",
        (
            Row("fx", "比力 X", "m/s²", 3),
            Row("fy", "比力 Y", "m/s²", 3),
            Row("fz", "比力 Z", "m/s²", 3),
            Row("wx", "角速度 X", "rad/s", 3),
            Row("wy", "角速度 Y", "rad/s", 3),
            Row("wz", "角速度 Z", "rad/s", 3),
            Row("quality", "质量"),
        ),
    ),
    "ins": (
        "组合导航",
        (
            Row("px", "位置 X", "m", 2),
            Row("py", "位置 Y", "m", 2),
            Row("pz", "位置 Z", "m", 2),
            Row("vx", "速度 X", "m/s", 2),
            Row("vy", "速度 Y", "m/s", 2),
            Row("vz", "速度 Z", "m/s", 2),
            Row("heading", "航向", "°", 1, DEG, True),
            Row("filter", "滤波状态"),
        ),
    ),
    "odom": (
        "里程计",
        (
            Row("px", "位置 X", "m", 2),
            Row("py", "位置 Y", "m", 2),
            Row("pz", "位置 Z", "m", 2),
            Row("vx", "速度 X", "m/s", 2),
            Row("vy", "速度 Y", "m/s", 2),
            Row("vz", "速度 Z", "m/s", 2),
            Row("heading", "航向", "°", 1, DEG, True),
            Row("distance", "累计航程", "m", 2),
        ),
    ),
}


def format_row(value: object, row: Row) -> str:
    if isinstance(value, bool):
        return "是" if value else "否"
    if isinstance(value, str):
        return value or "—"
    if value is None:
        return "—"
    try:
        number = float(value)
    except (TypeError, ValueError):
        return str(value)
    if not math.isfinite(number):
        return "—"
    number *= row.scale
    if row.wrap_deg:
        number %= 360.0
    return f"{number:.{row.digits}f}"


class _SampleCard(Card):
    def __init__(self, title: str, badge: str, rows: tuple[Row, ...]) -> None:
        super().__init__(title, badge)
        self._rows: dict[str, tuple[Row, object]] = {}
        for index, row in enumerate(rows):
            label = self.add_row(index, row.label, row.unit)
            self._rows[row.key] = (row, label)

    def update_fields(self, fields: dict[str, object]) -> None:
        for key, (row, label) in self._rows.items():
            flash(label, format_row(fields.get(key), row))


class NavPanel(QWidget):
    def __init__(self) -> None:
        super().__init__()
        outer = QVBoxLayout(self)
        outer.setContentsMargins(0, 0, 0, 0)

        self._holder = QWidget()
        self._grid = QGridLayout(self._holder)
        self._grid.setContentsMargins(12, 12, 12, 12)
        self._grid.setSpacing(12)
        self._grid.setColumnStretch(0, 1)
        self._grid.setColumnStretch(1, 1)

        scroll = QScrollArea()
        scroll.setWidgetResizable(True)
        scroll.setWidget(self._holder)
        outer.addWidget(scroll)

        self._cards: dict[tuple[str, str], _SampleCard] = {}

    def update_sample(self, kind: str, sensor: str, fields: dict[str, object]) -> None:
        spec = SPECS.get(kind)
        if spec is None:
            return
        title, rows = spec
        key = (kind, sensor)
        card = self._cards.get(key)
        if card is None:
            card = _SampleCard(title, sensor, rows)
            index = len(self._cards)
            self._grid.addWidget(card, index // 2, index % 2)
            self._cards[key] = card
        card.update_fields(fields)
