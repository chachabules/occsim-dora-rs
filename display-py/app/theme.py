"""深色工程控制台配色与全局样式。"""

from __future__ import annotations

COLORS = {
    "bg": "#0F1419",
    "surface": "#161C23",
    "surface_alt": "#1C2530",
    "border": "#263140",
    "text": "#E6EDF3",
    "muted": "#8B949E",
    "accent": "#22D3EE",
    "accent_dim": "#0E7490",
    "ok": "#3FB950",
    "warn": "#D29922",
    "error": "#F85149",
    "canvas": "#000000",
}

FONT_UI = "Microsoft YaHei UI"
FONT_MONO = "Consolas"

STYLE_SHEET = f"""
QWidget {{
    background: {COLORS["bg"]};
    color: {COLORS["text"]};
    font-family: "{FONT_UI}";
    font-size: 13px;
}}
QMainWindow, QDialog {{ background: {COLORS["bg"]}; }}

QFrame#header {{
    background: {COLORS["surface"]};
    border-bottom: 1px solid {COLORS["border"]};
}}
QLabel#product {{
    font-size: 17px;
    font-weight: 600;
    color: {COLORS["text"]};
}}
QLabel#subtitle, QLabel#muted {{ color: {COLORS["muted"]}; }}
QLabel#mono, QLabel[role="mono"] {{
    font-family: "{FONT_MONO}";
    font-size: 12px;
    color: {COLORS["muted"]};
}}
QLabel#value {{
    font-family: "{FONT_MONO}";
    font-size: 15px;
    font-weight: 500;
    color: {COLORS["text"]};
}}
QLabel#valueAlert {{ color: {COLORS["warn"]}; }}
QLabel#badge {{
    background: {COLORS["surface_alt"]};
    border: 1px solid {COLORS["border"]};
    border-radius: 9px;
    padding: 1px 8px;
    color: {COLORS["accent"]};
    font-size: 11px;
}}
QLabel#dot {{ font-size: 14px; }}

QTabWidget::pane {{ border: none; background: {COLORS["bg"]}; }}
QTabBar {{ background: {COLORS["surface"]}; }}
QTabBar::tab {{
    background: transparent;
    color: {COLORS["muted"]};
    padding: 9px 26px;
    margin-right: 2px;
    border: none;
    border-bottom: 2px solid transparent;
    font-size: 13px;
}}
QTabBar::tab:hover {{ color: {COLORS["text"]}; }}
QTabBar::tab:selected {{
    color: {COLORS["accent"]};
    border-bottom: 2px solid {COLORS["accent"]};
    font-weight: 600;
}}

QFrame#card {{
    background: {COLORS["surface"]};
    border: 1px solid {COLORS["border"]};
    border-radius: 8px;
}}
QLabel#cardTitle {{
    font-size: 14px;
    font-weight: 600;
    color: {COLORS["text"]};
}}
QLabel#cardKey {{ color: {COLORS["muted"]}; font-size: 12px; }}
QLabel#cardUnit {{ color: {COLORS["muted"]}; font-size: 11px; }}

QStatusBar {{
    background: {COLORS["surface"]};
    border-top: 1px solid {COLORS["border"]};
    color: {COLORS["muted"]};
}}
QStatusBar::item {{ border: none; }}

QScrollArea {{ border: none; background: {COLORS["bg"]}; }}
QScrollBar:vertical {{
    background: transparent;
    width: 8px;
    margin: 0;
}}
QScrollBar::handle:vertical {{
    background: {COLORS["border"]};
    border-radius: 4px;
    min-height: 30px;
}}
QScrollBar::add-line, QScrollBar::sub-line {{ height: 0; }}

QComboBox {{
    background: {COLORS["surface_alt"]};
    border: 1px solid {COLORS["border"]};
    border-radius: 6px;
    padding: 4px 10px;
    color: {COLORS["text"]};
}}
QComboBox::drop-down {{ border: none; width: 18px; }}
QComboBox QAbstractItemView {{
    background: {COLORS["surface_alt"]};
    border: 1px solid {COLORS["border"]};
    selection-background-color: {COLORS["accent_dim"]};
}}

QToolTip {{
    background: {COLORS["surface_alt"]};
    color: {COLORS["text"]};
    border: 1px solid {COLORS["border"]};
    padding: 4px 6px;
}}
"""
