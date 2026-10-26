"""OccSim 传感器显示程序入口。

由 dora 数据流拉起：

    dora run dataflow.yml        # dataflow.yml 里已包含 occsim-display 节点
"""

from __future__ import annotations

import sys

from PySide6.QtGui import QFont
from PySide6.QtWidgets import QApplication

from app.theme import FONT_UI
from app.window import MainWindow


def main() -> int:
    app = QApplication(sys.argv)
    app.setApplicationName("OccSim Display")
    app.setFont(QFont(FONT_UI, 9))

    window = MainWindow()
    window.show()
    return app.exec()


if __name__ == "__main__":
    raise SystemExit(main())
