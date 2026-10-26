"""面板共用的小部件与格式化工具。"""

from __future__ import annotations

import math

from PySide6.QtCore import Qt, QTimer
from PySide6.QtWidgets import QFrame, QGridLayout, QHBoxLayout, QLabel, QVBoxLayout

from .theme import COLORS


def fmt_number(value: object, digits: int = 2) -> str:
    """数值格式化：None / NaN / Inf 统一显示为横杠。"""
    if value is None:
        return "—"
    if isinstance(value, bool):
        return "是" if value else "否"
    if isinstance(value, str):
        return value or "—"
    try:
        number = float(value)
    except (TypeError, ValueError):
        return str(value)
    if not math.isfinite(number):
        return "—"
    return f"{number:.{digits}f}"


def flash(label: QLabel, text: str, milliseconds: int = 180) -> None:
    """数值变化时短暂高亮，避免整屏跳动。"""
    if label.text() == text:
        return
    label.setText(text)
    label.setStyleSheet(f"color: {COLORS['accent']};")
    QTimer.singleShot(milliseconds, lambda: label.setStyleSheet(""))


class Card(QFrame):
    """标题 + 键值行的卡片。"""

    def __init__(self, title: str, badge: str = "") -> None:
        super().__init__()
        self.setObjectName("card")
        outer = QVBoxLayout(self)
        outer.setContentsMargins(14, 12, 14, 14)
        outer.setSpacing(8)

        header = QHBoxLayout()
        header.setSpacing(8)
        heading = QLabel(title)
        heading.setObjectName("cardTitle")
        header.addWidget(heading)
        header.addStretch(1)
        self.badge = QLabel(badge)
        self.badge.setObjectName("badge")
        self.badge.setVisible(bool(badge))
        header.addWidget(self.badge)
        outer.addLayout(header)

        self.body = QGridLayout()
        self.body.setHorizontalSpacing(10)
        self.body.setVerticalSpacing(4)
        self.body.setColumnStretch(1, 1)
        outer.addLayout(self.body)

    def add_row(self, row: int, key: str, unit: str = "") -> QLabel:
        label = QLabel(key)
        label.setObjectName("cardKey")
        value = QLabel("—")
        value.setObjectName("value")
        value.setAlignment(Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter)
        self.body.addWidget(label, row, 0)
        self.body.addWidget(value, row, 1)
        if unit:
            suffix = QLabel(unit)
            suffix.setObjectName("cardUnit")
            self.body.addWidget(suffix, row, 2)
        return value
