[English](../en/stage-c.md) | [简体中文](stage-c.md)

<!-- doc-id: stage-c; revision: 1; status: implementation-review -->

# Stage C：超大 PNG 路径

Stage A、Stage B 已合并。Stage C 正在本地开发；缓存和渲染器验收通过前，不宣称查看器已支持超大图片。
本轮尚未解码私人 48000 × 32000 PNG。

## C1 行解码原型

`large::stream` 用 `png 0.18.1` 逐行读取非隔行静态 PNG，将 RGB、调色板和灰度输入展开为 8-bit 通道，再把借用的行
转换成预乘 BGRA8，不分配完整位图。初始路径不支持 Adam7、APNG、16-bit 通道、ICC 和 HDR。
CPU 转换对编码值四舍五入，不做线性光颜色处理。

解码器有 64 MiB 尽力限制，调用方另设 4 MiB 行限制。块头预检使用检查过的 64 位偏移，限制单个附加块 1 MiB、合计
8 MiB，并限制块数。PNG 解码验证 CRC 和 Adler 校验，不解释忽略的文本和 ICC 内容。取消在块头与行之间检查，无法抢占
已经阻塞的文件系统读取。这些限制不能证明进程所有分配都有硬上限。

生成的小型 RGBA 图片逐行结果与 WIC 对比，预乘舍入差异不超过一个通道值。CRC 损坏、截断、超量元数据、取消和算术
限制测试通过。显式 Release 诊断分别用独立进程扫描生成的 8192 × 256、8192 × 8192 PNG：两者输入与输出行容量均为
65,536 字节，峰值工作集均为 10,022,912 字节；扫描分别耗时 13.698 ms、422.538 ms。这只验证高度增加 32 倍的样本，
不覆盖所有 PNG 编码和异常输入分配模式，没有读取私人极限图片。

## 依赖与依据

`png 0.18.1` 和 `crc32fast 1.5.2` 均采用 MIT OR Apache-2.0。纯 Rust 解码无需部署独立 libpng DLL，且提供逐行 API；
普通 WIC 路径不承诺有界流式读取。image-rs 仓库持续发布变更，并说明有模糊测试；这不等于对 Pixstride 做过安全审计。
没有启用 nightly 特性或使用外部 AI 服务。

参见 [Reader API](https://docs.rs/png/0.18.1/png/struct.Reader.html)、
[Decoder 限制](https://docs.rs/png/0.18.1/png/struct.Decoder.html)和
[维护仓库](https://github.com/image-rs/image-png)。

将 `PIXSTRIDE_TEST_ROOT` 设为仓库外目录后运行显式内存诊断：

```powershell
cargo test --release --locked release_streaming_memory_probe -- --ignored --nocapture
```

C2 必须先验证有界磁盘金字塔，再做 C3 集成。C4 还要检查原图驻留状态、可用空间；首次生成超过 2 GiB 临时缓存前，
必须得到负责人明确批准。
