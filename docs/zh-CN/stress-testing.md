[English](../en/stress-testing.md) | [简体中文](stress-testing.md)

<!-- doc-id: stress; revision: 2; status: proposal -->

# 跨格式压力测试计划

本计划覆盖兼容性、大型图片、资源耗尽和异常输入。列入计划不代表已支持或已测试。
现有极限样本只有一张私人非隔行 RGBA8 PNG：48000 × 32000 像素，文件长度为
2,071,102,933 字节。此前仅检查本地驻留状态和 33 字节文件头，没有完整解码。

## 分别记录四层限制

每个用例分别记录下列限制，通过某一层不能证明能通过下一层。

| 层级 | 需要记录的内容 | 示例 |
|---|---|---|
| 格式规范 | 规范版本、profile/子格式、字段约束、有效尺寸和帧语义。 | PNG 宽高上限为 2^31−1，不代表实际能完成这种尺寸的解码。 |
| 解码器实现 | 精确库版本和构建选项、尺寸/像素/元数据限制、工作缓冲、取消及区域解码成本。 | 库可以在低于格式上限时拒绝一个有效文件。 |
| Windows/GPU | WIC codec 可用性、API 参数位宽、D3D Feature Level、纹理限制和实际分配失败。 | FL 11.x 的 Texture2D 单边上限为 16384；WIC `CopyPixels` 的行跨度和缓冲长度为 `UINT`。 |
| Pixstride 策略 | CPU/GPU/磁盘预算、并发、支持能力及预期降级或错误。 | 有效文件可能因超预算被拒绝，不能因此标为文件损坏。 |

依据：[PNG](https://www.w3.org/TR/png-3/)、
[D3D11 限制](https://learn.microsoft.com/en-us/windows/desktop/direct3d11/overviews-direct3d-11-resources-limits)、
[WIC CopyPixels](https://learn.microsoft.com/en-us/windows/win32/api/wincodec/nf-wincodec-iwicbitmapsource-copypixels)。
转换尺寸、行跨度、偏移和分配长度前，先使用 checked 64-bit 运算。

## 测试级别与结果

| 级别 | 范围 | 必须观察的结果 |
|---|---|---|
| L1 | 小型有效文件、颜色、方向、Alpha，以及适用的多页和动画。 | 与独立参考结果一致，元数据有界。 |
| L2 | 格式实际允许范围内的 8K/16K 级图片、高压缩率、大文件和特殊宽高比。 | 文件头路由、解码/转换/上传耗时、进程和自有内存峰值、交互响应。 |
| L3 | 极端尺寸、高解码内存、长动画/多页、分块/网格布局和磁盘缓存复用。 | 有界资源、预览/区域可用性、取消、重开和清理；缺少能力时受控拒绝也是有效结果。 |
| L4 | 截断、损坏字段/偏移/数据块、溢出、分配失败、磁盘满和进程/设备故障。 | 结构化错误，无过期图片回写或越界访问，预留与句柄正确回收。 |

8K/16K 表示测试类别，不要求编码器生成非法尺寸。格式或库的限制较低时，测试边界前、边界值
和边界后，分别规定接受或拒绝结果。所有级别都适用资源故障测试，L4 将其独立列出以便复现。

## 格式矩阵

| 格式 | L1 | L2 | L3 | L4 |
|---|---|---|---|---|
| JPEG | Baseline/progressive、灰度、CMYK、EXIF 方向和 ICC。 | 大型 baseline/progressive、高低熵、原生缩小输出。 | 接近所选解码器的尺寸上限、长图扫描、渐进式系数内存和取消。 | 截断扫描、非法 marker 长度/尺寸、恶意元数据和预算不足。 |
| PNG | Palette/tRNS、灰度、RGBA、8/16-bit、Adam7。 | 8K/16K、极高压缩率、宽扫描行；比较隔行与非隔行。 | 私人 48000 × 32000 样本，首次构建、分块金字塔、100% 局部和磁盘缓存命中。 | CRC/IDAT 截断、非法过滤器/尺寸、附加块膨胀、磁盘满和取消构建。 |
| GIF | 调色板/透明、偏移、延时、disposal 和循环。 | 大画布加小子帧、极端时长。 | 大量帧、previous-disposal 快照、有界画布/帧缓存和按时间跳帧。 | 非法 LZW/矩形、子块截断、越界范围和分配失败。 |
| TIFF | Strip/tile、多页、方向、位深和支持的压缩方式。 | 大 strip/tile、多 IFD 条目，分别测 classic TIFF 和 BigTIFF。 | 超过 4 GiB 的独立合成 BigTIFF、超大分块页、多页、实际区域读取。 | 偏移溢出/循环、非法计数、重叠/文件外范围、不支持的 codec。 |
| WebP | VP8/VP8L、Alpha/ICC、动画混合和 disposal。 | 接近编码/库限制、高压缩率、支持范围内的大动画画布。 | 大量帧、画布/子帧不一致、增量解码内存和取消。 | RIFF/chunk 长度错误、帧越界、截断和预算耗尽。 |
| AVIF | 支持的 profile、色度采样、高位深、Alpha/ICC/NCLX 和序列。 | 大图，分别计算 YUV plane 和 RGB 转换占用。 | 固定后端支持的 grid/layer/sequence，参考帧和工作进程内存。 | 非法 box、尺寸/辅助图不一致、缺失数据、线程/内存耗尽。 |
| HEIC/HEIF | 支持的 HEVC profile、方向、ICC/NCLX 和辅助图。 | 分别测试单幅编码图和 grid 派生图。 | 多 grid cell、适配器支持时的可见 cell 访问、后端/线程限制。 | 非法 item 引用/grid 范围、循环、HEVC 截断、不支持的 profile 和资源耗尽。 |
| JPEG XL | 无损/有损、支持的位深、方向、ICC 和动画。 | Modular/VarDCT、可选渐进输出、极端宽高比。 | 支持的 level/尺寸边界、多帧、回调输出和解码器工作内存。 | 非法码流/box、尺寸不一致、截断、runner/内存限制。 |
| RAW | 获准使用的相机样本或合规 DNG；嵌入预览和完整解码分开。 | 传感器尺寸、CFA/位深、去马赛克峰值缓冲。 | 超大 DNG/获准拍摄样本、预览降级、有界工作进程。 | 厂商/容器偏移、缩略图损坏、解压限制、不支持相机的明确错误。 |
| PSD/PSB | 合成预览、颜色模式、位深、Alpha 和压缩方式。 | 大型合成图、多图层，不承诺图层编辑。 | PSB/超大声明画布、支持时的合成图选择性访问，按预算拒绝。 | 图层长度/计数、递归区段、RLE/ZIP 截断和恶意尺寸。 |

## 不混用不同格式的限制

| 格式 | 需要区分的规范边界 | 生成前核实的实现限制 |
|---|---|---|
| JPEG | 常见 JPEG 帧尺寸使用 16-bit 字段，其他 JPEG 家族或 profile 应单独核实。 | libjpeg-turbo 当前 `JPEG_MAX_DIMENSION` 为 65500，小于无符号 16-bit 最大值；复核固定 tag 和 WIC 各自的限制。 |
| PNG | 宽高为 1 至 2^31−1；图像的 IDAT 数据组成同一 zlib 流。 | 扫描行/Adam7 缓冲、像素策略、元数据膨胀和库限制，不承诺随机矩形读取。 |
| GIF | 逻辑画布和图像尺寸为 16-bit 字段；帧和合成语义另行处理。 | 限制画布字节、解码帧存储和总工作量，不能只限制压缩字节。 |
| TIFF | Classic TIFF 使用 32-bit 偏移，BigTIFF 使用 64-bit 偏移，不代表像素限制相同或普遍支持 BigTIFF。 | 记录 BigTIFF 能力、tag 类型、strip/tile 布局和解压工作集。 |
| WebP | VP8L 可表示至 16384；VP8 和 VP8X 的字段、约束不同；容器大小上限为 4 GiB 减 2 字节。 | 编码和解码限制可能不同；检查所选版本的 `WEBP_MAX_DIMENSION` 和动画画布限制。 |
| AVIF | AVIF profile、选中的 AV1 帧、`ispe`、grid 和序列约束必须一致。 | 记录后端尺寸/像素限制和 plane/参考帧/RGB 分配，不能从单个 API 整数推断最大值。 |
| HEIC | HEIF 派生几何与 HEVC profile/level 限制不同。 | 记录实际 WIC 扩展或 libheif/后端版本；grid 访问取决于能力。 |
| JPEG XL | 尺寸头和码流 level 有各自约束。 | 声明最大值前固定 libjxl、runner、输出回调行为和解码限制。 |
| RAW | RAW 是相机/厂商格式的集合，没有统一尺寸规范。 | 指明子格式和相机/DNG profile，分别测元数据、预览、unpack 和去马赛克。 |
| PSD/PSB | Adobe 规定 PSD 单边至 30000、PSB 至 300000。 | 合成图解码器可能只实现较小子集；分别测图层/ZIP/RLE 内存。 |

一手资料：[libjpeg-turbo 头文件](https://github.com/libjpeg-turbo/libjpeg-turbo/blob/main/src/jmorecfg.h)、
[GIF89a](https://www.w3.org/Graphics/GIF/spec-gif89a.txt)、
[BigTIFF](https://libtiff.gitlab.io/libtiff/specification/bigtiff.html)、
[WebP 容器](https://developers.google.com/speed/webp/docs/riff_container)、
[VP8L](https://developers.google.com/speed/webp/docs/webp_lossless_bitstream_specification)、
[AVIF](https://aomediacodec.github.io/av1-avif/)、
[libheif 分块](https://github.com/strukturag/libheif/wiki/Reading-and-Writing-Tiled-Images)、
[libjxl](https://github.com/libjxl/libjxl)、[LibRaw](https://www.libraw.org/about)、
[Adobe 格式](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/)。
没有确定最大值的项保留为适配器选定后的调研任务，不根据扩展名推断测试能否通过。

## 私人超大 PNG 测试规程

样本含 1,536,000,000 像素，完整 RGBA8 存储需要 6,144,000,000 字节，约 5.72 GiB。
这是计算值，没有实际分配，也不是峰值实测。公开文件不保留样本路径。
计划用 `PIXSTRIDE_STRESS_IMAGE` 指定本地只读输入，用 `PIXSTRIDE_TEST_ROOT` 指定独立、可清理的
缓存和输出根目录，目前没有读取这些变量的查看器或生成器。

每次测试前，先在不打开数据流的情况下检查云文件驻留。如果读取需要下载，先取得样本所有者同意。
不得改名、修改、复制、上传或提交原图；变异测试使用独立生成的样本。

先验证小型 PNG 与独立参考结果，再按同一轨迹比较 512/1024/2048 分块。
分别记录首次部分预览、完整预览、首个 100% 区域、底部任意区域可用、构建校验完成、
不同进度的取消和关闭重开。1024 分块和 512 MiB 构建预算属于实验输入。
非隔行 PNG 首次构建可能需要全流扫描；有效磁盘缓存重开时可以读取独立存储的分块，两个时序分开报告。

分别记录自有分配、进程私有字节/工作集、OS 缓存、VRAM 和磁盘占用。
注入 CPU/GPU/磁盘失败，只损坏生成的缓存记录，验证源修订检查、无过期结果回写、预算和句柄回收。
清理前将输出解析为绝对路径，确认位于配置的输出根下，并确认没有活动读写租约。

## 独立测试数据生成工具方案

该工具需单独获准后开发。输入为声明式 manifest、尺寸、图案/种子、格式选项、帧/页数和输出根，
不提供输入图片选项。逐行或逐块生成纯色、渐变、棋盘格、透明边缘和确定性噪声。
生产缓冲和编码输出都受预算约束，不为了测试有界解码而先创建完整超大位图。
编码器无法流式生成某个用例时，报告限制或跳过。

manifest 记录用例 ID、L1–L4 级别、预期接受/降级/拒绝结果、生成器和后端版本、种子、
精确尺寸/行跨度/位深/帧依赖、各层限制、预期像素/ROI hash、文件校验、大小和再分发许可。
路径相对于输出根。小型可分发样本需评审后才进入 Git，大型或私人产物留在本地。

异常文件只能通过明确的变异步骤从工具自产样本生成。达到磁盘、CPU 或时间预算就停止，
按用例 manifest 恢复，不能隐藏未完成文件的状态。
合成 DNG 不代表已验证所有厂商 RAW；真实相机样本需要来源和许可。
PSD/PSB 生成器需要独立格式校验。区分编码器拒绝和解码器失败，条件允许时用独立实现比较像素和区域。

## 测量和验收

分别报告进程冷启动、OS 热读、CPU 缓存命中、GPU 缓存命中、磁盘分块命中和首次构建。
记录输入接收/发布、CPU 解码/转换、上传提交、GPU 完成、`Present` 和已显示事件。
`Present` 或 GPU API 返回不能证明屏幕已显示；使用
[GPU timestamp disjoint 检查](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/ns-d3d11-d3d11_query_data_timestamp_disjoint)
并关联呈现事件。物理输入到光子仍需独立硬件测量。

初始目标仍为热启动窗口可见 P95 50 ms、GPU 命中时从输入 handler 开始到快照发布 P95 5 ms，
以及普通输入 handler 自身工作 P95 1 ms。以上均未实测，也不承诺实际屏显时限。
输入长尾分析至少采集 10,000 个事件，样本不足 1000 时将 P99 标为样本不足。
首次构建等昂贵测试逐次报告结果和范围，不用少量运行宣称稳定 P95/P99；保留失败和 trace 丢失记录。

每项 L3 能力应正确显示预览/区域，或明确报告不支持/超预算，同时提供输入响应、
有界队列和资源、取消/故障恢复、清理证据。加入极限路径后，重跑普通图片性能基线。

阶段 C 可执行行解码、缓存和视口诊断见[阶段 C](stage-c.md)。Rust CI 运行生成的小样本，GPU 和私人图片诊断需
显式启用，真实极限图片不得作为 CI 输入。大图缓存预算独立于普通图片单图 256 MiB 限制。
