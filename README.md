# rust-ime

Linux 版「豆包输入法」的 Rust + Tauri 重实现。

从 `豆包输入法.apk`（`com.bytedance.android.doubaoime`，字节内部 **wave-ime** 框架的 Android 移植）逆向出核心架构后，用 Rust 重写引擎、Tauri 做候选窗 UI、XIM/XTest 做 Linux 输入法接入。

## 逆向结论（详见 `../REA_FINDINGS.md`）

豆包输入法 = **原生 C++ 引擎 + 平台桥接**，不是 WebView 壳：
- 核心 `libkeyboard.so`（`keyboard::` 命名空间，2173 个导出符号）：拼音引擎 + 云转换 + 豆包 LLM + 纠错 + 翻译 + 键盘 UI。
- 平台被抽成接口：`libime_ui_android_platform.so` 提供 `GetWindow/GetFont/GetImage/GetTimer/GetAssets`；网络走 `android_host_network_bridge`（宿主桥）。
- Android 壳 `ImeService` 通过 `KeyboardJni`（215 个 native 方法）驱动引擎，用 `InputConnection.commitText/setComposingText/setSelection` 提交文本。
- 云端 `https://ime.doubao.com`：`/cloud/convert`、`/cloud/wordasso`、`/llm`、`/translate`、`/sync/*`。

## 本仓库的取舍

- **引擎用 Rust 原生重实现**（不 FFI 原 C++ 二进制）：拼音 26 键、本地词库候选、用户词库学习、候选选择/提交。云/LLM/语音/手写/翻译为 MVP 之后。
- **XIM + XTest** 作为 IM 框架接入层（标准 X11 IM 协议，兼容 fcitx5/ibus/GTK/Qt 客户端）。
- **Tauri** 做候选窗（webview，跟随光标、透明置顶）。

## 布局

```
rust-ime/
├── Cargo.toml                 # workspace
├── Makefile                   # 开发 / 质量门 / CI 镜像 / 打包
├── .github/workflows/ci.yml   # CI: test + clippy + release build
├── crates/
│   ├── ime-engine/            # 纯 Rust 引擎（无外部依赖，可离线编译）
│   │   ├── data/dict.txt      #   5500+ 词条（拼音 + 权重）
│   │   └── src/
│   │       ├── pinyin.rs      #   412 拼音音节 + 字符串分段（DP，长音节优先）
│   │       ├── candidate.rs   #   trie + 前缀召回 + 分段候选 + 用户词库加权
│   │       ├── engine.rs      #   Engine 状态机（feed_key/commit/finish/reset）
│   │       └── userdict.rs    #   用户词库持久化 + 学习
│   └── ime-fcitx5/            # X11 IM 框架集成
│       └── src/
│           ├── lib.rs         #   ImeServer（线程安全引擎 + XIC 管理）
│           ├── xim.rs         #   XIM FnCommitString / Preedit / Reset 属性投递
│           ├── commit.rs      #   XTest 合成按键 fallback
│           ├── cursor.rs      #   XQueryPointer 光标位置
│           └── x11_keys.rs    #   keysym → Key 转换
└── app/                       # Tauri 候选窗
    ├── src/main.rs            #   Tauri 命令 + X11 Display 管理 + XIM commit
    ├── index.html             #   候选窗前端（透明窗口，键盘监听）
    └── tauri.conf.json
```

## 构建

```bash
# 快速迭代（格式化 + 测试 + clippy）
make dev

# 完整 CI 镜像（test + lint + release build）
make ci

# 仅引擎测试（无外部依赖）
cargo test -p ime-engine

# Tauri 应用
make run
```

### 系统依赖

```bash
# Tauri 前端
sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev libglib2.0-dev

# X11 IM（XIM commit + XTest）
sudo apt install libx11-dev libxext-dev libxtest-dev

# 可选：fcitx5 开发头（未来接 fcitx5 addon API）
sudo apt install libfcitx5-dev
```

## MVP 能力

| 能力 | 状态 |
|------|------|
| 拼音 26 键输入 → 候选 | ✅ |
| 长音节优先消歧（xian→先/闲） | ✅ |
| 前缀召回（niha → 你好） | ✅ |
| 用户词库学习（提交后加权 + 持久化） | ✅ |
| 5500+ 词条覆盖常用汉字/词语 | ✅ |
| XIM 原生 commit（GTK/Qt/fcitx5 客户端） | ✅ |
| XTest fallback（ASCII 场景） | ✅ |
| 候选窗竖排，最多 10 个候选（带序号） | ✅ |
| 上下文预测（提交后预测下一个词） | ✅ |
| 光标定位候选窗（跟随鼠标 +8/+24） | ✅ |
| 全局键盘捕获（XRecord device_events） | ✅ |
| 中英切换（Ctrl+Space） | ✅ |
| 泄露字母自动擦除（提交前退格） | ✅ |
| 单实例锁 | ✅ |
| 开机自启（XDG autostart） | ✅ |
| 云端转换 / LLM 联想 / 语音 / 手写 / 翻译 | ⏳ MVP 后 |

## 使用

```bash
# 直接运行
./target/release/rust-ime-app

# 首次运行会写入 ~/.config/autostart/rust-ime.desktop（开机自启）
# 关闭自启：rm ~/.config/autostart/rust-ime.desktop
```

**按键行为**（已验证于 Xvfb）：

| 操作 | 行为 |
|------|------|
| 输入拼音（a-z） | 候选窗出现在鼠标右下方，显示拼音 + 候选 |
| 空格 / 回车 | 提交首选候选：先退格擦除泄露的拼音字母，再插入汉字 |
| 数字键 1-9 | 选择对应候选 |
| Esc | 取消输入，擦除泄露的拼音字母 |
| Ctrl+Space | 中英切换（终端打印 `IME ON/OFF`） |
| 提交/取消后 | 候选窗自动隐藏 |

**已知限制**：
- 不抓取键盘（`XRecord` 只观察），拼音字母会先进入目标应用，提交时用退格擦除。若应用在收到 Enter 时就提交表单（搜索框），可能出现误触发。可设 `RUSTIME_SUPPRESS=1` 启用实验性的键盘抓取。
- 候选窗定位跟随**鼠标**，不是文本插入点（精确插入点需 AT-SPI）。

## 架构

```
┌─────────────────────────────────────────────────────────────┐
│  Tauri 候选窗 (index.html) 520×96 透明置顶，默认隐藏           │
│  - 监听 "preedit" event → 渲染拼音/候选                       │
│  - 鼠标点击选择候选                                            │
└──────────────────────────┬──────────────────────────────────┘
                           │ tauri events / commands
┌──────────────────────────▼──────────────────────────────────┐
│  app/src/main.rs                                             │
│  - XRecord 全局按键 → 引擎 → 候选                             │
│  - Ctrl+Space 中英切换                                        │
│  - commit: 退格擦除 + xdotool type（UTF-8/CJK）                │
│  - 候选窗 show/hide + 跟随鼠标定位                             │
│  - 单实例锁 + XDG autostart                                   │
└──────────────────────────┬──────────────────────────────────┘
                           │
┌──────────────────────────▼──────────────────────────────────┐
│  ime-fcitx5 (X11 IM layer)                                  │
│  - keygrab.rs: XRecord device_events 全局捕获（8 字节事件头）    │
│  - ImeServer: 线程安全引擎 + XIC 连接管理                      │
│  - xim.rs: FnCommitString / Preedit / Reset 属性投递          │
│  - cursor.rs: 光标位置                                        │
└──────────────────────────┬──────────────────────────────────┘
                           │
┌──────────────────────────▼──────────────────────────────────┐
│  ime-engine (纯 Rust 引擎，无外部依赖)                        │
│  - pinyin.rs: 412 音节 + DP 分段                              │
│  - candidate.rs: trie 前缀召回 + 分段候选 + 用户词库加权        │
│  - engine.rs: 状态机 (feed_key → commit/finish/reset)         │
│  - userdict.rs: JSON 持久化 + 学习                             │
└─────────────────────────────────────────────────────────────┘
```

## 提交文本路径

```
用户选候选 → commit_candidate
  → Engine.commit_candidate(idx) → EngineEvent::Commit("你好")
  → commit_text():
      1. XIM: XSetICValues(FnCommitString) → 客户端 GTK/Qt 插入文字
      2. Fallback: XTestFakeKeyEvent 逐字符发送 (ASCII only)
  → clear_preedit(): XIM_PREEDIT_STRING = ""
```

## Roadmap

- [x] Phase 0: 工程化基础（git, lint, Makefile, CI）
- [x] Phase 1: 音节表修正（去重+补缺失 → 412 音节）
- [x] Phase 2: 词库扩充（85 → 5500+ 词）
- [x] Phase 3: 用户词库持久化（JSON + 自动保存）
- [x] Phase 4: IM 框架集成（XIM commit + XTest + cursor）
- [x] Phase 5: 候选窗 UX 完善（分页 + XIM 原生投递）
- [x] Phase 6: 工程化收尾（Makefile + README + CI）
- [ ] Phase 7: fcitx5 addon 注册（真正出现在 IM 切换列表）
- [ ] Phase 8: Wayland 支持（wlr-input-method protocol）
- [ ] Phase 9: 云端 API 接入（ime.doubao.com）
- [ ] Phase 10: LLM 联想 + 语音输入

## 智能预测

提交一个词后，候选窗会基于上下文预测下一个词：

- **高频搭配表**（内置 bigram，如「你好」→ 吗 / 啊 / 世界）
- **词库前缀补全**（词库里以该词开头的更长的词，只显示剩余部分）

按 `空格/回车` 接受首选预测，`数字键` 选择，`Esc` 关闭预测。

竖排候选最多 10 个，带序号，可鼠标点击选择。

## fcitx5 插件（系统级接入）

```bash
sudo apt install libfcitx5core-dev   # 开发头文件
make addon                           # 构建 librustime.so
sudo ./scripts/install-fcitx5-addon.sh
fcitx5-remote -r                     # 重启 fcitx5
```

安装后在 fcitx5 配置工具的输入法列表中添加 **rust-ime**。
插件由 fcitx5 驱动按键、渲染候选窗并提交文本，不再需要 XRecord/xdotool。

## 设置中心

候选窗右下角齿轮，或启动时加 `--settings` 打开：

```bash
./target/release/rust-ime-app --settings
```

- **通用**：候选数量（5–10）、智能预测、用户词库学习、开机自启
- **外观**：深色/浅色主题、候选字号（实时预览）
- **词库**：系统/用户词条统计、用户词库路径、一键清空
- **关于**：版本与实现信息

配置保存在 `~/.config/rust-ime/config.json`，用户词库在
`~/.local/share/rust-ime/udict.txt`，修改即时生效。
