"""不依赖 dora 数据流的自检：构造与桥接节点相同的 Arrow 结构，验证解码与渲染。

用法（在 occsim 环境里、本目录的上一级执行）：

    python tests/smoke_test.py
"""

from __future__ import annotations

import os
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import numpy as np
import pyarrow as pa

from app import control
from app.decode import decode_image, decode_pointcloud, decode_sample, kind_of
from app.node_worker import CONTROL_INTERVAL, NodeWorker
from app.render import qimage_from_frame, render_cloud

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")
from PySide6.QtWidgets import QApplication  # noqa: E402

from app.window import MainWindow  # noqa: E402


def image_struct(width: int, height: int, encoding: str, data: bytes) -> pa.StructArray:
    return pa.array(
        [{"width": width, "height": height, "encoding": encoding, "data": data}],
        type=pa.struct(
            [
                ("width", pa.uint32()),
                ("height", pa.uint32()),
                ("encoding", pa.string()),
                ("data", pa.large_binary()),
            ]
        ),
    )


def cloud_struct(count: int, data: bytes) -> pa.StructArray:
    return pa.array(
        [
            {
                "width": count,
                "height": 1,
                "encoding": "xyzi32f",
                "pointCount": count,
                "data": data,
            }
        ],
        type=pa.struct(
            [
                ("width", pa.uint32()),
                ("height", pa.uint32()),
                ("encoding", pa.string()),
                ("pointCount", pa.uint64()),
                ("data", pa.large_binary()),
            ]
        ),
    )


def gps_struct() -> pa.StructArray:
    fields = [
        ("fix", pa.string()),
        ("latitude", pa.float64()),
        ("longitude", pa.float64()),
        ("altitude", pa.float64()),
        ("vx", pa.float64()),
        ("vy", pa.float64()),
        ("vz", pa.float64()),
    ]
    values = {
        "fix": "3D",
        "latitude": 31.23,
        "longitude": 121.47,
        "altitude": 5.0,
        "vx": 0.1,
        "vy": 0.2,
        "vz": float("nan"),
    }
    return pa.array([values], type=pa.struct(fields))


def main() -> int:
    pixels = bytes([0, 128, 255, 64] * 4)  # 4x4 mono8
    frame = decode_image(image_struct(4, 4, "mono8", pixels))
    image = qimage_from_frame(frame)
    assert image.width() == 4 and image.height() == 4, image.size()
    print("image ok:", frame.width, frame.height, frame.encoding)

    points = np.array(
        [[1.0, 0.0, -1.0, 10.0], [20.0, 5.0, -2.0, 20.0], [np.nan, 0.0, 0.0, 0.0]],
        dtype="<f4",
    )
    cloud = decode_pointcloud(cloud_struct(3, points.tobytes()))
    result = render_cloud(cloud.points, 80.0)
    assert result.total == 3 and result.valid == 2, (result.total, result.valid)
    assert result.rgb.any(), "点云渲染结果全黑"
    # 三维：俯仰角变了，投影结果也必须跟着变（不是固定俯视图）。
    plan = render_cloud(cloud.points, 80.0, pitch=80.0)
    assert plan.rgb.shape == result.rgb.shape
    assert not np.array_equal(plan.rgb, result.rgb), "改俯仰角后画面没变，说明不是三维投影"
    print("cloud ok:", result.total, "points,", result.valid, "valid")

    sample = decode_sample(gps_struct())
    assert sample["fix"] == "3D" and abs(sample["latitude"] - 31.23) < 1e-9
    assert sample["vz"] != sample["vz"]  # NaN
    print("sample ok:", sample)

    assert kind_of("gps-mast__gps") == "gps"
    assert kind_of("image") == "image"

    app = QApplication(sys.argv)
    window = MainWindow()
    window.show()

    window.image_panel.show_frame(frame, "rgb-bow")
    radar = decode_image(
        image_struct(8, 8, "mono8", bytes([9] * 64)),
        {
            "sensor": "radar-mast",
            "range": 926.0,
            "azimuth": 3.2289,
            "antennaHeading": -3.099,
            "rotationRpm": 24.0,
            "captureTime": 550.04,
            "headingUp": True,
        },
    )
    assert radar.is_radar, "雷达帧没认出极坐标参数"
    # 艇艏朝上：天线相对方位 = azimuth - antennaHeading ≈ 2.6°
    assert abs((radar.bearing() or 0.0) - 2.6) < 0.5, radar.bearing()
    # 雷达走伪彩：弱回波偏蓝，不该是灰度（R=G=B）。
    colored = qimage_from_frame(radar, colorize=True)
    corner = colored.pixelColor(0, 0).getRgb()[:3]
    assert corner[2] > corner[0], f"雷达伪彩没生效: {corner}"
    window.image_panel.show_frame(radar, "radar-mast")
    window.cloud_panel.update_cloud(cloud, "lidar-mast")
    window.nav_panel.update_sample("gps", "gps-mast", sample)
    window.nav_panel.update_sample(
        "odom",
        "odom-main",
        {"px": 1.5, "py": -2.0, "pz": 0.0, "heading": 1.57, "distance": 42.0},
    )
    window.status_panel.update_sample(
        "state",
        {"simTime": 12.5, "paused": False, "controlMode": "manual", "mission": "survey"},
    )
    window.status_panel.update_sample("health", {"sent": 120, "dropped": 0, "simTime": 12.5})
    window.status_panel.update_stats(
        {"received": 120, "invalid": 0, "fps": 9.8, "ports": 3, "elapsed": 12.0}
    )
    for _ in range(20):
        app.processEvents()

    assert len(window.image_panel.tiles) == 2, window.image_panel.tiles.keys()
    assert window.image_panel.canvas.pixmap() is not None
    assert not window.image_panel.canvas.pixmap().isNull()

    # 雷达格：浮层要有量程/转速/时效，画布上要真的画出量程环与方位刻度。
    tile = window.image_panel.tiles["radar-mast"]
    window.image_panel.tick(550.2)
    overlay = tile.overlay.text()
    assert "range 926 m" in overlay and "24 rpm" in overlay, overlay
    assert "时效 0.16 s" in overlay, overlay
    tile.canvas.grab()  # 走一遍画布 paintEvent（含雷达叠加层）
    print("radar ok:", overlay.replace("\n", " / "))
    assert window.cloud_panel.view._pixmap is not None
    assert "°" in window.cloud_panel.view_label.text(), window.cloud_panel.view_label.text()
    window.cloud_panel.view.reset_view()  # 复位视角要能触发重绘
    for _ in range(5):
        app.processEvents()
    assert len(window.nav_panel._cards) == 2, window.nav_panel._cards
    assert "运行中" in window.status_panel.status.text(), window.status_panel.status.text()
    print("window ok:", window.windowTitle())

    # 控制协议：形状对齐、NaN 表示"没给"、超范围夹紧
    command = control.command("control", throttle=0.25, steering=0.0)
    assert command.type.field("kind").type == pa.string()
    row = command[0]
    assert row["kind"].as_py() == "control", row
    assert abs(row["throttle"].as_py() - 0.25) < 1e-9
    assert row["port"].as_py() != row["port"].as_py(), "没给的量必须是 NaN，不能是 0"
    estop = control.command("estop")[0]
    assert estop["throttle"].as_py() != estop["throttle"].as_py()
    assert control.command("control", throttle=2.0)[0]["throttle"].as_py() == 1.0

    # 令牌：去空白、字段名与 types 一致（ControlToken.token: Utf8）
    packed = control.token("  tok-abc  ")
    assert packed.type.field("token").type == pa.string()
    assert packed[0]["token"].as_py() == "tok-abc", packed
    assert control.token("")[0]["token"].as_py() == ""
    # 持续下发不依赖界面定时器：UI 卡住时工作线程仍按节拍送同一目标
    class FakeNode:
        def __init__(self) -> None:
            self.sent = []

        def send_output(self, port, payload) -> None:
            self.sent.append((port, payload))

        def log_info(self, message) -> None:
            pass

        def log_error(self, message) -> None:
            pass

    worker = NodeWorker()
    fake = FakeNode()
    worker.set_control(control.command("control", throttle=0.2))
    assert worker._drain_outbox(fake) is True
    assert len(fake.sent) == 1, fake.sent
    assert worker._drain_outbox(fake) is True  # 节拍未到，不重发
    assert len(fake.sent) == 1, fake.sent
    time.sleep(CONTROL_INTERVAL + 0.02)
    assert worker._drain_outbox(fake) is True
    assert len(fake.sent) == 2, fake.sent
    worker.stop_control()
    assert worker._drain_outbox(fake) is False
    print("control tick ok:", len(fake.sent), "sends")

    window.control_panel.token_input.setText("tok-abc")
    window.control_panel._on_token()
    window.control_panel._on_clear_token()
    assert window.control_panel.token_input.text() == ""
    assert not window.control_panel.throttle.isEnabled(), "清除令牌后应停止下发"
    window.control_panel._resume()  # 复位到未急停状态，后面的急停断言才成立

    # 回执解析 + 面板状态
    ack = control.decode_ack(
        pa.array(
            [{"status": "rejected", "kind": "control", "reason": "仿真已暂停"}],
            type=pa.struct([("status", pa.string()), ("kind", pa.string()), ("reason", pa.string())]),
        )
    )
    assert ack["status"] == "rejected" and ack["reason"] == "仿真已暂停", ack
    window.control_panel.show_ack(ack)
    assert window.control_panel.status.text() == "被拒绝", window.control_panel.status.text()
    assert "暂停" in window.control_panel.reason.text()

    window.control_panel.show_ack({"status": "accepted", "kind": "control", "reason": ""})
    assert window.control_panel.status.text() == "已受理"
    assert window.control_panel.count_row.text() == "1 / 1", window.control_panel.count_row.text()

    # 急停后停止下发、按钮变「重新接管」；再点一次恢复
    window.control_panel._on_estop()
    assert window.control_panel.estop_button.text() == "重新接管"
    assert not window.control_panel.throttle.isEnabled()
    window.control_panel._on_estop()
    assert window.control_panel.estop_button.text() == "急停"
    assert window.control_panel.throttle.isEnabled()
    print("control ok:", window.control_panel.count_row.text())

    window.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
