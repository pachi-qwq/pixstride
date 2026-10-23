//! 管理 Stage A 的 D3D11 渲染实现。

mod animation_timer;
mod d3d11;
mod playback_clock;
mod tiles;

#[cfg(test)]
pub(crate) use d3d11::render_thread;
pub(crate) use d3d11::render_thread_with_animation;
