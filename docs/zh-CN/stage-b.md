[English](../en/stage-b.md) | [简体中文](stage-b.md)

<!-- doc-id: stage-b; revision: 1; status: local-implementation-review -->

# 阶段 B：图片浏览核心

阶段 B 加入了首版本地实现：打开 JPEG、PNG、BMP，浏览当前目录，并通过已有 D3D11 渲染线程显示解码像素。
代码已通过编译和单元检查。只有负责人使用真实 JPEG、PNG、BMP 完成人工验收后，才能确认 Windows 图形界面行为。

## 打开与导航

Pixstride 接受一个可选命令行图片路径，支持 Unicode 和包含空格的路径。按 `Ctrl+O` 打开 Windows 原生文件对话框。
Left 和 Right 在当前目录的支持文件之间导航，到达目录边界时停留在当前图片。按 `F` 选择 Fit to Window，按 `1`
选择原始像素尺寸。Fit 保持宽高比并居中显示。原始像素模式以 1:1 比例居中，超出客户区的部分会被裁切；目前未实现平移。

目录扫描只纳入扩展名为 `.jpg`、`.jpeg`、`.png`、`.bmp` 的普通文件。排序先比较 Unicode 小写文件名，再用原始文件名
打破平局，确保结果确定。目录扫描在图片工作线程执行，不阻塞 Win32 消息循环。

## WIC 与像素所有权

一个有界工作线程初始化 COM MTA，并在该线程创建和使用全部 WIC 对象。线程解码第 0 帧并返回自有字节缓冲区；WIC
COM 接口不会跨线程传递。WIC 将像素转换成 32-bit premultiplied BGRA（`GUID_WICPixelFormat32bppPBGRA`）。若能读取
EXIF Orientation 元数据，则将方向 1–8 映射为 WIC 翻转或旋转。渲染器把像素上传为
`DXGI_FORMAT_B8G8R8A8_UNORM`，并使用 premultiplied-alpha 混合，将透明像素合成到背景色上。

分配 `CopyPixels` 缓冲区前，程序使用溢出检查计算尺寸、行跨度和总字节数。当前默认值为单边不超过 16,384 像素、
单张解码数据不超过 256 MiB、CPU 与 GPU 常驻缓存各 512 MiB。这些值是可调整的代码初值，并非测得的最佳配置。
超过边长或解码字节预算的图片会在分配整张像素缓冲区之前被拒绝。48,000 × 32,000 压力 PNG 不属于此处理路径，
本轮没有打开它。

## 任务调度与缓存

UI 为每次选择变化维护一个单调递增的 `generation`。工作线程最多保存一个待处理的当前图片请求、一次目录扫描和
两个相邻预加载请求。新选择会替换尚未开始的工作；已经进入 WIC 的调用会运行到返回，若其 `generation` 已过期，
结果会丢弃。CPU LRU 由工作线程独占，以规范化路径、文件长度和最后修改时间作为缓存键。该键能识别常见文件替换，
但不是内容哈希。

渲染线程独占 D3D11 Texture LRU 和 Immediate Context。GPU 缓存命中时复用已上传纹理，按宽 × 高 × 4 字节计费，
将较旧的非当前纹理淘汰到 512 MiB 预算以内。CPU 缓存按实际像素缓冲区长度计费。一次解码可能在 CPU 常驻缓存之上
再占用最多 256 MiB；阶段 B 不使用临时磁盘缓存。

## 诊断和限制

退出时，现有有界计时器汇总目录扫描、WIC 打开、格式转换器初始化、`CopyPixels`、成功解码总耗时、纹理上传、渲染线程
请求延迟、首张图片 `Present` 返回，以及 CPU/GPU 缓存命中数和当前/峰值常驻字节数。字节数只统计缓存拥有的条目，不含进程工作内存
或驱动额外分配。文件读取由 WIC 管理，因此当前版本不能将存储延迟与解码工作分开。
`Present` 返回是应用内部测量边界，不代表光子已到达显示器。

自动化测试覆盖路径排序与边界、尺寸和预算检查、EXIF 映射、CPU 缓存淘汰、过期 Generation 替换、视口尺寸，以及通过
WIC 编码并解码运行时生成的 BMP、JPEG 和 PNG。Alpha 显示、文件对话框、加载期间 Resize、快速导航、解码中关闭窗口和真实
GPU 纹理显示仍需人工图形界面验收。Device Lost 恢复、平移、GIF、TIFF、WebP、RAW、AVIF、HEIC、JPEG XL、超大图分块
和磁盘缓存不在本阶段范围内。

在 Windows 10/11 x64、已安装 MSVC 和 Windows SDK 的环境中构建：

```powershell
cargo build --release --locked
cargo run --release -- "<image-path>"
```

不提供路径时，程序使用阶段 A 生成的测试纹理启动。运行 `cargo run --release` 后按 `Ctrl+O` 选择图片。标题栏显示当前路径。
构建检查及当前验证状态见[开发说明](../en/development.md)。
