# sv-rs — Supervisor 进程管理工具(Rust 版)🚀

`sv`(Go 版 [`github.com/x1t/sv`](https://github.com/x1t/sv))的 Linux 原生 Rust 重写。
单文件、无运行时依赖、**输出与 Go 版逐字节对齐**(golden 测试锁定),可在任意 Linux
主机直接运行,也可替换原 `sv` 使用。

## ✨ 核心特性

- 🎯 **序号化操作** — 用数字序号代替长进程名,支持单个/多个/范围/混合格式
- 📊 **逐字节对齐输出** — 自定义确定性渲染器,Unicode 直线边框、CJK 宽度对齐、
  彩色状态与图标,与 Go 版输出完全一致(golden 测试保证)
- 🌐 **远程与认证** — 支持 HTTPS 与 Basic 认证的远程 Supervisor(RPC 优先)
- 🔄 **命令回退** — 默认本地端点连接失败时自动回退 `supervisorctl`
- 🛠️ **智能配置检测** — 自动检测并补齐 Supervisor RPC 配置段(原子写入 + 备份)
- 🔧 **系统服务集成** — 支持安装为 systemd / SysV 服务并创建命令软链接
- 🔐 **静态分发** — `build.sh` 用 zig 产出 musl 全静态二进制,amd64 / arm64

## 🏗️ 项目架构

```
sv-rs/
├── src/
│   ├── main.rs          # 入口(极简)
│   ├── cli.rs           # CLI 分发与用法/控制台文案
│   ├── client.rs        # XML-RPC 客户端与 supervisorctl 回退
│   ├── client_map.rs    # RPC struct → ProcessInfo 字段映射
│   ├── config.rs        # 配置检测与自动补齐
│   ├── render.rs        # 确定性表格渲染(对齐 golden)
│   ├── parse.rs         # 进程数据模型与纯文本解析
│   ├── service.rs       # Linux 系统服务管理(systemd/SysV)
│   ├── service_links.rs # 命令软链接的创建/归属删除
│   ├── service_files.rs # 服务资产文本、runlevel 链接、信号等待
│   ├── spec.rs          # 动作/参数校验
│   ├── util.rs          # 命令执行等工具
│   └── xmlrpc.rs        # XML-RPC 编解码
├── tests/cli_e2e.rs     # 端到端 golden 测试(真实 HTTP 服务端)
├── tests/golden/        # 由 Go 版捕获的逐字节基线
├── build.sh             # musl 静态发布构建
└── .github/workflows/   # fmt/clippy/test 门禁 + 静态发布
```

## 🚀 快速开始

### 环境要求

- Rust **1.97+**(edition 2024;`linker_messages` lint 依赖新 rustc)
- Supervisor 3.x+(被管理端)

### 安装编译

```bash
# 方式一:静态发布构建(推荐,输出到 dist/,全静态、可在任意 Linux 运行)
git clone https://github.com/x1t/sv-rs.git && cd sv-rs
cargo install --locked cargo-zigbuild   # 并安装 zig,见 build.sh 顶部说明
./build.sh                              # 默认构建 amd64 + arm64
./build.sh x86_64-unknown-linux-musl    # 或只构建指定目标

# 方式二:本地直接跑
cargo build --release
./target/release/sv-rs --help
```

> 作为 `sv` 的替代品使用时,软链接或改名即可,例如
> `sudo ln -s "$(pwd)/dist/sv-rs-linux-amd64" /usr/local/bin/sv`(x86_64 机器);
> arm64 机器用 `dist/sv-rs-linux-arm64`。

### 基本使用

```bash
./sv-rs status                  # 查看全部进程状态(带序号、完美对齐表格)
./sv-rs list                    # 同 status
./sv-rs restart 1               # 重启序号为 1 的进程
./sv-rs stop 2 4 6              # 停止序号 2、4、6
./sv-rs start 1-3               # 启动序号 1~3
./sv-rs restart nginx redis     # 用进程名操作
./sv-rs restart 1 nginx 3-5     # 混合格式
sudo ./sv-rs service install    # 安装为系统服务并创建软链接
./sv-rs configure rpc           # 检测并补齐 RPC 配置(带备份);加 --dry-run 预览
```

### 环境配置

```bash
export SUPERVISOR_HOST="http://localhost:9001/RPC2"      # 默认即此
export SUPERVISOR_HOST="https://user:pass@host:9001/RPC2" # 远程 + HTTPS 认证
export SUPERVISOR_USER="user"       # 或拆分设置用户名/密码
export SUPERVISOR_PASSWORD="pass"
export SUPERVISOR_CONFIG="/etc/supervisord.conf"          # 配置文件路径

# 同步控制操作超时(秒,可选):start/stop/restart 默认 120s,以等待 supervisor
# 完成优雅停止与启动判定;慢服务可调大,查询 status 仍为 10s,不受影响
export SUPERVISOR_TIMEOUT="300"
```

## 🧪 测试

```bash
cargo fmt --all -- --check   # 格式检查
cargo clippy --all-targets -- -D warnings   # 零警告
cargo test                   # 103 项:90 单元 + 13 golden 端到端(真实 HTTP 服务端)
```

golden 测试把编译产物作为真实进程运行,喂以真实本地 HTTP XML-RPC 服务端,
再将 stdout / stderr 与 `tests/golden/` 下 Go 版真实输出做逐字节比对——
任何渲染差异都会导致测试失败。

## 📦 发布

打 `v*` 标签推送到 GitHub 即触发 CI:先跑质量门禁,再用 zig 构建两个 musl 静态
二进制并挂到 Release(可在 GitHub Actions 页面 `workflow_dispatch` 手动触发构建)。
