[English](TERMINOLOGY.md) | [简体中文](TERMINOLOGY.zh-CN.md)

<!-- doc-id: terminology; revision: 2; status: active -->

# Terminology

Use these names consistently. Stages A–C implement some of the boundaries below; deferred concepts
remain vocabulary, not support claims. Consult the architecture for implementation scope and preserve API identifiers verbatim.

## Platform and image terms

| English | Preferred Simplified Chinese | Meaning |
|---|---|---|
| Win32 | Win32 原生 API | Windows desktop window, input, and system APIs used by the application. |
| windows-rs | windows-rs | Rust bindings; specify the crate and version when relevant. |
| D3D11 Immediate Context | D3D11 即时上下文 | The `ID3D11DeviceContext` used to submit rendering commands; one render-thread owner. |
| DXGI Swap Chain | DXGI 交换链 | The window presentation buffers and associated presentation state. |
| GPU Texture | GPU 纹理 | A GPU image resource such as `ID3D11Texture2D`; distinguish it from its view. |
| Shader Resource View (SRV) | 着色器资源视图 | A view used by a shader to read a resource; it is not the pixel allocation itself. |
| Image Decoder | 图片解码器 | A format adapter that reads metadata and produces pixels under a defined capability contract. |
| Pixel Buffer | 像素缓冲区 | CPU storage with dimensions, format, stride, alpha mode, color description, and ownership. |
| Stride | 行跨度 | Bytes between row starts, including padding; it may exceed packed row bytes. |
| Premultiplied Alpha | 预乘 Alpha | RGB values multiplied by alpha; also specify the transfer/color domain. |
| Tile | 分块 | A bounded region at one resolution level; edge and filter-border pixels require explicit geometry. |
| Tile Pyramid | 分块金字塔 | Multiple image resolutions divided into Tiles; available levels determine display detail. |
| Cache Eviction | 缓存淘汰 | Removing cache membership; surviving references can keep the allocation alive. |
| Resource Retirement | 资源退役 | Keeping a submitted GPU resource unavailable for reuse until its in-flight work completes. |
| First Build | 首次构建 | Building derived preview/Tiles without a valid disk cache; separate from later cache hits. |
| Displayed | 已显示事件 | A trace/tool event for a presented frame reaching display; not a physical photon measurement. |

## Project concepts and lifetime

| Concept | What and why | Owner | Creation, update, release; distinction |
|---|---|---|---|
| Render Snapshot | An immutable description of the latest requested view, separating UI state from render work. | UI publishes; Render consumes. | Created on a visible state change, replaced by newer state, released after readers finish. It contains intent, not completed pixels. |
| Generation | A monotonic number marking a domain's state changes, used to reject obsolete work. | The module owning that domain. | Created with the domain, advanced on defined invalidation, discarded with it. Always name the domain; it is not a universal image-content identity. |
| Snapshot Sequence | A Generation for Render Snapshot ordering. | UI. | Starts with the window session, advances when publishing a snapshot, ends at shutdown. It does not identify a file revision. |
| Device Epoch | A Generation for the current D3D11 device lifetime. | Render. | Starts on device creation, changes after replacement, ends when the device is released. All handles from an older epoch become invalid. |
| File Identity | A file identifier where available, with a path fallback where necessary. | Navigation/I/O. | Captured when discovering/opening a source and rechecked as needed. It identifies the file, not immutable contents. |
| Source Revision | A content-validity description using identity, length, modification state, and validation policy. | I/O/Decoder; Cache stores the value. | Captured for a read, revalidated before publishing, invalidated on change. Weak metadata can miss content replacements. |
| Image Key | A cache key for a source revision, page/frame, decoder and pixel policy. | Cache, starting in stage B. | Created from a validated request and retained while referenced. A path alone is insufficient. |
| Color Key | A key for a color transform and its target/policy. | Color conversion/Cache, later work. | Created when a transform is chosen, changed on policy/profile changes, released with derived entries. It does not change the source revision. |
| Tile Key | Image Key plus resolution level, coordinates, geometry, border and derivation version. | Tile Cache, stage C. | Created for a requested Tile; retained with its index/cache entry. A Tile coordinate alone is not unique. |
| Budget Reservation | A token that accounts for an allocation before it happens. | Resource-budget owner. | Reserved before allocation, committed on success, released on failure or the last allocation owner. Multiple references are charged once. |
| In-flight Task | A pending/running task whose output may still be used by multiple requests. | Task scheduler, stage B onward. | Created on a miss, reused for the same key, completed/cancelled once. A stale result must not change the current target. |
| Cache Lease | A claim that a cache generation is actively being built or read. | Disk Cache, stage C. | Acquired before use and released on completion/cancellation; crash recovery validates stale claims. It is not a general file lock guarantee. |
| Tile Builder | The bounded decoder/downsampler that writes a Tile Pyramid in merged Stage C. | Large-image path, stage C. | Starts only for an eligible source; ends on verified completion, cancellation, or failure. It is absent from the ordinary-image path. |

Only introduce version domains required by the approved implementation and its tests. Deferred
concepts are vocabulary for later work, not permission to implement all counters, caches, or processes now.
Use `RenderSnapshot`, `DeviceEpoch`, and similar code spellings only when referring to actual identifiers.
