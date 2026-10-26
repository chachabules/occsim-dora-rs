"""渲染工具：图像 → QImage（雷达走伪彩），点云 → 三维透视位图，雷达叠加层。

点云单帧 115200 点、10 Hz，这里全部用 numpy 做投影与栅格化，不依赖 OpenGL / pyqtgraph：
相机绕原点轨道运动，正交基投影 + 透视除法 + 深度缓冲，绘制结果仍是一张 RGB 位图，
交互（旋转/推拉/平移）由面板重新渲染实现。
"""

from __future__ import annotations

import math
from dataclasses import dataclass

import numpy as np
from PySide6.QtCore import QPointF, QRectF, Qt
from PySide6.QtGui import QColor, QImage, QPainter, QPen

from .decode import ImageFrame
from .theme import COLORS

#: 三维位图像素尺寸与默认量程（米）。
CLOUD_SIZE = 800
DEFAULT_RANGE = 80.0
RING_STEPS = (10, 20, 40, 80)

#: 默认视角：绕竖直轴的方位角、俯仰角（度），以及垂直视场角。
DEFAULT_YAW = 200.0
DEFAULT_PITCH = 22.0
FIELD_OF_VIEW = 45.0

#: 雷达方位刻度的间隔（度）。
AZIMUTH_STEP = 30
#: 量程环的候选间隔（米），取第一个不小于 range/4 的档位，保证大约 4 环。
RANGE_STEP_STEPS = (10, 20, 25, 50, 100, 200, 250, 500, 1000, 2000, 2500, 5000)

#: 高度/强度 → 颜色，青 → 绿 → 琥珀 → 红。
_LUT_STOPS = (
    (0.00, (34, 211, 238)),
    (0.45, (63, 185, 80)),
    (0.75, (210, 153, 34)),
    (1.00, (248, 81, 73)),
)

#: 雷达回波伪彩：底色 → 深蓝 → 青 → 绿 → 黄 → 橙 → 红。
_RADAR_STOPS = (
    (0.00, (4, 8, 14)),
    (0.12, (10, 44, 82)),
    (0.30, (12, 106, 148)),
    (0.48, (26, 176, 140)),
    (0.66, (168, 208, 74)),
    (0.82, (236, 158, 46)),
    (1.00, (250, 70, 52)),
)


def _build_lut(stops: tuple[tuple[float, tuple[int, int, int]], ...]) -> np.ndarray:
    """色标 → 256 级查找表。"""
    lut = np.zeros((256, 3), dtype=np.uint8)
    for index in range(256):
        t = index / 255.0
        for (t0, c0), (t1, c1) in zip(stops, stops[1:]):
            if t <= t1:
                k = (t - t0) / (t1 - t0)
                lut[index] = [c0[i] + (c1[i] - c0[i]) * k for i in range(3)]
                break
        else:
            lut[index] = stops[-1][1]
    return lut


_LUT = _build_lut(_LUT_STOPS)
_RADAR_LUT = _build_lut(_RADAR_STOPS)


# ---------------------------------------------------------------- 图像


def radar_rgb(data: bytes, width: int, height: int) -> np.ndarray:
    """mono8 雷达回波 → RGB 伪彩（强度越高越暖）。"""
    gray = np.frombuffer(data, dtype=np.uint8)
    return _RADAR_LUT[gray].reshape(height, width, 3)


def qimage_from_frame(frame: ImageFrame, colorize: bool = False) -> QImage:
    """把扁平像素包成 QImage；`colorize` 为真时单通道按雷达伪彩上色。

    返回的图已脱离原始缓冲。
    """
    if frame.channels == 1 and colorize:
        rgb = radar_rgb(frame.data, frame.width, frame.height)
        image = QImage(
            rgb.data,
            frame.width,
            frame.height,
            frame.width * 3,
            QImage.Format.Format_RGB888,
        )
    else:
        if frame.channels == 1:
            fmt = QImage.Format.Format_Grayscale8
        elif frame.channels == 4:
            fmt = QImage.Format.Format_RGBA8888
        else:  # 理论上 decode 已经拦掉
            raise ValueError(f"不支持的通道数 {frame.channels}")
        image = QImage(
            frame.data,
            frame.width,
            frame.height,
            frame.width * frame.channels,
            fmt,
        )
    if image.isNull():
        raise ValueError("QImage 构造失败")
    return image.copy()


# ---------------------------------------------------------------- 点云三维


@dataclass(slots=True)
class CloudRender:
    rgb: np.ndarray  # (size, size, 3) uint8
    total: int
    valid: int
    low: float  # 着色区间下界（m 或 intensity）
    high: float
    yaw: float = DEFAULT_YAW
    pitch: float = DEFAULT_PITCH


@dataclass(slots=True)
class _Camera:
    """绕原点轨道运动的透视相机；世界坐标为 X 前、Y 右、Z 上。"""

    eye: np.ndarray
    right: np.ndarray
    up: np.ndarray
    forward: np.ndarray
    focal: float

    def project(self, world: np.ndarray) -> tuple[np.ndarray, np.ndarray, np.ndarray]:
        """世界坐标 → 以画面中心为原点的像素偏移与相机深度。"""
        rel = world - self.eye
        x = rel @ self.right
        y = rel @ self.up
        depth = rel @ self.forward
        safe = np.where(depth > 1e-3, depth, 1e-3)
        return self.focal * x / safe, -self.focal * y / safe, depth


def _camera(yaw: float, pitch: float, distance: float, size: int) -> _Camera:
    yaw_r = math.radians(yaw)
    pitch_r = math.radians(pitch)
    direction = np.array(
        [
            math.cos(pitch_r) * math.cos(yaw_r),
            math.cos(pitch_r) * math.sin(yaw_r),
            math.sin(pitch_r),
        ]
    )
    eye = direction * distance
    forward = -direction
    right = np.cross(forward, np.array([0.0, 0.0, 1.0]))
    norm = float(np.linalg.norm(right))
    right = right / norm if norm > 1e-6 else np.array([1.0, 0.0, 0.0])
    up = np.cross(right, forward)
    focal = (size * 0.5) / math.tan(math.radians(FIELD_OF_VIEW) * 0.5)
    return _Camera(eye, right, up, forward, focal)


def render_cloud(
    points: np.ndarray,
    range_m: float = DEFAULT_RANGE,
    size: int = CLOUD_SIZE,
    color_by: str = "height",
    yaw: float = DEFAULT_YAW,
    pitch: float = DEFAULT_PITCH,
    zoom: float = 1.0,
) -> CloudRender:
    """三维透视栅格化：X 前 / Y 右 / Z 上，按高度或强度着色，带远近雾化。"""
    total = int(points.shape[0])
    rgb = np.zeros((size, size, 3), dtype=np.uint8)
    occupied = np.zeros(size * size, dtype=bool)
    camera = _camera(yaw, pitch, (2.8 * range_m) / max(zoom, 1e-3), size)

    if total == 0:
        _draw_ground(rgb, occupied, camera, range_m, size)
        return CloudRender(rgb, 0, 0, 0.0, 0.0, yaw, pitch)

    x, y, z, intensity = points[:, 0], points[:, 1], points[:, 2], points[:, 3]
    # 传感器坐标系 z 向下，翻成世界坐标的“上”。
    world = np.stack([x, y, -z], axis=1)
    finite = np.isfinite(world).all(axis=1)
    finite &= (np.abs(x) <= range_m) & (np.abs(y) <= range_m)
    if not finite.any():
        _draw_ground(rgb, occupied, camera, range_m, size)
        return CloudRender(rgb, total, 0, 0.0, 0.0, yaw, pitch)

    world = world[finite]
    values = (-z[finite]) if color_by == "height" else intensity[finite]
    sx, sy, depth = camera.project(world)
    px = np.rint(sx + size * 0.5)
    py = np.rint(sy + size * 0.5)
    inside = (depth > 1e-3) & (px >= 0) & (px < size) & (py >= 0) & (py < size)
    valid = int(inside.sum())
    if valid == 0:
        _draw_ground(rgb, occupied, camera, range_m, size)
        return CloudRender(rgb, total, 0, 0.0, 0.0, yaw, pitch)

    px = px[inside].astype(np.int32)
    py = py[inside].astype(np.int32)
    depth = depth[inside].astype(np.float32)
    values = np.where(np.isfinite(values[inside]), values[inside], 0.0).astype(np.float32)

    # 深度缓冲：同一像素只留最近的点（1/z 越大越近）
    nearness = 1.0 / np.maximum(depth, 1e-3)
    buffer = np.zeros(size * size, dtype=np.float32)
    index = py * size + px
    np.maximum.at(buffer, index, nearness)
    winner = nearness >= buffer[index]

    low, high = np.percentile(values, [2, 98])
    if not np.isfinite(low) or not np.isfinite(high) or high - low < 1e-6:
        low, high = float(values.min()), float(values.min()) + 1e-3
    normalized = np.clip((values - low) / (high - low), 0.0, 1.0)
    colors = _LUT[(normalized * 255).astype(np.uint8)].astype(np.float32)

    # 远处压暗：三维前后关系靠这个区分，比纯色散点好读。
    span = float(depth.max() - depth.min())
    fog = np.ones_like(depth) if span < 1e-6 else 1.0 - 0.55 * (depth - depth.min()) / span
    colors *= fog[:, None]

    flat = rgb.reshape(-1, 3)
    flat[index[winner]] = colors[winner].astype(np.uint8)

    occupied = buffer > 0.0
    _draw_ground(rgb, occupied, camera, range_m, size)
    return CloudRender(rgb, total, valid, float(low), float(high), yaw, pitch)


def _draw_ground(
    rgb: np.ndarray,
    occupied: np.ndarray,
    camera: _Camera,
    range_m: float,
    size: int,
) -> None:
    """地面参考：同心距离环 + 十字轴线，已被点云占用的像素不覆盖。"""
    flat = rgb.reshape(-1, 3)
    ring_color = np.array([104, 132, 164], dtype=np.uint8)
    axis_color = np.array([68, 86, 108], dtype=np.uint8)

    for meters in RING_STEPS:
        if meters > range_m:
            continue
        # 采样密度按周长给：太稀会画成断断续续的点，看不出是环。
        count = max(240, int(2.0 * math.pi * meters * 4))
        angles = np.linspace(0.0, 2.0 * math.pi, count)
        zeros = np.zeros_like(angles)
        ring = np.stack([np.cos(angles) * meters, np.sin(angles) * meters, zeros], axis=1)
        _paint(flat, occupied, camera, ring, size, ring_color)

    span = np.linspace(-range_m, range_m, max(240, int(2.0 * range_m * 4)))
    zeros = np.zeros_like(span)
    _paint(flat, occupied, camera, np.stack([span, zeros, zeros], axis=1), size, axis_color)
    _paint(flat, occupied, camera, np.stack([zeros, span, zeros], axis=1), size, axis_color)


def _paint(
    flat: np.ndarray,
    occupied: np.ndarray,
    camera: _Camera,
    world: np.ndarray,
    size: int,
    color: np.ndarray,
) -> None:
    sx, sy, depth = camera.project(world)
    px = np.rint(sx + size * 0.5).astype(np.int32)
    py = np.rint(sy + size * 0.5).astype(np.int32)
    inside = (depth > 1e-3) & (px >= 0) & (px < size) & (py >= 0) & (py < size)
    index = py[inside] * size + px[inside]
    empty = ~occupied[index]
    flat[index[empty]] = color
    # 被点云挡住的参考线做半透明叠加，否则水面这类密点云上几乎看不到距离环。
    covered = index[~empty]
    if covered.size:
        blended = flat[covered].astype(np.float32) * 0.25 + color.astype(np.float32) * 0.75
        flat[covered] = blended.astype(np.uint8)


def cloud_qimage(render: CloudRender) -> QImage:
    """位图 → QImage（已拷贝，可脱离 numpy 缓冲）。"""
    height, width = render.rgb.shape[:2]
    image = QImage(render.rgb.data, width, height, width * 3, QImage.Format.Format_RGB888)
    return image.copy()


# ---------------------------------------------------------------- 雷达叠加层


def range_ring_step(range_m: float) -> float:
    """把量程切成人眼友好的整数间隔，大约 4 环。"""
    if range_m <= 0:
        return 0.0
    target = range_m / 4.0
    for step in RANGE_STEP_STEPS:
        if step >= target:
            return float(step)
    return float(RANGE_STEP_STEPS[-1])


def draw_radar_overlay(
    painter: QPainter,
    rect: QRectF,
    *,
    range_m: float,
    bearing: float | None = None,
) -> None:
    """在 PPI 图像区域上叠加量程环与方位刻度。

    `rect` 是图像在控件里的实际显示矩形（可能是等比缩放并居中的），
    `bearing` 是天线的相对方位角（度，0 = 屏幕正上、顺时针）。
    """
    radius = min(rect.width(), rect.height()) / 2.0
    if range_m <= 0 or radius < 12:
        return
    center = rect.center()
    grid = QColor(COLORS["accent"])
    grid.setAlpha(80)
    label = QColor(COLORS["text"])
    label.setAlpha(165)

    painter.save()
    painter.setRenderHint(QPainter.RenderHint.Antialiasing)
    painter.setBrush(Qt.BrushStyle.NoBrush)

    pen = QPen(grid)
    pen.setWidthF(1.0)
    painter.setPen(pen)

    font = painter.font()
    font.setPointSizeF(max(7.0, min(9.0, radius / 26.0)))
    painter.setFont(font)
    baseline = font.pointSizeF() + 2.0

    step = range_ring_step(range_m)
    value = step
    while 0.0 < value < range_m * 0.99:
        ring = radius * value / range_m
        painter.setPen(pen)
        painter.drawEllipse(center, ring, ring)
        # 量程数字贴着正上方的环顶，避开放射状刻度。
        painter.setPen(label)
        painter.drawText(QPointF(center.x() + 4.0, center.y() - ring + baseline), f"{value:.0f}")
        value += step

    for degrees in range(0, 360, AZIMUTH_STEP):
        radians = math.radians(degrees)
        dx, dy = math.sin(radians), -math.cos(radians)
        # 0° 那条加长当参考方向；正上方留给量程数字，不写方位角避免重叠。
        inner = 0.90 if degrees == 0 else 0.93
        painter.setPen(pen)
        painter.drawLine(
            QPointF(center.x() + dx * radius * inner, center.y() + dy * radius * inner),
            QPointF(center.x() + dx * radius, center.y() + dy * radius),
        )
        if degrees == 0:
            continue
        painter.setPen(label)
        painter.drawText(
            QRectF(
                center.x() + dx * radius * 0.78 - 14.0,
                center.y() + dy * radius * 0.78 - 8.0,
                28.0,
                16.0,
            ),
            Qt.AlignmentFlag.AlignCenter,
            f"{degrees:d}",
        )

    if bearing is not None:
        radians = math.radians(bearing)
        sweep = QPen(QColor(COLORS["warn"]))
        sweep.setWidthF(1.4)
        painter.setPen(sweep)
        painter.drawLine(
            center,
            QPointF(center.x() + math.sin(radians) * radius, center.y() - math.cos(radians) * radius),
        )

    painter.restore()
