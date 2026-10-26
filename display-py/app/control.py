"""控制协议的 Python 侧：打包 `ControlCmd` / `ControlToken`、解析 `ControlAck`。

字段与 `types/usv/occsim/control/v1.yml` 逐字段对齐：
    ControlCmd    kind: Utf8, throttle/steering/port/starboard: Float64
    ControlToken  token: Utf8
    ControlAck    status/kind/reason: Utf8

约定：控制量归一化到 [-1, 1]，0 是有效指令（停车/回中），**没给**的量填 NaN。
桥接侧还有一层校验（范围、推进二选一、control 必须有推进量），这里只保证形状对。
"""

from __future__ import annotations

import math

import pyarrow as pa

from .decode import scalar

#: 端口名，与 dataflow.yml、桥接侧一致；都不含 `__`，不会被当成观测端口。
CMD_PORT = "cmd"
TOKEN_PORT = "cmd_token"
ACK_PORT = "cmd_ack"

#: kind 的三个取值。
KINDS = ("control", "estop", "release")

#: 令牌下发回执专用的 kind（不是发给网页的指令）。
KIND_TOKEN = "token"

#: 控制量的绝对值上限。
LIMIT = 1.0

#: 回执状态 → 中文，用于面板展示。
STATUS_TEXT = {
    "accepted": "已受理",
    "rejected": "被拒绝",
    "released": "已释放",
}

#: 必须与 types 里的 ControlCmd 一致（顺序也一致）。
CONTROL_CMD_TYPE = pa.struct(
    [
        pa.field("kind", pa.string(), nullable=True),
        pa.field("throttle", pa.float64(), nullable=True),
        pa.field("steering", pa.float64(), nullable=True),
        pa.field("port", pa.float64(), nullable=True),
        pa.field("starboard", pa.float64(), nullable=True),
    ]
)


def _number(value: float | None) -> float:
    """None/NaN/Inf → NaN；有限值夹到 [-1, 1]。"""
    if value is None:
        return float("nan")
    number = float(value)
    if not math.isfinite(number):
        return float("nan")
    return max(-LIMIT, min(LIMIT, number))


def command(
    kind: str,
    *,
    throttle: float | None = None,
    steering: float | None = None,
    port: float | None = None,
    starboard: float | None = None,
) -> pa.StructArray:
    """打包成单行 `ControlCmd`。"""
    if kind not in KINDS:
        raise ValueError(f"未知 kind {kind!r}")
    return pa.array(
        [
            {
                "kind": kind,
                "throttle": _number(throttle),
                "steering": _number(steering),
                "port": _number(port),
                "starboard": _number(starboard),
            }
        ],
        type=CONTROL_CMD_TYPE,
    )


#: 必须与 types 里的 ControlToken 一致。
CONTROL_TOKEN_TYPE = pa.struct([pa.field("token", pa.string(), nullable=True)])


def token(value: str) -> pa.StructArray:
    """打包成单行 `ControlToken`；空串表示清除令牌并交还控制权。"""
    return pa.array([{"token": str(value).strip()}], type=CONTROL_TOKEN_TYPE)


def describe(kind: str, **values: float | None) -> str:
    """一行人类可读的指令摘要，例如 `control throttle +0.25 · steering +0.00`。"""
    parts = [kind]
    for name, value in values.items():
        if value is None:
            continue
        parts.append(f"{name} {value:+.2f}")
    return " · ".join(parts)


def decode_ack(value) -> dict:
    """把 dora 收到的 `ControlAck` 读成 `{status, kind, reason}`。"""
    return {
        "status": (scalar(value, "status") or "").strip(),
        "kind": (scalar(value, "kind") or "").strip(),
        "reason": (scalar(value, "reason") or "").strip(),
    }
