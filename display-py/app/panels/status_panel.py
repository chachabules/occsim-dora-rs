"""状态页：仿真状态、船体实际输出、健康度与本机接收统计。"""

from __future__ import annotations

import math

from PySide6.QtCore import Qt
from PySide6.QtWidgets import QGridLayout, QLabel, QWidget

from ..theme import COLORS
from ..widgets import Card, flash, fmt_number


def _degrees(value: object) -> str:
    """航向以弧度下发，页面按度显示；缺失值仍走 fmt_number 的横杠。"""
    try:
        angle = float(value)  # type: ignore[arg-type]
    except (TypeError, ValueError):
        return "—"
    if not math.isfinite(angle):
        return "—"
    return f"{math.degrees(angle) % 360.0:.1f}"


class StatusPanel(QWidget):
    def __init__(self) -> None:
        super().__init__()
        grid = QGridLayout(self)
        grid.setContentsMargins(12, 12, 12, 12)
        grid.setSpacing(12)

        self.state_card = Card("仿真状态")
        self.status = QLabel("等待状态数据…")
        self.status.setStyleSheet(
            f"font-size: 20px; font-weight: 600; color: {COLORS['muted']};"
        )
        self.status.setAlignment(Qt.AlignmentFlag.AlignLeft | Qt.AlignmentFlag.AlignVCenter)
        self.state_card.body.addWidget(self.status, 0, 0, 1, 3)

        self.sim_time = self.state_card.add_row(1, "仿真时刻", "s")
        self.control_mode = self.state_card.add_row(2, "控制模式")
        self.mission = self.state_card.add_row(3, "当前任务")
        grid.addWidget(self.state_card, 0, 0)

        # 网页 state.state 里的实际输出：转速是 shafts[].rpm，操舵量代替舵角
        self.hull_card = Card("船体")
        self.heading = self.hull_card.add_row(0, "航向", "°")
        self.rpm_port = self.hull_card.add_row(1, "左桨转速")
        self.rpm_starboard = self.hull_card.add_row(2, "右桨转速")
        self.steering = self.hull_card.add_row(3, "操舵")
        self.thrust = self.hull_card.add_row(4, "推力", "N")
        grid.addWidget(self.hull_card, 0, 1)

        self.health_card = Card("健康度")
        self.sent = self.health_card.add_row(0, "已发送")
        self.dropped = self.health_card.add_row(1, "丢弃")
        self.loss = self.health_card.add_row(2, "丢弃率", "%")
        grid.addWidget(self.health_card, 1, 0)

        self.stats_card = Card("接收统计")
        self.received = self.stats_card.add_row(0, "接收帧数")
        self.invalid = self.stats_card.add_row(1, "无效帧")
        self.ports = self.stats_card.add_row(2, "端口数")
        self.fps = self.stats_card.add_row(3, "帧率", "Hz")
        self.elapsed = self.stats_card.add_row(4, "运行时长", "s")
        grid.addWidget(self.stats_card, 1, 1)

        grid.setRowStretch(2, 1)
        self._sent_total = 0
        self._dropped_total = 0

    # -- 数据入口 -----------------------------------------------------
    def update_sample(self, kind: str, fields: dict[str, object]) -> None:
        if kind == "state":
            paused = fields.get("paused")
            if paused is True:
                text, color = "● 已暂停", COLORS["warn"]
            elif paused is False:
                text, color = "● 运行中", COLORS["ok"]
            else:
                text, color = "● 未知", COLORS["muted"]
            self.status.setText(text)
            self.status.setStyleSheet(f"font-size: 20px; font-weight: 600; color: {color};")
            flash(self.sim_time, fmt_number(fields.get("simTime"), 2))
            flash(self.control_mode, fmt_number(fields.get("controlMode")))
            flash(self.mission, fmt_number(fields.get("mission")))
            flash(self.heading, _degrees(fields.get("heading")))
            flash(self.rpm_port, fmt_number(fields.get("rpmPort"), 1))
            flash(self.rpm_starboard, fmt_number(fields.get("rpmStarboard"), 1))
            flash(self.steering, fmt_number(fields.get("steering"), 3))
            flash(self.thrust, fmt_number(fields.get("thrust"), 1))
        elif kind == "health":
            self._sent_total = int(fields.get("sent") or 0)
            self._dropped_total = int(fields.get("dropped") or 0)
            flash(self.sent, f"{self._sent_total:,}")
            flash(self.dropped, f"{self._dropped_total:,}")
            total = self._sent_total + self._dropped_total
            rate = self._dropped_total / total * 100.0 if total else 0.0
            flash(self.loss, fmt_number(rate, 2))

    def update_stats(self, stats: dict) -> None:
        flash(self.received, f"{int(stats.get('received', 0)):,}")
        flash(self.invalid, f"{int(stats.get('invalid', 0)):,}")
        flash(self.ports, str(int(stats.get('ports', 0))))
        flash(self.fps, fmt_number(stats.get("fps"), 1))
        flash(self.elapsed, fmt_number(stats.get("elapsed"), 0))
