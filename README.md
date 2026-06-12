# Keybearer

<div align="center">

<b>
  🔒 Keep LLM API keys <b>local</b>. Use them <b>anywhere</b>. 🌐
</b>

_害怕公共服务器上的 API Key 泄露？来使用 Keybearer！_

</div>

## 原理

Keybearer 基于 SSH Agent Forwarding 的协议通道，通过 Seccomp hook 读取，实现安全的 API Key 传输通道。

API Key 永远存储在本地宿主机的 `~/.config/keybearer/config.yaml` 中。运行时本地 `keybearer agent` 通过 SSH agent forwarding 传递 credential query；远端 `keybearer run` 只看到 forwarded `SSH_AUTH_SOCK` 和按需生成的匿名 memfd，不修改远端配置文件、不进入远端 shell 历史。

目标是在公用集群上避免其他人或 root 通过远端文件、环境变量、shell 历史、配置目录直接读取 API Key；注意，root 仍有可能观察或干预进程和内核，因此该边界必须在威胁模型中持续验证。

## 快速上手

1. 克隆本仓库：`git clone https://github.com/HeZeBang/keybearer.git && cd keybearer`
2. 编译：`cargo build --release`
3. 上传至服务器：`scp ./target/release/keybearer your-server:/tmp/keybearer`
4. 准备好 `~/.config/keybearer/config.yaml`（见[配置样例](#配置文件)）
5. 开启临时认证 Agent：`eval "$(keybearer agent)"`
6. SSH（带认证）到服务器：`keybearer ssh your-server` 或 `ssh -A your-server`
7. 运行 LLM CLI：`/tmp/keybearer run codex` / `opencode` / `claude`

### 对比

| 对比维度 | 修改配置文件 | 代理转发 | **KeyBearer** |
|---|---|---|---|
| **基本原理** | 把 Key 写入远端配置文件 | **代理服务器转发并注入 Key** | **在远端运行时按需注入配置** |
| **API Key 物理位置** | 远端服务器磁盘 | **本地代理服务器** | **本地宿主机** |
| **Key 留存形式** | 明文 | **无** | **无持久化（仅运行时匿名 memfd）** |
| **轮换/切换方式** | 手动再次编辑文件，或在 CC Switch UI 中切换 provider 后重新写入远端 | 在代理服务端更新 Key | **修改本地 `config.yaml`，远端立即生效** |
| **写入磁盘** | 是 | **否** | **否** |
| **是否进入远端 shell 历史** | 手动编辑命令可能进入历史；CC Switch 不经过 shell | 代理地址可能进入历史 | **否** |
| **生命周期** | 直到下次修改 | 直到服务器关闭 | **断联即焚** |
| **防窃取** | 差（依赖文件权限，易疏忽） | 较好 | **较强** |
| **侵入性** | 直接修改远端持久配置 | **仅修改代理配置** | **不修改远端持久配置** |
| **网络路径** | 远端 → API | 远端 → 代理 → API | **远端 → API** |
| **典型适用场景** | 单用户本机、临时测试、本机多工具切换、远端有 GUI | 团队共享、固定网络拓扑 | **远程 SSH 服务器、共享集群、多远程主机** |

## 使用说明

### Agent

Keybearer agent 是本地守护进程，管理 credential 并通过 SSH agent protocol 提供给远端。

```bash
# 后台启动（推荐），输出 shell 变量供 eval
eval "$(keybearer agent)"

# 前台启动（调试用）
keybearer agent -D

# 指定 socket 路径
keybearer agent -a /tmp/my-agent.sock --control-sock /tmp/my-control.sock

# 终止 agent（输出 unset 命令供 eval）
eval "$(keybearer agent -k)"
```

启动后会设置以下环境变量：

- `SSH_AUTH_SOCK` — 指向 keybearer agent socket
- `KEYBEARER_CONTROL_SOCK` — 控制通道（用于热重载）
- `KEYBEARER_AGENT_PID` — agent 进程 PID
- `KEYBEARER_UPSTREAM_SSH_AUTH_SOCK` — 原始 SSH agent（如有）

### SSH 连接与 Agent Forwarding

Keybearer 依赖 SSH agent forwarding 将 credential 传递到远端。以下三种方式等价：

```bash
# 方式 1：keybearer ssh（自动加 -A）
keybearer ssh your-server

# 方式 2：手动 ssh -A
ssh -A your-server

# 方式 3：在 ~/.ssh/config 中配置 ForwardAgent（推荐）
ssh your-server
```

`keybearer ssh <host>` 只是 `ssh -A <host>` 的简写，没有额外魔法。对于经常连接的主机，推荐在 `~/.ssh/config` 中永久配置：

```
Host your-server
    ForwardAgent yes
```

这样普通 `ssh your-server` 即可自动转发 agent，无需每次加 `-A` 或使用 `keybearer ssh`。

### 按主机选择 Profile

默认情况下，远端使用 `config.yaml` 中 `defaults` 对应的 profile。如果不同主机需要不同的 provider（例如公司集群用 OpenAI，个人服务器用 DeepSeek），可以通过环境变量 `KEYBEARER_PROFILE_ID` 覆盖默认选择。

在 `~/.ssh/config` 中用 `SetEnv` 为每台主机绑定 profile：

```
Host work-cluster
    HostName cluster.company.com
    ForwardAgent yes
    SetEnv KEYBEARER_PROFILE_ID=openai-work

Host personal
    HostName my.server.com
    ForwardAgent yes
    SetEnv KEYBEARER_PROFILE_ID=deepseek

Host lab
    HostName lab.university.edu
    ForwardAgent yes
    # 不设置 KEYBEARER_PROFILE_ID，使用 defaults 中的 profile
```

> **注意：** 远端 `sshd_config` 需要配置 `AcceptEnv KEYBEARER_PROFILE_ID` 才能接收该变量。如果远端不支持，也可以在远端 shell 中手动 `export KEYBEARER_PROFILE_ID=xxx`。

### Shell Alias

为常用命令设置 alias，省去每次输入 `keybearer run`：

```bash
# ~/.bashrc 或 ~/.zshrc
alias codex='keybearer run codex'
alias opencode='keybearer run opencode'
alias claude='keybearer run claude'
```

### 运行 LLM CLI

在远端（或本地）通过 seccomp 拦截配置文件读取：

```bash
keybearer run codex
keybearer run opencode
keybearer run claude
keybearer run -- any-command --with-args
```

`keybearer run` 会拦截以下文件的 `open`/`openat` 系统调用，注入 key 和配置：

| 文件路径 | 应用 | 模式 |
|---|---|---|
| `~/.codex/auth.json` | Codex | 替换 |
| `~/.codex/config.toml` | Codex | 合并 |
| `~/.config/opencode/opencode.json` | OpenCode | 合并 |
| `~/.claude/settings.json` | Claude Code | 合并 |

**合并模式**会读取远端已有的配置文件，保留用户自定义内容，仅注入/覆盖 keybearer 管理的字段。

### 配置验证与调试

```bash
# 验证配置文件语法和语义
keybearer check

# Dry Run，查看注入后的配置文件内容（不实际拦截）
keybearer dry-run codex
keybearer dry-run opencode
keybearer dry-run claudeCode
keybearer dry-run claudeCode cc    # 指定 profile id
```

### 手动管理 Profile（CLI）

> 这些命令将来可能废弃，推荐直接编辑 `config.yaml`。

```bash
# 添加 profile
keybearer add openai-compatible myprofile sk-xxx \
  --name "My Profile" \
  --base-url https://api.example.com/v1 \
  --app codex --app opencode \
  --model gpt-4o

# 列出所有 profile
keybearer list

# 设置某 app 的默认 profile
keybearer use codex myprofile

# 删除 profile
keybearer remove myprofile
```

## 配置文件

配置路径优先级：

1. `$KEYBEARER_CONFIG_DIR/config.yaml`
2. `$XDG_CONFIG_HOME/keybearer/config.yaml`
3. `~/.config/keybearer/config.yaml`

配置文件保存为 `0600` 权限，目录为 `0700`。

### Schema

添加以下行到配置文件首行即可获得编辑器补全和校验：

```yaml
# yaml-language-server: $schema=https://raw.githubusercontent.com/HeZeBang/keybearer/refs/heads/master/config.schema.json
```

完整 schema 见 [`config.schema.json`](config.schema.json)。

### 结构说明

```yaml
schemaVersion: 1          # 固定为 1

profiles:
  <profile-id>:           # ID 只允许 [A-Za-z0-9._-]
    name: "显示名称"
    providerKind: openai | anthropic | openai-compatible
    apiKey: sk-xxx
    baseUrl: https://...  # openai-compatible 必填，其余可选
    apps:                 # 至少一项
      - codex             # 仅 openai / openai-compatible
      - opencode          # 所有 provider 均可
      - claudeCode        # 仅 anthropic
    appConfig:            # 可选，按 app 配置
      codex:
        model: gpt-5.5                  # 默认 gpt-5.5
        reasoningEffort: high           # minimal | low | medium | high | xhigh
        disableResponseStorage: true    # 默认 true
      opencode:                         # 透传 OpenCode ProviderConfig
        models:
          my-model:
            name: "My Model"
            # 完整 schema 见 https://opencode.ai/config.json
      claudeCode:
        model: claude-sonnet-4-20250514
        haikuModel: claude-haiku-4-5-20251001
        sonnetModel: claude-sonnet-4-20250514
        opusModel: claude-opus-4-20250115
    meta: {}              # 自定义元数据（备注等）

defaults:                 # 每个 app 的默认 profile
  codex: <profile-id>
  opencode: <profile-id>
  claudeCode: <profile-id>
```

### Provider 兼容性

| providerKind | codex | opencode | claudeCode |
|---|:---:|:---:|:---:|
| `openai` | o | o | x |
| `openai-compatible` | o | o | x |
| `anthropic` | x | o | o |

Schema 会在编辑器中静态校验不兼容的组合。

## 示例配置

```yaml
# yaml-language-server: $schema=https://raw.githubusercontent.com/HeZeBang/keybearer/refs/heads/master/config.schema.json
schemaVersion: 1
profiles:
  default:
    name: DeepSeek-Flash
    providerKind: openai-compatible
    baseUrl: https://api.deepseek.com/v1
    apiKey: sk-your-api-key
    apps:
      - codex
      - opencode
    appConfig:
      codex:
        model: deepseek-v4-flash
      opencode:
        models:
          deepseek-v4-flash:
            name: "Deepseek V4 Flash"
    meta: {}
  cc:
    name: ClaudeCode
    providerKind: anthropic
    baseUrl: https://token-plan-cn.xiaomimimo.com/anthropic
    apiKey: tp-your-api-key
    apps:
      - claudeCode
    appConfig:
      claudeCode:
        model: mimo-v2.5-pro
        sonnetModel: mimo-v2.5-pro
        haikuModel: mimo-v2.5-pro
        opusModel: mimo-v2.5-pro
  other:
    name: other
    providerKind: openai
    baseUrl: https://your-domain.com/v1
    apiKey: sk-your-api-key
    apps:
      - codex
defaults:
  codex: other
  opencode: default
  claudeCode: cc
```

## Credit

Keybearer 的配置设计和多应用支持思路大量参考了 [CC-Switch](https://github.com/farion1231/cc-switch) 项目，感谢其在 Codex / OpenCode / Claude Code 配置管理上的探索。

## License

MIT