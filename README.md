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
| 候选窗分页（9 条/页，翻页） | ✅ |
| 光标定位候选窗 | ✅ |
| 云端转换 / LLM 联想 / 语音 / 手写 / 翻译 | ⏳ MVP 后 |

## 架构

```
┌─────────────────────────────────────────────────────────────┐
│  Tauri 候选窗 (index.html)                                   │
│  - 透明置顶 webview，跟随光标                                 │
│  - 键盘事件 → invoke("feed_key")                              │
│  - 监听 "preedit" event → 渲染候选                           │
└──────────────────────────┬──────────────────────────────────┘
                           │ tauri commands
┌──────────────────────────▼──────────────────────────────────┐
│  app/src/main.rs (Rust backend)                              │
│  - DisplayHandle: X11 Display 生命周期                        │
│  - commit_text: XIM first → XTest fallback                   │
│  - page_next/page_prev: 候选翻页                              │
└──────────────────────────┬──────────────────────────────────┘
                           │
┌──────────────────────────▼──────────────────────────────────┐
│  ime-fcitx5 (X11 IM layer)                                  │
│  - ImeServer: 线程安全引擎 + XIC 连接管理                      │
│  - xim.rs: FnCommitString / Preedit / Reset 属性投递          │
│  - commit.rs: XTest 合成按键                                  │
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
