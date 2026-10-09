# Vendored themes

These 21 JSON files are a point-in-time copy of the `themes/` directory in
[gpui-component](https://github.com/longbridge/gpui-kit) (now GPUI Kit), taken
at commit `a8d1d26`. They are embedded into the binary at compile time (see
`src/ui/theme_catalogue.rs`), because nothing ships them to us at runtime — and
the published `gpui-component` crate does not contain them at all.

Each file names its own author and upstream URL; leave that metadata intact.

Upstream may have added, changed, or removed themes since this snapshot. To
refresh, re-copy `themes/` from the upstream repository at the commit of the
`gpui-component` release in `Cargo.toml`. The crate in `~/.cargo/registry/`
will not do: `themes/` is not in it.

Note that the catalogue is uneven: only 10 of the 21 families ship both a light
and a dark variant, ten are dark-only, and `aurora` is light-only. That is why
the app lets you choose a light theme and a dark theme independently rather
than picking a single "family".
