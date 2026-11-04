# Vendored crates

`i-slint-renderer-femtovg` is Slint's renderer at 1.18.1, unchanged apart from the lines
marked `Bubble:` in `itemrenderer.rs`: item origins, sizes, border widths and text
baselines land on whole device pixels at every display scale. A conic gradient starts at
the top, as Slint documents it (femtovg starts on the right).

When upgrading Slint, copy in the new version of the crate and reapply those lines.

`femtovg` is femtovg at 0.27.0, without its book and tests, and unchanged apart from
`src/renderer/opengl.rs` (marked `Bubble:`): it compiled all 16 shader programs for every
new window, 300 ms or more on some drivers. They now compile when first used.

Slint 1.18 pins this version of femtovg; upgrade the two together.
