"""操控页：临时令牌 + 推杆 → `ControlCmd`（10 Hz）→ 桥接 → 网页，下方显示浏览器回执。

网页侧每包只生效 500 ms，所以这里必须持续发；停发超过 400 ms 桥接会自己交还
控制权。急停与交还控制权都是一次性指令，之后停止下发，必须显式「重新接管」。

令牌不进 `dataflow.yml`：在这一页填一次，经 `cmd_token` 端口下发给桥接，桥接只
存在内存里。网页复位或换运行前缀后重新填一次即可恢复。
"""

from __future__ import annotations

import time

from PySide6.QtCore import Qt, QTimer
from PySide6.QtWidgets import (
    QComboBox,
    QGridLayout,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QPushButton,
    QSlider,
    QVBoxLayout,
    QWidget,
)

from .. import control
from ..theme import COLORS
from ..widgets import Card, fmt_number

#: UI 刷新「当前目标」的节拍；真正的 10 Hz 下发由 `NodeWorker` 维持，
#: 界面卡顿不会把控制流打断。
SEND_INTERVAL_MS = 100


class _Axis(QWidget):
    """一格推杆：名称 + 滑块 + 归一化数值。"""

    def __init__(self, key: str) -> None:
        super().__init__()
        layout = QGridLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setHorizontalSpacing(10)
        layout.setColumnStretch(1, 1)

        name = QLabel(key)
        name.setObjectName("cardKey")
        self.slider = QSlider(Qt.Orientation.Horizontal)
        self.slider.setRange(-100, 100)
        self.slider.setValue(0)
        self.value = QLabel("+0.00")
        self.value.setObjectName("value")
        self.value.setFixedWidth(52)
        self.value.setAlignment(Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter)

        layout.addWidget(name, 0, 0)
        layout.addWidget(self.slider, 0, 1)
        layout.addWidget(self.value, 0, 2)
        self.slider.valueChanged.connect(self._refresh)

    def _refresh(self) -> None:
        self.value.setText(f"{self.value_at():+.2f}")

    def value_at(self) -> float:
        """滑块 → [-1, 1]。"""
        return self.slider.value() / 100.0

    def reset(self) -> None:
        self.slider.setValue(0)


class ControlPanel(QWidget):
    def __init__(self, worker) -> None:
        super().__init__()
        self.worker = worker
        self._halted = False
        self._last_ack_at = 0.0
        self._accepted = 0
        self._rejected = 0

        root = QHBoxLayout(self)
        root.setContentsMargins(12, 12, 12, 12)
        root.setSpacing(12)

        # -- 左：令牌 + 推杆 -------------------------------------------
        left = QVBoxLayout()
        left.setSpacing(10)

        self.token_card = Card("接入令牌", control.TOKEN_PORT)
        self.token_input = QLineEdit()
        self.token_input.setPlaceholderText("网页「接入程序 > 允许外部控制」给的临时令牌")
        self.token_input.setEchoMode(QLineEdit.EchoMode.Password)
        self.token_input.returnPressed.connect(self._on_token)
        self.token_card.body.addWidget(self.token_input, 0, 0, 1, 3)

        token_buttons = QHBoxLayout()
        token_buttons.setSpacing(8)
        apply_button = QPushButton("下发令牌")
        apply_button.clicked.connect(self._on_token)
        clear_button = QPushButton("清除")
        clear_button.clicked.connect(self._on_clear_token)
        token_buttons.addWidget(apply_button)
        token_buttons.addWidget(clear_button)
        token_buttons.addStretch(1)
        self.token_card.body.addLayout(token_buttons, 1, 0, 1, 3)

        self.token_hint = QLabel("令牌只下发给桥接、存内存，不写进 dataflow.yml。")
        self.token_hint.setWordWrap(True)
        self.token_hint.setObjectName("cardUnit")
        self.token_card.body.addWidget(self.token_hint, 2, 0, 1, 3)
        left.addWidget(self.token_card)

        self.drive = Card("推杆", "10 Hz")
        self.mode = QComboBox()
        self.mode.addItems(["统一油门", "左右差动"])
        mode_key = QLabel("模式")
        mode_key.setObjectName("cardKey")
        self.drive.body.addWidget(mode_key, 0, 0)
        self.drive.body.addWidget(self.mode, 0, 1, 1, 2)

        self.throttle = _Axis("油门")
        self.port = _Axis("左桨")
        self.starboard = _Axis("右桨")
        self.steering = _Axis("舵")
        self.drive.body.addWidget(self.throttle, 1, 0, 1, 3)
        self.drive.body.addWidget(self.port, 1, 0, 1, 3)
        self.drive.body.addWidget(self.starboard, 2, 0, 1, 3)
        self.drive.body.addWidget(self.steering, 3, 0, 1, 3)
        left.addWidget(self.drive)

        buttons = QHBoxLayout()
        buttons.setSpacing(8)
        self.stop_button = QPushButton("停车")
        self.stop_button.clicked.connect(self._on_stop)
        self.estop_button = QPushButton("急停")
        self.estop_button.setStyleSheet(
            f"background: {COLORS['error']}; color: #ffffff;"
            " font-weight: 600; padding: 7px 16px; border-radius: 6px;"
        )
        self.estop_button.clicked.connect(self._on_estop)
        self.release_button = QPushButton("交还控制权")
        self.release_button.clicked.connect(self._on_release)
        buttons.addWidget(self.stop_button)
        buttons.addWidget(self.estop_button)
        buttons.addWidget(self.release_button)
        buttons.addStretch(1)
        left.addLayout(buttons)
        left.addStretch(1)
        root.addLayout(left, 1)

        # -- 右：回执 --------------------------------------------------
        side = QVBoxLayout()
        side.setSpacing(10)
        self.ack_card = Card("回执", "cmd_ack")
        self.status = self.ack_card.add_row(0, "状态")
        self.reason = QLabel("—")
        self.reason.setWordWrap(True)
        self.reason.setObjectName("cardUnit")
        self.reason.setAlignment(Qt.AlignmentFlag.AlignLeft | Qt.AlignmentFlag.AlignTop)
        self.ack_card.body.addWidget(self.reason, 1, 0, 1, 3)
        self.kind_row = self.ack_card.add_row(2, "指令")
        self.sent_row = self.ack_card.add_row(3, "已发送")
        self.count_row = self.ack_card.add_row(4, "受理 / 拒绝")
        self.age_row = self.ack_card.add_row(5, "距上次受理", "s")
        self.hint = QLabel("移动推杆即开始控制；停发 400 ms 桥接会自动交还控制权。")
        self.hint.setWordWrap(True)
        self.hint.setObjectName("cardUnit")
        self.ack_card.body.addWidget(self.hint, 6, 0, 1, 3)
        side.addWidget(self.ack_card)
        side.addStretch(1)
        holder = QWidget()
        holder.setLayout(side)
        holder.setFixedWidth(300)
        root.addWidget(holder)

        self.mode.currentIndexChanged.connect(self._sync_mode)
        self._sync_mode()

        self.timer = QTimer(self)
        self.timer.setInterval(SEND_INTERVAL_MS)
        self.timer.timeout.connect(self._tick)
        self.timer.start()

    # -- 外部入口 -----------------------------------------------------
    def show_ack(self, ack: dict) -> None:
        """桥接回传的回执（`ControlAck`）。"""
        status = str(ack.get("status") or "")
        tone = {
            "accepted": COLORS["ok"],
            "rejected": COLORS["error"],
            "released": COLORS["warn"],
        }.get(status, COLORS["muted"])
        self.status.setText(control.STATUS_TEXT.get(status, status or "未知"))
        self.status.setStyleSheet(f"color: {tone}; font-weight: 600;")
        self.reason.setText(ack.get("reason") or "—")
        kind = str(ack.get("kind") or "")
        self.kind_row.setText(kind or "—")
        if kind == control.KIND_TOKEN:
            # 令牌回执单独回显在令牌卡片上，别混进推杆的状态里
            self.token_hint.setText(ack.get("reason") or "令牌已更新")

        if status == "accepted":
            self._accepted += 1
            self._last_ack_at = time.monotonic()
        elif status == "rejected":
            self._rejected += 1
        elif status == "released" and not self._halted:
            # 上游静默 400 ms 的兜底释放：动一下推杆就会重新租约，别把面板锁死
            self.hint.setText("桥接已释放控制权（上游静默）；继续推杆会自动重新接管。")
        self.count_row.setText(f"{self._accepted} / {self._rejected}")
        self._refresh_age()

    # -- 令牌 ---------------------------------------------------------
    def _on_token(self) -> None:
        value = self.token_input.text().strip()
        if not value:
            self._on_clear_token()
            return
        self.worker.send_token(control.token(value))
        self.token_hint.setText(
            "令牌已下发；点「重新接管」后推杆生效。"
            if self._halted
            else "令牌已下发：桥接开始发现网页实例，结果会回到「回执」。"
        )

    def _on_clear_token(self) -> None:
        self.token_input.clear()
        self.worker.send_token(control.token(""))
        self._set_halted(True, "令牌已清除；填新令牌后可重新接管。")
        self.token_hint.setText("已请求清除令牌，桥接会交还控制权并断开。")

    # -- 指令 ---------------------------------------------------------
    def _tick(self) -> None:
        if not self._halted:
            self._send_control()
        self.sent_row.setText(f"{self.worker.sent_commands:,}")
        self._refresh_age()

    def _send_control(self) -> None:
        """只更新发送目标；真正的 10 Hz 节拍由工作线程维持，界面卡顿不影响。"""
        if self.mode.currentIndex() == 1:
            array = control.command(
                "control",
                port=self.port.value_at(),
                starboard=self.starboard.value_at(),
                steering=self.steering.value_at(),
            )
        else:
            array = control.command(
                "control",
                throttle=self.throttle.value_at(),
                steering=self.steering.value_at(),
            )
        self.worker.set_control(array)

    def _on_stop(self) -> None:
        for axis in (self.throttle, self.steering, self.port, self.starboard):
            axis.reset()
        self._set_halted(False, "已收油门；停发 400 ms 会自动交还控制权。")
        self._send_control()

    def _on_estop(self) -> None:
        if self._halted:
            self._resume()
            return
        self._set_halted(True, "急停已下发；点「重新接管」才恢复推进。")
        self.worker.send_command(control.command("estop"))

    def _on_release(self) -> None:
        self._set_halted(True, "已交还控制权；点「重新接管」恢复推进。")
        self.worker.send_command(control.command("release"))

    def _resume(self) -> None:
        for axis in (self.throttle, self.steering, self.port, self.starboard):
            axis.reset()
        self._set_halted(False, "已重新接管；停发 400 ms 会自动交还控制权。")
        self._send_control()

    # -- 内部 ---------------------------------------------------------
    def _sync_mode(self, *_: object) -> None:
        differential = self.mode.currentIndex() == 1
        self.throttle.setVisible(not differential)
        self.port.setVisible(differential)
        self.starboard.setVisible(differential)

    def _set_halted(self, halted: bool, hint: str) -> None:
        self._halted = halted
        if halted:
            # 停下就别再往网页送推进量，否则急停/交还之后还会追着发
            self.worker.stop_control()
        for axis in (self.throttle, self.steering, self.port, self.starboard):
            axis.setEnabled(not halted)
        self.mode.setEnabled(not halted)
        self.estop_button.setText("重新接管" if halted else "急停")
        self.hint.setText(hint)

    def _refresh_age(self) -> None:
        if not self._last_ack_at:
            self.age_row.setText("—")
            return
        self.age_row.setText(fmt_number(time.monotonic() - self._last_ack_at, 1))
