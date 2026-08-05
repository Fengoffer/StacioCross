# StacioCross — Stacio Windows / Linux 跨平台适配（PoC）

Mac 版（AppKit + Rust Core）的 Win/Linux 适配工作区。共享 Rust Core（StacioCore）三平台复用，
Windows 与 Linux 共用一套 Rust 自绘 UI（egui + wgpu + `alacritty_terminal` 终端内核）。

> **本仓库与 Mac 仓库的关系**：`/Users/mac/Documents/Stacio` 是 Mac 现状的唯一参照源，本仓库只读引用；
> 适配代码不写入 Mac 仓库。进度交接锚点见 `docs/platform/adaptation-handoff.md`（在 Mac 仓库中）。

## 结构

```
crates/
├── term/         # 终端模型（alacritty_terminal 封装）+ wgpu 渲染器
├── app/          # egui 应用壳：三栏工作台 + 终端视图嵌入
└── platform/     # 平台适配层 trait + Windows / Unix 实现
```

## 运行

```bash
cargo run -p stacio-app          # 工作台应用
cargo run -p stacio-app -- --stress   # PoC1 终端高强度输出压测
```

## PoC 目标（2-3 周）

1. 远程终端渲染流畅度（alacritty_terminal + wgpu，高强度输出滚动/重绘流畅）
2. 三栏工作台复杂 UI（侧栏 / 工作区 / Inspector + 文件拖放）
3. Windows 系统集成（文件对话框 / 通知 / 凭据 / URL scheme / 单实例）
