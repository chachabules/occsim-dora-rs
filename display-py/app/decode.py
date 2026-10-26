"""把 dora 事件里的 Arrow 数据还原成 Python 可渲染的形式。

桥接节点发来的是单行 ``StructArray``，字段定义见 occsim-dora-rs 的 types/ 目录。
这里只做取值与校验，不做任何 Qt 相关的转换。
"""

from __future__ import annotations

import math
from dataclasses import dataclass

import numpy as np
import pyarrow as pa
import pyarrow.compute as pc

#: 图像编码 → 通道数，与 integration/common.py 保持一致。
CHANNELS = {"rgba8": 4, "mono8": 1}

#: 八类观测的 kind 后缀。
KINDS = ("image", "pointcloud", "gps", "imu", "ins", "odom", "state", "health")


class DecodeError(ValueError):
    """负载与声明不符，整帧丢弃。"""


@dataclass(slots=True)
class ImageFrame:
    """一帧图像；雷达 PPI 的极坐标参数来自 dora metadata，缺省为 None。"""

    width: int
    height: int
    encoding: str
    channels: int
    data: bytes
    capture_time: float | None = None
    range_m: float | None = None
    azimuth: float | None = None
    antenna_heading: float | None = None
    rotation_rpm: float | None = None
    heading_up: bool = True

    @property
    def is_radar(self) -> bool:
        """有量程且知道天线指向，才算雷达 PPI（相机只有采集时刻）。"""
        return self.range_m is not None and (
            self.azimuth is not None or self.antenna_heading is not None
        )

    def bearing(self) -> float | None:
        """天线相对屏幕正上方的方位角（度，顺时针）。

        艇艏朝上的 PPI 里 `azimuth` 是相对艇艏的角，需要减去基准朝向；
        正北朝上时屏幕正上就是正北，直接用 `azimuth`。
        """
        if self.azimuth is None:
            return None
        reference = self.antenna_heading if self.heading_up else None
        value = self.azimuth - reference if reference is not None else self.azimuth
        return math.degrees(value) % 360.0

    def age(self, sim_now: float | None) -> float | None:
        """相对当前仿真时刻的时效（秒）；仿真时钟未知时返回 None。"""
        if self.capture_time is None or sim_now is None:
            return None
        return max(0.0, sim_now - self.capture_time)


@dataclass(slots=True)
class CloudFrame:
    width: int
    height: int
    points: np.ndarray  # (N, 4) float32，列依次为 x/y/z/intensity


def field(value: pa.Array | pa.ChunkedArray, name: str):
    """按字段名取一列，缺失时返回 None。"""
    try:
        return pc.struct_field(value, name)
    except (pa.ArrowInvalid, pa.ArrowNotImplementedError, KeyError):
        return None


def scalar(value: pa.Array | pa.ChunkedArray, name: str):
    """取单行结构体里的某个标量，缺失时返回 None。"""
    column = field(value, name)
    if column is None or len(column) == 0:
        return None
    return column[0].as_py()


def metadata_text(metadata: dict | None, key: str, default: str = "") -> str:
    """metadata 里的字符串参数（兼容 bytes）。"""
    if not metadata:
        return default
    raw = metadata.get(key, default)
    if isinstance(raw, bytes):
        return raw.decode("utf-8", "replace")
    return str(raw)


def metadata_number(metadata: dict | None, key: str) -> float | None:
    """metadata 里的数值参数；dora 可能给 int/float/bytes/str，统一转 float。"""
    if not metadata:
        return None
    raw = metadata.get(key)
    if raw is None or isinstance(raw, bool):
        return None
    try:
        value = float(raw.decode("utf-8", "replace")) if isinstance(raw, bytes) else float(raw)
    except (TypeError, ValueError):
        return None
    return value if math.isfinite(value) else None


def metadata_flag(metadata: dict | None, key: str) -> bool | None:
    """metadata 里的布尔参数；缺省或非布尔字面量返回 None。"""
    if not metadata:
        return None
    raw = metadata.get(key)
    if isinstance(raw, bool):
        return raw
    text = metadata_text(metadata, key).strip().lower()
    if text in ("true", "1"):
        return True
    if text in ("false", "0"):
        return False
    return None


def kind_of(port: str) -> str:
    """端口名 → kind；类型端口就是自身，实例端口取 ``__`` 之后的部分。"""
    return port.rsplit("__", 1)[-1]


def decode_image(value, metadata: dict | None = None) -> ImageFrame:
    encoding = scalar(value, "encoding") or ""
    channels = CHANNELS.get(encoding)
    if channels is None:
        raise DecodeError(f"未知图像编码 {encoding!r}")
    width, height = scalar(value, "width"), scalar(value, "height")
    if not isinstance(width, int) or not isinstance(height, int) or width < 1 or height < 1:
        raise DecodeError(f"非法图像尺寸 {width}x{height}")
    data = scalar(value, "data")
    if not isinstance(data, (bytes, bytearray)):
        raise DecodeError("图像缺少像素数据")
    if len(data) != width * height * channels:
        raise DecodeError(
            f"像素长度不符：{len(data)} != {width}x{height}x{channels}"
        )
    # 结构体只装像素与尺寸，雷达极坐标由桥接放在 metadata 里。
    heading_up = metadata_flag(metadata, "headingUp")
    return ImageFrame(
        width,
        height,
        encoding,
        channels,
        bytes(data),
        capture_time=metadata_number(metadata, "captureTime"),
        range_m=metadata_number(metadata, "range"),
        azimuth=metadata_number(metadata, "azimuth"),
        antenna_heading=metadata_number(metadata, "antennaHeading"),
        rotation_rpm=metadata_number(metadata, "rotationRpm"),
        heading_up=True if heading_up is None else heading_up,
    )


def decode_pointcloud(value) -> CloudFrame:
    width, height = scalar(value, "width"), scalar(value, "height")
    count = scalar(value, "pointCount")
    data = scalar(value, "data")
    if not isinstance(data, (bytes, bytearray)):
        raise DecodeError("点云缺少点数据")
    if not count:
        raise DecodeError("点云缺少 pointCount")
    if len(data) != count * 16:
        raise DecodeError(f"点云长度不符：{len(data)} != {count}×16")
    points = np.frombuffer(data, dtype="<f4").reshape(-1, 4)
    return CloudFrame(int(width or 0), int(height or 0), points)


def decode_sample(value) -> dict[str, object]:
    """JSON 类观测：把所有字段摊平成 dict。"""
    struct_type = value.type
    names = list(getattr(struct_type, "names", []) or [])
    if not names:
        raise DecodeError("结构体没有字段")
    return {name: scalar(value, name) for name in names}
