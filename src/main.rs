//! Pixstride Stage A 的 Windows 原生程序入口。

#![cfg_attr(not(windows), allow(dead_code))]

mod app;
mod diagnostics;
mod image;
mod large;
mod renderer;
mod view_transform;
mod window;

fn main() -> anyhow::Result<()> {
    app::run()
}
