# 对比 · DuiBi

**本地优先的文本对比与合并桌面工具。**
用 Rust + egui 编写，单文件 exe，无需运行时，**所有内容永不上传**。

> A local-first text compare & merge tool. Everything happens on your machine —
> there is no network code anywhere in the binary.

---

## 为什么不是网页版

| | 网页版 | DuiBi |
|---|---|---|
| 隐私 | 文本要发到服务器 | 全部本地处理，无任何网络请求 |
| 大文件 | 几千行就开始卡 | 10 万行实时对比（实测 76 ms） |
| 对齐 | 逐行硬对齐 | 相似度智能对齐，改过的行正对着原行 |
| 编辑 | 只能贴文本 | 完整编辑器：撤销/重做、查找替换、语法高亮 |
| 合并 | 无 | 逐块合并箭头 + 一键全量合并 |
| 离线 | 不行 | 随时可用 |

---

## 功能

**对比**
- 输入即对比（去抖动，大文件自动延长间隔）
- 行级 / **词级** / 字符级差异高亮，可随时切换
- Patience / Myers / LCS 三种算法
- **智能对齐**：修改块内按相似度配对行，而不是简单按位置对齐
- 可忽略：大小写、行首尾空白、空白数量、全部空白、空行
- 统计：新增 / 删除 / 修改行数 + 相似度百分比

**编辑**
- 双栏虚拟滚动编辑器，10 万行不卡
- 行号、变更条、当前行高亮、空白字符显示
- 撤销 / 重做（左右**独立**，连续输入合并为一步）
- 查找 / 替换：字面量 / 全字 / **正则**（支持 `$1` 捕获组），高亮全部匹配
- 清理工具 15 种：去重、排序、去空行、去首尾空白、换行转空格…
  菜单顶部先选「应用到左侧 / 右侧」，然后单击工具即可；**鼠标停留 2 秒**会
  弹出该工具的具体说明和示例
- 自动换行、可调字号与行高、可拖动的左右分栏

**合并**
- 中间栏每个差异块两个箭头：`>` 推到右侧，`<` 拉到左侧
- 全部合并到左 / 右（**一次 Ctrl+Z 即可整体撤销**）
- 三路合并已预留扩展点（见 `src/core/merge.rs`）

**单文件模式**
- 视图 → 勾选「单文件内容」，界面变成单个编辑框，当普通文本编辑器用
- 对比 / 合并菜单、差异导航、概览条、差异统计全部隐藏，只留编辑相关功能
- 取消勾选即回到左右对比，**另一侧的内容原样保留**

**其他**
- 深色 / 浅色 / **跟随系统**主题
- 中文 / English 界面切换
- 语法高亮（Rust、Python、JS、JSON、Markdown、YAML、HTML、C++、Go、SQL… 100+ 种）
- 编码自动检测（UTF-8 / UTF-16 / **GBK** / Big5 / Shift_JIS…），保存时原样写回，CRLF/LF 不被改写
- 导出 unified diff（`patch` / `git apply` 可直接使用）
- 最近打开文件、右侧差异概览条
- 拖放文件：拖到哪一半就打开在哪一半（悬停时会高亮目标侧）；一次拖两个文件则
  自动分别填入左右
- 完整快捷键（按 <kbd>F1</kbd> 查看）

---

## 快速开始

```bash
git clone <repo> && cd duibi
cargo run --release
```

也可以直接指定两个文件：

```bash
cargo run --release -- old.txt new.txt
# 或安装后
duibi old.txt new.txt
```

**作为 git difftool：**

```bash
git config --global diff.tool duibi
git config --global difftool.duibi.cmd 'duibi "$LOCAL" "$REMOTE"'
git difftool
```

---

## 快捷键

| 按键 | 作用 |
|---|---|
| <kbd>Ctrl</kbd>+<kbd>O</kbd> | 打开文件 |
| <kbd>Ctrl</kbd>+<kbd>S</kbd> | 保存当前侧 |
| <kbd>Ctrl</kbd>+<kbd>F</kbd> / <kbd>Ctrl</kbd>+<kbd>H</kbd> | 查找 / 替换 |
| <kbd>Ctrl</kbd>+<kbd>Z</kbd> / <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>Z</kbd> | 撤销 / 重做 |
| <kbd>Ctrl</kbd>+<kbd>A</kbd> | 全选 |
| <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>X</kbd> | 交换左右 |
| <kbd>F3</kbd> / <kbd>Ctrl</kbd>+<kbd>D</kbd> | 下一处差异 |
| <kbd>Shift</kbd>+<kbd>F3</kbd> | 上一处差异 |
| <kbd>Ctrl</kbd>+<kbd>+</kbd> / <kbd>-</kbd> / <kbd>0</kbd> | 字号放大 / 缩小 / 重置 |
| <kbd>Tab</kbd> / <kbd>Shift</kbd>+<kbd>Tab</kbd> | 缩进 / 反缩进选中行 |
| <kbd>F1</kbd> | 快捷键一览 |
| <kbd>Esc</kbd> | 关闭查找栏 |

---

## 构建与打包

### 依赖

只需要 Rust 1.90+（`rust-version` 已在 `Cargo.toml` 中声明）。
**不需要** Node、Python、C 编译器或 oniguruma —— syntect 使用纯 Rust 的
`regex-fancy` 后端。

### Windows（主要目标）

```powershell
cargo build --release
# 产物：target\release\duibi.exe   （单文件，可直接分发）
```

想要图标：把 `.ico` 放到 `assets/icon.ico`，重新构建即可（`build.rs` 会自动
嵌入图标与版本信息；文件不存在时静默跳过）。

生成安装包（可选，需要 [WiX](https://wixtoolset.org/) 或 Inno Setup）：

```powershell
cargo install cargo-wix
cargo wix --nocapture        # 产出 .msi
```

### macOS / Linux

代码本身跨平台（eframe/egui + glow）。

```bash
# Linux 先装开发库
sudo apt install libgtk-3-dev libxcb-render0-dev libxcb-shape0-dev \
                 libxcb-xfixes0-dev libxkbcommon-dev libssl-dev

cargo build --release

# macOS .app / .dmg
cargo install cargo-bundle && cargo bundle --release

# Linux AppImage
cargo install cargo-appimage && cargo appimage
```

> 注意：需求文档指定主目标为 Windows，上面两条路径可编译但未在本机验证过。

### 发布体积

`Cargo.toml` 的 release 配置已开启 `lto`、`codegen-units = 1`、`panic = "abort"`
与 `strip`。

---

## 测试

```bash
cargo test                       # 单元 + 集成测试
cargo test --release --test performance -- --nocapture   # 性能门槛
```

**实测数据**（Intel UHD Graphics，release 构建）：

| 场景 | release | debug |
|---|---|---|
| 10 万行、100+ 处分散修改 | **76 ms** | 308 ms |
| 10 万行完全相同 | **4.8 ms** | 17 ms |
| 在第 95,000 行连续输入 200 次 | **297 µs** | 3.4 ms |
| 同上但在第 1 行 | 312 µs | 2.6 ms |
| 6 万行病态输入（无公共行） | **7.7 ms** | 40 ms |

第三、四行是关键：**在第 9 万行输入和在第 1 行一样快**（差异在噪声范围内），
这正是 rope + 增量行缓存要达到的效果。

对比是去抖动的：默认停止输入 90 ms 后开始，超过 2 万行时延长到 280 ms；
单次对比还有 250 ms 的时间预算，超时会降级成较粗的结果而不是卡住界面。

### 界面截图工具

OpenGL 窗口无法用 BitBlt / PrintWindow 截图（只会得到白屏），所以仓库里带了一个
让 egui 自己截图的小工具：

```bash
cargo run --bin shot -- out.bmp samples/before.rs samples/after.rs
```

---

## 架构

术语表见 [CONTEXT.md](CONTEXT.md) —— row / line / side / filler / hunk 这些词在
代码和讨论中含义固定，改动前建议先扫一眼。


```
src/
├── core/                  纯逻辑，不依赖 GUI，可独立测试
│   ├── diff/
│   │   ├── options.rs     对比选项（算法、粒度、忽略规则）
│   │   ├── engine.rs      行级对比 → 对齐行表 + 差异块  ★核心
│   │   ├── similarity.rs  Sørensen–Dice 相似度（智能对齐用）
│   │   ├── inline.rs      词级 / 字符级行内差异
│   │   └── unified.rs     unified diff 导出
│   ├── text/
│   │   ├── buffer.rs      ropey + 增量行缓存  ★核心
│   │   ├── history.rs     撤销/重做，输入自动合并
│   │   ├── encoding.rs    编码检测与原样回写
│   │   ├── cleanup.rs     15 种清理工具
│   │   └── search.rs      查找/替换（正则）
│   └── merge.rs           合并补丁（三路合并预留位）
├── ui/
│   ├── theme.rs           两套手调配色 + 对比度测试
│   ├── editor.rs          虚拟滚动双栏编辑器  ★核心
│   ├── rowlayout.rs       Fenwick 树支撑的变高行布局
│   ├── highlight.rs       syntect + 检查点，支持随机跳转
│   ├── gutter.rs          中间合并栏
│   ├── overview.rs        右侧差异概览条
│   ├── find.rs            查找栏
│   ├── editing.rs         光标移动 / 选区（纯逻辑，可测）
│   ├── tabs.rs            制表符展开与坐标映射
│   ├── toolbar.rs         菜单与工具栏
│   └── statusbar.rs       状态栏
├── app.rs                 状态与帧循环
├── config.rs              偏好持久化
└── i18n.rs                中英文文案
```

### 三个值得说明的设计

**1. 对齐行表（`core/diff/engine.rs`）**
两栏共享同一份 `Vec<DiffRow>`。第 `i` 行左边显示 `rows[i].left()`，右边显示
`rows[i].right()`，任一侧可以为空——这就是插入/删除处出现空白填充的原理，
也是两栏能严格对齐的原因。每个 `DiffRow` 只占 12 字节，30 万行约 3.6 MB。

**2. rope + 行缓存（`core/text/buffer.rs`）**
`ropey::Rope` 是唯一真相来源，提供 O(log n) 的字符/行换算；同时维护一份
`Vec<String>` 供对比引擎和渲染器直接借用。每次编辑只重建**受影响的那几行**，
所以在第 9 万行输入和在第 3 行一样快。代价是文本在内存中存了两份。

**3. 带检查点的语法高亮（`ui/highlight.rs`）**
syntect 是顺序解析器，要知道第 9 万行的状态就得先解析前面 9 万行——这与虚拟
滚动天然冲突。做法是每 512 行存一份解析器快照，绘制某个窗口时"回退到最近的
检查点，最多重解析 512 行"。构建检查点本身按帧限额摊销，所以跳到大文件末尾
时文字立即出现、随后几帧内上色，而不是卡住。编辑只作废**编辑点之后**的检查点。

---

## 配置文件

```
Windows: %APPDATA%\DuiBi\config\config.json
Linux:   ~/.config/DuiBi/config.json
macOS:   ~/Library/Application Support/DuiBi/config.json
```

只存偏好设置和最近打开文件的**路径**，不存任何文本内容。
文件损坏或字段缺失会回退到默认值，不会导致启动失败。

---

## 已知限制

- **制表符行不做语法高亮的分段着色**：制表符展开会打乱字节偏移，这类行按
  单色渲染（`ui/editor.rs::build_galley` 有说明）。
- **自动换行时滚动条是估算值**：行高需要实际排版才能知道，未测量的行先按一行
  计算，滚动过程中逐步收敛——与其他编辑器处理大文件换行的方式一致。
- **超过 20 万行不做语法高亮**：此时基本都是机器生成文件，收益不抵成本。
- **UTF-16 文件另存为 UTF-8**：`encoding_rs` 不支持编码到 UTF-16。
- **自动换行时上下键仍按逻辑行移动**，不是按屏幕上的可视行（光标滚动跟随不受影响）。
- macOS / Linux 未在本机验证。

## 环境变量

- `DUIBI_CONFIG` —— 指定配置文件路径，用于自动化测试时不污染真实设置。

---

## 许可

MIT
