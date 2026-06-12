# Keybearer

像 ssh-agent 一样转发授权能力，但保护的是 LLM CLI 工具的 API Key。

API Key 持久化在本地可信宿主机的 `~/.config/keybearer/config.yaml` 中。运行时本地 `keybearer agent` 通过 SSH agent forwarding 暴露 credential query；远端 `keybearer run` 只看到 forwarded `SSH_AUTH_SOCK` 和按需生成的匿名 memfd。Key 不进远端环境变量、不写远端配置文件、不进入远端 shell 历史。

目标是在公用集群上避免管理员或 root 通过远端文件、环境变量、shell 历史、配置目录直接读取 API Key；root 仍可观察或干预进程和内核，因此该边界必须在威胁模型中持续验证。

## 当前状态

当前 MVP 是可运行的 `keybearer` 二进制，已经完成：

- `keybearer agent`：ssh-agent-compatible proxy，透传普通 SSH agent 请求，处理 Keybearer extension。
- 用户编辑 `~/.config/keybearer/config.yaml`，用 `keybearer check` 验证，再启动或 reload agent。
- `keybearer add/list/remove/use/check`：保留本地配置便利命令；活跃状态仍以 YAML 文件为准。
- `keybearer run`：fork 子进程，在 child 中安装 seccomp，parent 通过 `SECCOMP_IOCTL_NOTIF_RECV` 拦截 `open`、`openat`、`openat2`，并用 memfd 注入 Client 侧合成的 AppConfig。
- Agent 返回 `(AppType, optional ProfileId)` 对应的 credential；Client 负责 path matching、AppConfig 解析、merge/replace、序列化和 memfd 注入。
- Codex：`~/.codex/auth.json` replace；`~/.codex/config.toml` TOML merge。
- OpenCode：`~/.config/opencode/opencode.json` JSON/JSON5 merge。

当前未完成项集中在 M10：fallback socket credential-protocol migration、stale remote socket conflict、OS secret store / 本地加密存储、新 AppType 扩展。

## 配置文件

```text
config: ~/.config/keybearer/config.yaml
schema: keybearer-hook/config.schema.json
required top-level keys: schemaVersion, profiles
optional top-level key: defaults
```

示例：

```yaml
schemaVersion: 1
profiles:
  work:
    name: Work OpenAI
    providerKind: openai
    apiKey: sk-...
    apps: [codex, opencode]
    models:
      codex:
        models: [gpt-4o]
        reasoningEffort: high
      opencode:
        models: [gpt-4o]
  oc:
    name: OpenCode compatible
    providerKind: openai-compatible
    apiKey: sk-...
    baseUrl: https://api.example.com/v1
    apps: [opencode]
defaults:
  codex: work
  opencode: oc
```

推荐流程：

```bash
$EDITOR ~/.config/keybearer/config.yaml
keybearer check
eval "$(keybearer agent)"

# foreground/systemd-friendly mode
keybearer agent -D -a /run/user/$UID/keybearer-agent.sock --control-sock /run/user/$UID/keybearer-control.sock
```

`keybearer check` 会校验 YAML 语法、schema version、profile id、default 引用、app enablement，以及 OpenAI-compatible profile 的 `baseUrl`。

## Credential protocol

SSH agent extension：

```text
get-credential@keybearer.dev(appType, profileId?) -> CredentialResponse JSON
```

Agent owns profile resolution：

- non-empty `profileId` 精确选择该 profile；不存在或未启用对应 AppType 时 fail closed。
- empty `profileId` 只使用 `defaults[appType]`；没有默认值时 fail closed。
- 不再读取 SSH hostname、legacy host mapping，且没有 first-provider fallback。
- Agent 不返回路径、TOML、JSON 或完整 virtual file bytes。

保留的本地诊断/control extension：`query`、`get-provider@keybearer.dev`、`add-provider@keybearer.dev`、`reload-store@keybearer.dev`。

## Client-side AppConfig rules

- Codex `~/.codex/auth.json`：replace mode，不读取 remote base，memfd 内容为 compact JSON `{"OPENAI_API_KEY":"..."}`。
- Codex `~/.codex/config.toml`：TOML merge mode，保留 unrelated remote fields/tables，仅管理 top-level model fields 和 `model_providers.keybearer-<profileId>`；API key 不进入 TOML。
- OpenCode `~/.config/opencode/opencode.json`：JSON/JSON5 merge mode，保留 unrelated root fields 和 existing providers，仅替换 `provider.keybearer-<profileId>`。
- malformed merge-mode base config：继续原 syscall，不注入 Keybearer config。
- missing/unreadable/oversized merge-mode base config：从空 base 合成 memfd，不写回磁盘。

## 远端 profile 选择

默认远端命令使用 YAML `defaults`。需要覆盖 profile 时设置：

```bash
KEYBEARER_PROFILE_ID=work keybearer run codex ...
```

SSH config 示例：

```sshconfig
Host epyc1
  ForwardAgent yes
  SetEnv KEYBEARER_PROFILE_ID=work
```

`KEYBEARER_PROFILE_ID` 可来自命令环境、shell profile 或 SSH `SetEnv`；Client 视为同一个来源。

## 目标体验

本地机器：

```bash
$EDITOR ~/.config/keybearer/config.yaml
keybearer check
eval "$(keybearer agent)"
keybearer ssh user@cluster 'KEYBEARER_PROFILE_ID=work keybearer run cat ~/.codex/auth.json'
```

远端集群：

```bash
keybearer run codex "帮我写个快排"
keybearer run -- opencode run "review 这段代码"
alias codex="keybearer run codex"
```

## 架构

### 本地 keybearer-agent

- Provider profiles 持久化在本地 YAML config；当前 `apiKey` 是 `0600` plaintext，属于可信本地宿主机边界。
- `keybearer-agent` 作为 ssh-agent-compatible proxy 运行：监听新的 `$SSH_AUTH_SOCK`，把普通 SSH agent 请求转发给原始 ssh-agent，只处理 Keybearer 自定义 extension。
- `eval "$(keybearer agent)"` backgrounds the proxy with random socket paths, exports `SSH_AUTH_SOCK=<keybearer-proxy-sock>` and `KEYBEARER_CONTROL_SOCK`, saves upstream ssh-agent socket for transparent SSH key passthrough, and exports `KEYBEARER_AGENT_PID` for `eval "$(keybearer agent -k)"`.
- 如果目标平台没有可用 ssh-agent 或 SSH agent forwarding 不可用，M10 fallback 模式再迁移 credential protocol 到 `$KEYBEARER_SOCK` 指向的独立 Unix socket；当前新 credential path 不使用 fallback socket。
- 当前 AppType MVP 只支持 Codex 和 OpenCode。Claude/Gemini 只有确认真实 path/schema 后才新增。

### 远端 keybearer run

- fork 后在 child 中先安装 seccomp，再 `execvp` 目标命令。
- parent 监督 `SECCOMP_IOCTL_NOTIF_RECV` notifications。
- 当目标路径触发 `open`、`openat`、`openat2` 时，Client 将路径映射到 AppConfig rule；merge mode 在 supervisor 进程读取 remote base；随后通过 SSH agent extension 向本地 agent 请求 credentials，合成 memfd bytes 并用 `SECCOMP_IOCTL_NOTIF_ADDFD` 把 fd 返回给 child。
- 非目标路径继续执行原 syscall。

## 威胁模型

保护的数据面：

- 远端环境变量。
- 远端磁盘与配置文件。
- shell history 和包含 API Key 的 process argv。
- 集群上对 `~/.codex/auth.json` 或等价 CLI 配置文件的意外读取。

明确边界：

- 完全失陷的本地机器可以读取 keys，因为本地 agent 持有它们。
- 有特权的远端 root 可以访问 forwarded `$SSH_AUTH_SOCK`、trace、pause 或篡改进程和内核状态；Keybearer 降低 key material 暴露面，但不让远端执行对 root 具备密码学意义上的私密性。
- 恶意远端 child process 可以请求 policy 允许的任何 managed AppConfig path，因此 credential protocol 和 Client path policy 都必须 least-privilege。

## MVP 命令面

- `keybearer agent` starts the local ssh-agent-compatible proxy in shell-eval mode: it backgrounds the agent, uses random socket paths, prints exports for `SSH_AUTH_SOCK`, `KEYBEARER_CONTROL_SOCK`, optional `KEYBEARER_UPSTREAM_SSH_AUTH_SOCK`, and `KEYBEARER_AGENT_PID`.
- `keybearer agent -D` runs the agent in foreground with random socket paths unless `-a` / `--control-sock` are supplied; it does not print shell exports.
- `keybearer agent -a <path>` fixes the eval-mode `SSH_AUTH_SOCK`; `keybearer agent --control-sock <path>` fixes `KEYBEARER_CONTROL_SOCK`.
- `eval "$(keybearer agent -k)"` kills the pid from `KEYBEARER_AGENT_PID`, restores `SSH_AUTH_SOCK` to `KEYBEARER_UPSTREAM_SSH_AUTH_SOCK` when present, and unsets Keybearer-only agent environment variables.
- `keybearer check` 验证 `~/.config/keybearer/config.yaml`。
- `keybearer add <provider-kind> <profile-id> <api-key> [--app codex] [--app opencode] [--model <model>]` 是便利写入命令；不替代手工编辑 YAML。
- `keybearer list/remove/use` 是持久化配置的便利命令面。
- `keybearer ssh [ssh-args...] <host> [remote-command]` wraps `ssh -A ...` using the current `SSH_AUTH_SOCK`; it does not start an agent or choose a socket path.
- `keybearer run [--] <command> [args...]` 监督命令并注入 AppConfig；新 credential path 通过 `$SSH_AUTH_SOCK` 的 Keybearer extension 请求 credentials。

第一批 MVP AppType：

- Codex：`~/.codex/auth.json` 与 `~/.codex/config.toml`。
- OpenCode：`~/.config/opencode/opencode.json`。

Claude/Gemini 不在本轮 MVP 范围内；只有确认真实 path/schema 后才新增 AppType。

## 开发与验证

```bash
cd keybearer-hook
cargo test
KEYBEARER_DEBUG=/tmp/keybearer-debug.log cargo test debug_mode_logs_to_file -- --nocapture
```

## 当前 milestone

M9.5 已完成：credential protocol 与 Client-side AppConfig merge。当前未完成 milestone 是 [M10 — fallback socket 与跨平台补齐](MILESTONES.md#m10--fallback-socket-与跨平台补齐)。
