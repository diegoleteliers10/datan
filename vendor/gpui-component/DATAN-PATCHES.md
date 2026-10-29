# Datan patches

This folder contains `gpui-component` 0.7.3. Datan uses a local Cargo patch because the upstream
chart builders do not expose font size for axis labels.

Datan adds `axis_font_size` to LineChart, AreaChart, BarChart and CandlestickChart. The chart
renderers scale label text and the axis space with this value. Datan adds `label_font_size` to
PieChart and RadarChart. The date picker and calendar accept `ui_scale`, which scales their
input, popup, month grid, labels, and click targets. The scale is bounded from 0.75 to 1.5 so the
picker stays usable at the canvas zoom limits. Defaults keep upstream sizing. Datan passes the
canvas zoom from `crates/datan-app/src/main.rs`.

Rebase these changes when Datan updates `gpui-component`. Keep upstream defaults unchanged.
