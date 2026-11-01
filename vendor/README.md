# Vendored crates

`i-slint-renderer-femtovg` is Slint's renderer at 1.18.1, unchanged apart from the lines
marked `Bubble:` in `itemrenderer.rs`: item origins, sizes, border widths and text
baselines land on whole device pixels at every display scale.

When upgrading Slint, copy in the new version of the crate and reapply those lines.
