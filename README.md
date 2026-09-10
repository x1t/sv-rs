# sv-rs — Supervisor 进程控制工具（Rust 版）🚀

`sv-rs` 是 [`github.com/x1t/sv`](https://github.com/x1t/sv) 的 Linux 原生 Rust 重写。
它是 Supervisor 的**短生命周期控制面 CLI**：通过 XML-RPC 或 `supervisorctl` 查询和控制
已经运行的 `supervisord`，自身不作为常驻服务运行。

## ✨ 核心特性

- 🎯 **序号化操作** — 用数字序号代替长进程名，支持单个/多个/范围/混合格式
- 📊 **逐字节对齐输出** — 自定义确定性渲染器、Unicode 直线边框、CJK 宽度对齐
- 🌐 **远程与认证** — 支持 HTTPS 与 Basic 认证的远程 Supervisor
- 🔄 **命令回退** — 默认本地端点连接失败时自动回退 `supervisorctl`
- 🛠️ **智能配置检测** — 自动检测并补齐 Supervisor RPC 配置段，支持原子写入与备份
- 🔐 **静态分发** — `build.sh` 用 zig 产出 musl 全静态二进制，支持 amd64 / arm64

> `supervisord` 才是负责常驻运行、拉起和监控业务进程的服务端。`sv-rs` 不需要 daemon、
> systemd unit 或 SysV init 脚本。

## 🏗️ 项目架构

```
sv-rs/
├── src/
│   ├── main.rs          # 入口
│   ├── cli.rs           # CLI 分发与用法/控制台文案
│   ├── client.rs        # XML-RPC 客户端与 supervisorctl 回退
│   ├── client_map.rs    # RPC struct → ProcessInfo 字段映射
│   ├── config.rs        # 配置检测与自动补齐
│   ├── render.rs        # 确定性表格渲染
│   ├── parse.rs         # 进程数据模型与纯文本解析
│   ├── spec.rs          # 动作/参数校验
│   ├── util.rs          # 命令执行等工具
│   └── xmlrpc.rs        # XML-RPC 编解码
├── tests/cli_e2e.rs     # 端到端 golden 测试
├── tests/golden/        # Go 版输出基线
├── build.sh             # musl 静态发布构建
└── .github/workflows/   # fmt/clippy/test 门禁与静态发布
```

## 🚀 安装

### Cargo 安装

```bash
cargo install --path . --locked
sv-rs --help
```

### 静态发布二进制

```bash
git clone https://github.com/x1t/sv-rs.git && cd sv-rs
cargo install --locked cargo-zigbuild  # 并安装 zig，见 build.sh 顶部说明
./build.sh                             # 默认构建 amd64 + arm64
./build.sh x86_64-unknown-linux-musl   # 或只构建指定目标

sudo install -m 0755 dist/sv-rs-linux-amd64 /usr/local/bin/sv
```

### 从 GitHub Release 安装

安装脚本自动识别 Linux amd64/arm64，从 `x1t/sv-rs` 下载最新正式 Release，
将二进制安装为 `/usr/local/bin/sv`。两个版本使用相同安装路径，请选择其中一个。

```sh
curl -fsSL https://raw.githubusercontent.com/x1t/sv-rs/refs/heads/main/install.sh | sh
```

指定已发布版本或安装目录：

```sh
curl -fsSL https://raw.githubusercontent.com/x1t/sv-rs/refs/heads/main/install.sh | sh -s -- --version v0.3.0 --install-dir /usr/bin
```

脚本支持 `curl` / `wget` 下载以及 `SV_VERSION`、`SV_INSTALL_DIR` 环境变量。
安装目录须已存在；没有写权限时会尝试 `sudo`，OpenWrt 可直接以 root 执行。
安装只复制二进制，不会启动 Supervisor；安装后使用 `sv --help`。

脚本链接指向 `main` 分支；Release 二进制来自最新正式发布。

## 📖 基本使用

通过安装脚本安装后，以下示例中的 `sv-rs` 请使用 `sv`；Cargo 安装仍使用 `sv-rs`。

```bash
sv-rs status                  # 查看全部进程状态
sv-rs list                    # 同 status
sv-rs restart 1               # 重启序号为 1 的进程
sv-rs stop 2 4 6              # 停止序号 2、4、6
sv-rs start 1-3               # 启动序号 1~3
sv-rs restart nginx redis     # 用进程名操作
sv-rs restart 1 nginx 3-5     # 混合格式
sv-rs init                    # 初始化并启动 Supervisor RPC
sv-rs configure rpc           # 检测并补齐 RPC 配置
sv-rs configure rpc --dry-run # 预览配置变更
```

`init/status/start/stop/restart` 都是一次性控制命令。它们连接或初始化 Supervisor RPC
服务端，命令完成后进程退出，不需要由 systemd、SysV 或 OpenWrt init 管理 `sv-rs`。

## 🔧 Supervisor 配置

```bash
export SUPERVISOR_HOST="http://localhost:9001/RPC2"
export SUPERVISOR_HOST="https://user:pass@host:9001/RPC2"
export SUPERVISOR_USER="user"
export SUPERVISOR_PASSWORD="pass"
export SUPERVISOR_CONFIG="/etc/supervisor/supervisord.conf"
export SUPERVISOR_TIMEOUT="300"
```

也可以预览并应用本地 RPC 配置：

```bash
sv-rs configure rpc --dry-run
sudo sv-rs init
```

Supervisor 的常驻服务仍应由系统原有的 `supervisord.service`、`supervisor.service` 或
对应的 SysV/OpenWrt 服务管理。`sv-rs init` 只会初始化 RPC 配置并重启这些 Supervisor 服务。

## 🔄 Supervisor初始化

`sv-rs init` 是一次性初始化命令：它补齐本地配置中的 RPC listener 和标准 RPC 接口，
调用系统已有的 Supervisor 服务使配置生效，然后退出。systemd、SysV 或 OpenWrt init/procd
只负责 `supervisord`，不负责 `sv-rs`。

## 🧪 测试

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```

golden 测试会启动真实本地 HTTP XML-RPC 服务端，把 `sv-rs` 当作真实进程运行，再将 stdout /
stderr 与 `tests/golden/` 下 Go 版输出逐字节比对。

## 📦 发布

推送 `v*` 标签会触发 CI：先执行格式、clippy 和全量测试，再用 zig 构建两个 musl 静态二进制
并挂到 GitHub Release。也可以在 GitHub Actions 页面手动触发构建。
