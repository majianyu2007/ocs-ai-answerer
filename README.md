# ocs-ai-answerer

基于大语言模型（LLM）的 [OCS 网课助手](https://docs.ocsjs.com/) 题库服务，使用 Rust 编写。

只需配置自己的 **OpenAI Compatible** 或 **Anthropic Compatible** API，即可作为一个普通题库导入 OCS，通过官方 `AnswererWrapper` 接口完成搜题与 AI 作答。无需传统题库、第三方搜题服务、数据库或任何额外基础设施，**无需修改 OCS 源码**。

> 本项目只负责「接收题目 → 调用模型 → 规范化答案 → 返回给 OCS」，不包含网页交互、自动填写等脚本逻辑。

---

## 目录

1. [核心功能](#核心功能)
2. [技术架构](#技术架构)
3. [环境要求](#环境要求)
4. [快速开始（本地运行）](#快速开始本地运行)
5. [OpenAI Compatible API 配置](#openai-compatible-api-配置)
6. [Anthropic Compatible API 配置](#anthropic-compatible-api-配置)
7. [Docker 部署](#docker-部署)
8. [导入 OCS 题库配置](#导入-ocs-题库配置)
9. [提高准确率与节省费用](#提高准确率与节省费用)
10. [API 接口说明](#api-接口说明)
11. [各题型请求与响应示例](#各题型请求与响应示例)
12. [答案规范化规则](#答案规范化规则)
13. [配置项参考](#配置项参考)
14. [常见问题排查](#常见问题排查)
15. [开发与测试](#开发与测试)

---

## 核心功能

- **三种协议**：OpenAI Chat Completions（`/v1/chat/completions`）、OpenAI Responses（`/v1/responses`）与 Anthropic Messages（`/v1/messages`），修改配置即可切换，无需重新编译。
- **兼容各类服务**：OpenAI、DeepSeek、OpenRouter、硅基流动、阿里云百炼、Ollama、自建代理/网关等，可自定义 Base URL、完整 Endpoint、API Key、模型、Temperature、Max Tokens、超时、请求头和额外请求参数。
- **原生适配 OCS `AnswererWrapper`**：提供可直接导入的 [`ocs_config.json`](ocs_config.json)，服务端也可通过 `/ocs-config` 动态生成。
- **四种题型**：单选（single）、多选（multiple）、判断（judgement）、填空（completion），以及未知题型的自动推断。
- **可靠的答案规范化**：优先解析结构化 JSON，对 Markdown 代码块、「答案：B」、「正确答案是 B」、选项原文、`ACD`/`A,C,D`/`A、C、D`、判断同义词、带解释的填空等情况做明确、可测试的回退；无法可靠识别时返回明确错误，绝不瞎猜。
- **按 OCS 实际匹配逻辑校验答案**：内置 OCS 选项匹配算法的 Rust 移植版，返回前先模拟 OCS 是否会选中且只选中目标选项，必要时自动改用字母或 `===` 分隔格式。
- **图片题**：支持 OCS「图片题优化」，把题目/选项中的图片以 base64 发送给支持视觉的模型；模型不支持图片时自动降级为纯文本。
- **答案缓存**：同一道题只付一次费用，可持久化到文件，重启后仍然有效；并发的相同题目合并为一次请求。
- **准确率增强**：默认先分析后作答（思维链）；输出无法解析时自动要求模型按格式重述；可选多模型/多次采样投票与裁判模型。
- **工程化**：异步 I/O、HTTP 连接池复用、可配置超时与有限重试（仅对网络错误/超时/429/5xx 重试）、统一错误分类、结构化日志、可选访问令牌。

## 技术架构

```text
OCS 脚本 ──(AnswererWrapper: POST/GET /api/answer)──▶ Axum HTTP 层 (src/api)
                                                          │
                                                          ▼
                                    答题流程 Answerer (src/answer/mod.rs)
                    ┌─────────────────────┼──────────────────────────┐
                    ▼                     ▼                          ▼
          题目解析/题型推断       Prompt 构建 (prompt.rs)      答案解析与规范化 (normalize.rs)
                                          │                          │
                                          ▼                          ▼
                          Provider 抽象层 (src/provider)     OCS 匹配算法移植 (ocs.rs)
                          ├── AiClient：连接池/超时/重试/错误映射
                          ├── OpenAiProtocol   → POST …/chat/completions
                          ├── OpenAiResponsesProtocol → POST …/responses
                          └── AnthropicProtocol → POST …/v1/messages
```

| 组件 | 选型 |
| --- | --- |
| 异步运行时 | Tokio |
| Web 框架 | Axum 0.8 |
| HTTP 客户端 | Reqwest（rustls，全局复用连接池） |
| 序列化 / 配置 | Serde、serde_json、TOML |
| 日志 | tracing、tracing-subscriber |
| 错误 | thiserror |
| 命令行 | clap |
| 测试 | wiremock（Mock 上游 HTTP 服务，无需真实付费接口） |

目录结构：

```text
ocs-ai-answerer/
├── Cargo.toml / Cargo.lock
├── config.example.toml      # 配置示例（复制为 config.toml）
├── .env.example             # 环境变量示例（复制为 .env）
├── ocs_config.json          # 可直接导入 OCS 的题库配置
├── Dockerfile / compose.yaml
├── src/
│   ├── main.rs              # 启动入口、命令行参数
│   ├── lib.rs
│   ├── config.rs            # 配置结构、环境变量覆盖、启动校验
│   ├── error.rs             # 分类错误类型
│   ├── api/                 # HTTP 层：路由、处理器、请求/响应类型、OCS 配置生成
│   ├── provider/            # AI Provider 抽象层：mod.rs（公共逻辑）、openai.rs、responses.rs、anthropic.rs
│   └── answer/              # 答题流程 mod.rs、question.rs（题目模型）、prompt.rs、normalize.rs、
│                            # ocs.rs（OCS 匹配算法移植）、cache.rs（答案缓存）、ensemble.rs（投票）
└── tests/                   # provider_tests / answer_tests / api_tests / pipeline_tests
```

## 环境要求

- 运行预编译二进制或 Docker：**无任何依赖**（不需要 Python、Node.js 等）。
- 从源码构建：Rust stable **1.88+**（Edition 2024）。安装方式：<https://rustup.rs>
  - macOS / Linux：`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
  - Windows：下载并运行 `rustup-init.exe`，按提示安装 Visual Studio C++ Build Tools。
- 一个可用的 OpenAI Compatible 或 Anthropic Compatible API Key。

## 快速开始（本地运行）

```bash
# 1. 获取代码并进入目录
cd ocs-ai-answerer

# 2. 复制配置文件
cp config.example.toml config.toml        # Windows: copy config.example.toml config.toml

# 3. 编辑 config.toml，至少修改 [ai] 下的 base_url、api_key、model

# 4. 启动（开发模式）
cargo run

# 或构建正式版二进制后运行
cargo build --release
./target/release/ocs-ai-answerer           # Windows: .\target\release\ocs-ai-answerer.exe
```

看到如下日志即启动成功：

```text
INFO 已加载配置文件 path=config.toml
INFO AI 接口已配置（尚未实际连通，可访问 /health?check=true 检测） provider=openai model="gpt-4o-mini" ...
INFO 服务已启动: http://127.0.0.1:3000
INFO OCS 题库配置: http://127.0.0.1:3000/ocs-config
```

验证模型接口是否真的可用（会向模型发送一次很小的请求）：

```bash
curl "http://127.0.0.1:3000/health?check=true"
```

试答一道题：

```bash
curl -X POST http://127.0.0.1:3000/api/answer \
  -H "Content-Type: application/json" \
  -d '{"title":"下面哪种语言主要用于系统编程？","type":"single","options":"A. HTML\nB. Rust\nC. CSS\nD. SQL"}'
```

命令行参数：

| 参数 | 说明 |
| --- | --- |
| `-c, --config <路径>` | 指定配置文件（默认 `./config.toml`，也可用环境变量 `OCS_CONFIG`） |
| `--host <地址>` / `-p, --port <端口>` | 覆盖监听地址/端口 |
| `--check` | 只校验配置后退出 |
| `--print-ocs-config` | 打印 OCS 题库配置 JSON 后退出 |

> 不想把 API Key 写进文件？也可以不创建 `config.toml`，直接用环境变量启动：
> `OCS_AI_API_KEY=sk-xxx OCS_AI_MODEL=gpt-4o-mini cargo run`（程序也会自动读取当前目录的 `.env` 文件）。

## OpenAI Compatible API 配置

`provider = "openai"`，请求 `POST {base_url}/chat/completions`，认证头为 `Authorization: Bearer <api_key>`。

**`base_url` 填写到版本号为止**（如 `.../v1`）；若只填写域名（没有路径），程序会自动补上 `/v1/chat/completions`。

```toml
[ai]
provider = "openai"
base_url = "https://api.openai.com/v1"
api_key  = "sk-..."
model    = "gpt-4o-mini"
temperature = 0.1
max_tokens  = 1024
```

常见服务示例：

| 服务 | base_url | model 示例 |
| --- | --- | --- |
| OpenAI | `https://api.openai.com/v1` | `gpt-4o-mini` |
| DeepSeek | `https://api.deepseek.com/v1` | `deepseek-chat` |
| OpenRouter | `https://openrouter.ai/api/v1` | `openai/gpt-4o-mini` |
| 硅基流动 | `https://api.siliconflow.cn/v1` | `Qwen/Qwen2.5-72B-Instruct` |
| 阿里云百炼 | `https://dashscope.aliyuncs.com/compatible-mode/v1` | `qwen-plus` |
| Ollama（本地） | `http://127.0.0.1:11434/v1` | `qwen2.5:7b`（api_key 随便填） |

其它说明：

- 代理/网关路径不规范时，直接填写完整地址：`endpoint = "https://gw.example.com/custom/path/chat"`，此时忽略 `base_url`。
- 额外请求头放在 `[ai.headers]`，额外请求体参数放在 `[ai.extra_body]`（原样合并到请求 JSON 顶层）：

  ```toml
  [ai.headers]
  "HTTP-Referer" = "https://example.com"

  [ai.extra_body]
  top_p = 0.9
  enable_thinking = false                      # 关闭 Qwen3 思考模式
  response_format = { type = "json_object" }  # 仅支持该参数的服务可开启
  ```

- **推理模型**（`deepseek-reasoner`、o 系列等）：输出前会消耗大量 token 思考，请把 `max_tokens` 调到 4096 以上，并适当增大 `timeout_secs`；部分模型不支持 `temperature`，可删除该行（不设置就不会发送）。程序会自动忽略回复中的 `<think>…</think>` 内容。

### OpenAI Responses

使用原生 Responses API 时，设置 `provider = "openai-responses"`（环境变量为 `OCS_AI_PROVIDER=openai-responses`）。原有 `openai` 模式仍使用 Chat Completions，不会自动切换协议。

```toml
[ai]
provider = "openai-responses"
base_url = "https://api.openai.com/v1"
model = "gpt-4.1"
max_tokens = 8192
# API Key 通过 OCS_AI_API_KEY 环境变量设置
```

该示例请求 `https://api.openai.com/v1/responses`。`base_url` 留空时默认使用 `https://api.openai.com/v1`；带路径时追加 `/responses`，仅有域名时追加 `/v1/responses`。也可以用 `endpoint`（或 `OCS_AI_ENDPOINT`）指定完整地址，覆盖自动拼接。

认证仍使用 Bearer API Key。请求将系统提示放入 `instructions`、消息放入 `input`，`max_tokens` 映射为 `max_output_tokens`，默认发送 `stream: false`、`store: false`。支持图片输入及答案修复所需的助手历史消息；只读取已完成响应中助手的 `output_text`，失败、未完成或非最终输出不会作为答案。额外参数应使用 Responses API 的字段，而非 Chat Completions 的 `response_format` 等字段。

使用 Docker 的 `.env` 配置时，对应写法如下（不要将真实密钥提交到仓库）：

```dotenv
OCS_AI_PROVIDER=openai-responses
OCS_AI_BASE_URL=https://api.openai.com/v1
OCS_AI_MODEL=gpt-4.1
OCS_AI_MAX_TOKENS=8192
# OCS_AI_API_KEY 在本机或服务器的私密 .env 中填写
```

修改 `.env` 后执行 `docker compose up -d` 重新创建服务，使新的环境变量生效；仅 `docker compose restart` 不会更新容器环境变量。如果 Compose 的 `environment` 中已设置同名变量，应同时修改该处，否则它会覆盖 `env_file`。

`store: false` 是向上游发送的不存储请求选项，不代表第三方网关不会记录请求。API Key 只用于服务器调用上游；OCS 导入配置携带的是独立的 `server.access_token`，不是上游 API Key，导入配置同样需要保密。

## Anthropic Compatible API 配置

`provider = "anthropic"`，请求 `POST {base_url}/v1/messages`，自动发送 `x-api-key` 与 `anthropic-version` 请求头，`system` 提示词使用顶层 `system` 字段，用户消息使用 `content` 内容块。

**`base_url` 只需填写域名**；若已经以 `/v1` 结尾，则只拼接 `/messages`。

```toml
[ai]
provider = "anthropic"
base_url = "https://api.anthropic.com"
api_key  = "sk-ant-..."
model    = "claude-sonnet-4-5"
max_tokens = 1024             # Anthropic 必填，未设置时默认 1024
temperature = 0.1             # Anthropic 范围是 0~1，超出会被自动截断
anthropic_version = "2023-06-01"
```

兼容 Anthropic 协议的第三方服务同样只需修改 `base_url`（或 `endpoint`）。响应中的 `thinking` 等非文本内容块会被忽略，只取 `text` 块作为答案。

## Docker 部署

```bash
cp .env.example .env
# 编辑 .env，填写 OCS_AI_API_KEY、OCS_AI_MODEL、OCS_AI_BASE_URL 等

docker compose up -d --build
docker compose logs -f
```

答案缓存保存在宿主机的 `./data` 目录，容器重建后仍然有效。

默认只映射到本机 `127.0.0.1:3000`。远程访问建议保留此绑定，通过 HTTPS 反向代理或隧道（如 Cloudflare Tunnel）对外提供服务；原生运行时也应绑定回环地址。仅放行 `/api/answer` 与 `/ocs-config`，将 `/health`、`/api/cache` 及其子路径保留为本机管理接口，其余路径默认拒绝。

**务必设置独立、随机的 `OCS_ACCESS_TOKEN`**，并通过 `OCS_PUBLIC_URL` 指定对外 HTTPS 地址。`OCS_AI_API_KEY` 是服务端调用上游模型的密钥，不能作为访问令牌，也不要填进 OCS 客户端配置。若确需直接开放端口，可将 compose 端口改为 `"3000:3000"`，但仍需配置访问令牌、防火墙与 HTTPS。

想使用完整的配置文件（自定义提示词等）时，复制 `config.example.toml` 为 `config.toml`，并在 `compose.yaml` 中取消 `volumes` 的注释。容器内会自动设置 `OCS_SERVER_HOST=0.0.0.0`。

不使用 compose：

```bash
docker build -t ocs-ai-answerer .
docker run -d --name ocs-ai-answerer -p 127.0.0.1:3000:3000 --env-file .env ocs-ai-answerer
```

镜像为多阶段构建，运行时基于 Alpine，只包含单个静态链接的二进制文件。

## 导入 OCS 题库配置

### 1. 获取配置

任选其一：

- 直接使用仓库中的 [`ocs_config.json`](ocs_config.json)（默认地址 `http://127.0.0.1:3000`）；
- 服务启动后访问 `http://127.0.0.1:3000/ocs-config`，会按当前地址和访问令牌自动生成；
- 运行 `ocs-ai-answerer --print-ocs-config`。

```json
[
  {
    "name": "OCS AI Answerer",
    "url": "http://127.0.0.1:3000/api/answer",
    "method": "post",
    "type": "GM_xmlhttpRequest",
    "contentType": "json",
    "headers": {
      "Content-Type": "application/json"
    },
    "data": {
      "title": "${title}",
      "type": "${type}",
      "options": "${options}",
      "images": {
        "handler": "return (env) => env.images"
      },
      "suggestion_title": "${suggestion_title}",
      "suggestion_options": "${suggestion_options}"
    },
    "handler": "return (res) => res && res.code === 1 ? [res.question, res.answer, { ai: true }] : [res && res.msg ? res.msg : 'AI 题库请求失败', undefined]"
  }
]
```

### 2. 导入

打开 OCS 面板 → **通用 → 全局设置 → 题库配置**，把上面的 JSON 粘贴进去，保存后刷新页面即可。

### 3. 配置说明

- `images` 字段使用字段解析器 `return (env) => env.images` 原样上传图片数组（如果写成 `"${images}"` 占位符，OCS 会把数组拼成字符串）；`${suggestion_title}`、`${suggestion_options}` 为图片地址替换成 `[图片N]` 后的文本。GET 方式无法上传图片。
- `${title}`、`${type}`、`${options}` 由 OCS 在请求时替换：`options` 是各选项文本以换行 `\n` 拼接的字符串，`type` 取值为 `single`/`multiple`/`judgement`/`completion`，无法识别时为 `unknown`。
- `handler` 成功时返回 `[题目, 答案, { ai: true }]`，OCS 会在搜索结果上显示「AI」标签；失败时把错误信息放在题目位置、答案为 `undefined`，这样在 OCS 搜索结果面板里能直接看到失败原因（如 API Key 错误、超时），而不是笼统的「未找到」。
- 服务端出错时 HTTP 状态码仍为 200、`code` 为 0。这是有意为之：OCS 使用 `GM_xmlhttpRequest` 时，任何非 200 状态码都会被直接丢弃，handler 根本拿不到错误信息。
- 设置了访问令牌时，`/ocs-config` 生成的配置会在 `data` 中自动带上 `"token": "..."`。

### 4. 本地地址与远程地址

| 场景 | url 应填写 |
| --- | --- |
| 服务和浏览器在同一台电脑 | `http://127.0.0.1:3000/api/answer` |
| 服务部署在局域网其它机器 | `http://192.168.x.x:3000/api/answer`（服务需监听 `0.0.0.0`） |
| 服务部署在云服务器 | `https://你的域名/api/answer`（建议经 Nginx/Caddy 配置 HTTPS） |

`127.0.0.1` 永远指向**运行浏览器的那台电脑**。如果服务在服务器上，填 `127.0.0.1` 是连不上的。

部署到服务器时建议在配置中设置 `server.public_url = "https://你的域名"`，`/ocs-config` 会生成正确的地址。

### 5. 连接相关的注意事项

- **`@connect` 域名白名单**：油猴脚本只能用 `GM_xmlhttpRequest` 访问脚本头部 `@connect` 声明过的域名。OCS 默认已包含 `localhost` 与 `127.0.0.1`，因此本地使用无需额外操作。使用**远程域名或局域网 IP** 时，OCS 导入配置会提示「不在脚本的白名单中」，此时需要安装 OCS 的 **全域名通用版本**（文档：<https://docs.ocsjs.com/docs/other/api#全域名通用版本>），或在脚本管理器中为该脚本手动添加 `@connect`。首次请求时脚本管理器也可能弹窗询问是否允许跨域访问，选择「总是允许」。
- **跨域（CORS）**：`GM_xmlhttpRequest` 不受浏览器 CORS 限制，推荐使用。若改为 `"type": "fetch"`，请求会以网课页面为来源发出，需要服务端开启 CORS（默认 `server.cors = true`，已开启）。
- **混合内容**：网课页面一般是 HTTPS。`fetch` 模式下，HTTPS 页面无法请求 `http://` 的远程地址（浏览器会拦截混合内容，`127.0.0.1` 一般例外）。`GM_xmlhttpRequest` 模式不受此限制。远程部署建议直接使用 HTTPS。
- **超时**：OCS 等待单个题库 60 秒后判定超时。本服务默认 `server.request_timeout_secs = 55`，超时后会返回明确的错误信息，而不是让 OCS 一直等待。

## 提高准确率与节省费用

没有任何模型能保证 100% 正确，错题主要来自：图片题（模型看不到图）、依赖具体教材的题、多选题漏选或多选、网页抓取的题目文本残缺。本服务在模型侧能做的优化都已内置：

| 手段 | 默认 | 作用 | 代价 |
| --- | --- | --- | --- |
| 图片题（`ai.vision`） | 开启 | 图片题可以被正确作答 | 图片会消耗输入 token |
| 先分析后作答（`answer.step_by_step`） | 开启 | 普通模型准确率明显提升，尤其是计算题和多选题 | 输出 token 增加 |
| 格式自修复（`answer.self_repair_attempts`） | 1 次 | 模型偶尔不按格式输出时自动纠正，而不是直接失败 | 仅在出错时多一次请求 |
| 答案缓存（`[cache]`） | 开启 | 重做、换账号时同一道题不再付费 | 无 |
| 多模型投票（`[ensemble]`） | 关闭 | 不同模型/多次采样取多数，分歧大时交给裁判模型 | 费用约为模型数倍 |

### 模型选择建议

- **图片题**需要视觉模型：GPT-4o / GPT-4.1 系列、Claude、Qwen-VL、GLM-4V 等。DeepSeek 目前不支持图片，请设置 `vision = false`（或让程序自动降级）。
- **推理模型**（`deepseek-reasoner`、o 系列、开启 thinking 的 Claude）本身会深度思考：建议 `step_by_step = false`、`max_tokens = 8192`、`timeout_secs = 45`，并删除 `temperature`。注意推理模型较慢，单题耗时需在 OCS 的 60 秒限制内。
- **性价比**：`deepseek-chat` + `step_by_step = true` 已能覆盖绝大多数文字题；再加一个视觉模型作为投票者即可兼顾图片题。

### 多模型投票示例

```toml
[ai]                                  # 主模型（同时也是第一个投票者）
provider = "openai"
base_url = "https://api.deepseek.com/v1"
api_key  = "sk-..."
model    = "deepseek-chat"
vision   = false

[ensemble]
enabled = true
samples = 2          # 每个模型最多作答 2 次
min_agreement = 0.6  # 领先答案得票率低于 60% 时请裁判模型决定
early_stop = true    # 首轮所有模型一致时直接采用（大多数题只花 N 次请求）

[[ensemble.models]]
provider = "openai"
base_url = "https://api.openai.com/v1"
api_key  = "sk-..."
model    = "gpt-4.1-mini"

[ensemble.judge]
provider = "anthropic"
base_url = "https://api.anthropic.com"
api_key  = "sk-ant-..."
model    = "claude-sonnet-4-5"
```

流程：所有模型先各答一次 → 一致则直接返回 → 不一致时补充采样（使用更高的温度以获得独立的判断）→ 按规范化后的答案计票 → 得票率不足时由裁判模型参考候选答案重新分析。所有轮次都受 `server.request_timeout_secs` 约束：剩余时间不足时不再开启新一轮，超时未返回的投票会被放弃而不会拖垮整个请求。响应中的 `confidence` 为最终答案的得票率，`votes` 为有效票数。

### 答案缓存

- 以「题型 + 归一化题干 + 归一化选项 + 图片」为键，`A.`/`A、` 前缀、空格和标点差异不影响命中。
- 默认持久化到 `data/cache.jsonl`（Docker 中为挂载的 `./data` 目录）。
- 发现缓存了错误答案时：`POST /api/cache/forget`（请求体与 `/api/answer` 相同）删除单题，或请求时带上 `"no_cache": true` 强制重新作答；`DELETE /api/cache` 清空全部缓存。
- 回答失败的题目不会被缓存。

### 图片题

在 OCS「通用 → 全局设置 → 高级设置」中开启 **图片题优化**（默认开启），并使用本项目的 `ocs_config.json`（POST 方式，已包含图片字段）。OCS 会把图片转换为 base64，并把题干中的图片地址替换为 `[图片1]` 这样的占位符，本服务会把图片与占位符一起发给模型。

## API 接口说明

### `POST /api/answer`

请求体（JSON）：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `title` | string | **必填**，题目内容（也接受 `question`） |
| `type` | string | `single` / `multiple` / `judgement` / `completion` / `unknown`，也接受「单选」「多选题」等中文别名；缺失或未知时自动推断 |
| `options` | string 或 string[] | 选项。OCS 默认格式为以 `\n` 分隔的字符串，也接受字符串数组；可带或不带 `A.` 前缀 |
| `token` | string | 可选，访问令牌（也可用 `Authorization: Bearer <token>` 请求头） |
| `images` | string[] | 可选，图片题的 base64 data URL（或 http 地址），由 OCS 图片题优化提供 |
| `suggestion_title` / `suggestion_options` | string | 可选，图片地址替换为 `[图片N]` 后的题干/选项 |
| `no_cache` | bool | 可选，跳过缓存重新作答 |

也接受 `application/x-www-form-urlencoded` 表单。

成功响应：

```json
{
  "code": 1,
  "question": "下面哪种语言主要用于系统编程？",
  "answer": "Rust",
  "type": "single",
  "model": "gpt-4o-mini",
  "provider": "openai",
  "elapsed_ms": 1234,
  "ai": true,
  "cached": false
}
```

启用投票时额外返回 `confidence`（得票率）与 `votes`（有效票数）。

开启 `answer.include_explanation` 时额外返回 `explanation` 字段（不影响 `answer`）。

失败响应（HTTP 状态码仍为 200）：

```json
{
  "code": 0,
  "msg": "AI 接口认证失败 (HTTP 401)，请检查 API Key 与权限: Incorrect API key provided",
  "error_type": "provider_unauthorized",
  "question": "下面哪种语言主要用于系统编程？"
}
```

`error_type` 取值：

| error_type | 含义 |
| --- | --- |
| `bad_request` | 缺少 title、请求体不是合法 JSON 等 |
| `unauthorized` | 访问令牌无效或缺失 |
| `timeout` | 整体处理超过 `server.request_timeout_secs` |
| `provider_connect` | DNS/网络/TLS 错误，无法连接 AI 接口 |
| `provider_timeout` | 单次 AI 请求超时 |
| `provider_unauthorized` | 上游返回 401/403 |
| `provider_rate_limited` | 上游返回 429 |
| `provider_server_error` | 上游返回 5xx |
| `provider_api_error` | 上游返回其它错误（如 400 参数错误、模型不存在） |
| `provider_invalid_response` | 上游响应不是合法 JSON 或缺少必要字段 |
| `provider_empty_content` | 模型返回空内容（常见于 max_tokens 过小） |
| `answer_empty` | 模型输出为空 |
| `answer_unrecognized` | 模型输出无法可靠地对应到答案 |

### `GET /api/answer`

与 POST 使用同一套处理逻辑，参数通过查询字符串传递：

```bash
curl -G http://127.0.0.1:3000/api/answer \
  --data-urlencode "title=下面哪种语言主要用于系统编程？" \
  --data-urlencode "type=single" \
  --data-urlencode $'options=A. HTML\nB. Rust\nC. CSS\nD. SQL'
```

`GET /ocs-config?method=get` 可生成 GET 方式的 OCS 配置。

### `GET /health`

```json
{
  "status": "ok",
  "service": "ocs-ai-answerer",
  "version": "0.1.0",
  "provider": "openai",
  "model": "gpt-4o-mini",
  "endpoint": "https://api.openai.com/v1/chat/completions",
  "uptime_secs": 42,
  "upstream": { "status": "unchecked", "hint": "仅表示服务进程正常；…" }
}
```

- 默认**不会**请求模型，`upstream.status` 为 `unchecked`，只代表服务进程正常。
- `ensemble` 字段列出投票模型与裁判模型，`cache` 字段显示缓存条目数。
- `GET /health?check=true` 会向模型发送一次真实的小请求，`upstream.status` 为 `ok` 或 `error`（附带错误原因与耗时）。设置了访问令牌时需带上 `token`。

### 缓存管理

- `DELETE /api/cache`：清空全部缓存（含持久化文件）。
- `POST /api/cache/forget`：删除单道题的缓存，请求体与 `/api/answer` 相同，返回 `{"code":1,"removed":true}`。

设置了访问令牌时同样需要携带。

### `GET /ocs-config`

返回当前服务对应的 OCS 题库配置数组。地址优先使用 `server.public_url`，否则根据请求的 `Host` 推断。设置了访问令牌时需带上 `?token=...` 才能获取。

## 各题型请求与响应示例

| 题型 | 请求 | 模型输出示例 | 返回的 `answer` |
| --- | --- | --- | --- |
| 单选 | `{"title":"…系统编程？","type":"single","options":"A. HTML\nB. Rust\nC. CSS\nD. SQL"}` | `{"answer":"B"}` | `Rust` |
| 多选 | `{"title":"以下属于编程语言的是","type":"multiple","options":"A. Rust\nB. HTML\nC. Go\nD. Python"}` | `{"answer":["A","C","D"]}` | `Rust#Go#Python` |
| 判断（有选项） | `{"title":"地球是平的","type":"judgement","options":"对\n错"}` | `{"answer":false}` | `错误` |
| 判断（无选项） | `{"title":"水在 0℃ 结冰","type":"judgement","options":""}` | `{"answer":true}` | `正确` |
| 填空 | `{"title":"中国首都是____，最大城市是____","type":"completion","options":""}` | `{"answer":["北京","上海"]}` | `北京#上海` |
| 未知（有选项） | `{"title":"…","type":"unknown","options":"A. x\nB. y\nC. z"}` | `{"answer":["A","C"]}` | `x#z` |
| 未知（无选项） | `{"title":"…","type":"unknown","options":""}` | `{"answer":["答案"]}` | `答案` |

## 答案规范化规则

OCS 匹配选项的方式（已阅读 OCS 源码 `core/worker/resolvers/*` 确认）：

1. **选项文本优先**：把答案按分隔符（默认 `===`、`#`、`---`、`###`、`|`、`;`、`；`，按顺序取第一个出现的）拆开，与选项文本做归一化匹配和相似度匹配；
2. **纯字母兜底**：只有文本匹配不到时，才把 `B`、`ACD`、`A#C#D` 这类纯字母答案按下标映射到选项；
3. 判断题按固定词表匹配：`正确/对/是/√/true/T/yes/1` 与 `错误/错/否/×/X/false/F/no/0` 等。

因此本服务：

- **选择题**默认返回**选项原文**（多选用 `#` 连接），这是 OCS 最可靠的匹配方式；返回前会用移植的 OCS 匹配算法模拟一次，如果原文可能误选或漏选（例如选项之间是包含关系「计算机 / 计算机网络」、选项文本本身含有 `#`），会自动改用 `===` 连接或字母形式（如 `A#C`）。可通过 `answer.choice_format` 改为固定返回字母。
- **多选题**结果去重并按选项顺序排列，绝不把 JSON 数组直接作为 `answer` 返回。
- **判断题**优先使用配置中的 `正确/错误`，并校验 OCS 能匹配到对应选项；不能时改用选项自身的文字。
- **填空题**多个空按顺序用 `#` 连接，OCS 会按分隔符拆分后依次填入各个空。
- **模型输出解析**：依次尝试纯 JSON → Markdown 代码块中的 JSON → 文本中嵌入的 JSON → 被截断的 JSON 中的 `answer` 字段 → 明确的文本模式（「答案：B」「正确答案是 A、C」「B. Rust」、选项原文、同义词等）。
- **防误判**：普通英文句子中的字母（如 `A lot of…`、`CPU`）不会被当作选项；字母与选项原文矛盾（`C. Rust` 但 Rust 是 B）、单选题返回多个选项、字母超出选项范围等情况都会返回 `answer_unrecognized` 错误，而不是随便给一个答案。

## 配置项参考

完整说明见 [`config.example.toml`](config.example.toml)。

| 配置项 | 默认值 | 环境变量 | 说明 |
| --- | --- | --- | --- |
| `server.host` | `127.0.0.1` | `OCS_SERVER_HOST` | 监听地址，远程访问填 `0.0.0.0` |
| `server.port` | `3000` | `OCS_SERVER_PORT` | 监听端口 |
| `server.log_level` | `info` | `OCS_LOG_LEVEL` | 日志级别；`debug` 会输出提示词与模型原始输出。也支持 `RUST_LOG` |
| `server.request_timeout_secs` | `55` | | 单题总超时（含重试） |
| `server.access_token` | 空 | `OCS_ACCESS_TOKEN` | 访问令牌，空表示不校验 |
| `server.public_url` | 空 | `OCS_PUBLIC_URL` | 对外地址，用于生成 OCS 配置 |
| `server.cors` | `true` | | 是否允许跨域 |
| `ai.provider` | `openai` | `OCS_AI_PROVIDER` | `openai`（Chat Completions）、`openai-responses` 或 `anthropic` |
| `ai.base_url` | 官方地址 | `OCS_AI_BASE_URL` | API 根地址 |
| `ai.endpoint` | 空 | `OCS_AI_ENDPOINT` | 完整请求地址，优先于 base_url |
| `ai.api_key` | 空（必填） | `OCS_AI_API_KEY` | API Key |
| `ai.model` | 空（必填） | `OCS_AI_MODEL` | 模型名称 |
| `ai.temperature` | `0.1` | `OCS_AI_TEMPERATURE` | 不设置则不发送 |
| `ai.max_tokens` | `2048` | `OCS_AI_MAX_TOKENS` | Responses 映射为 `max_output_tokens`；OpenAI 为 None 时不发送；Anthropic 为 None 时使用 1024 |
| `ai.timeout_secs` | `40` | `OCS_AI_TIMEOUT_SECS` | 单次请求超时 |
| `ai.connect_timeout_secs` | `10` | | 连接超时 |
| `ai.max_retries` | `1` | `OCS_AI_MAX_RETRIES` | 仅对网络错误、超时、429、5xx 重试 |
| `ai.retry_backoff_ms` | `800` | | 重试间隔基数 |
| `ai.anthropic_version` | `2023-06-01` | | Anthropic 版本请求头 |
| `ai.headers` | 空 | | 额外请求头 |
| `ai.extra_body` | 空 | | 额外请求体参数 |
| `ai.vision` | `true` | `OCS_AI_VISION` | 是否发送图片 |
| `ai.max_concurrency` | `8` | | 对该模型的最大并发请求数 |
| `ai.name` | 自动 | | 日志中显示的名称 |
| `answer.system_prompt` 等 | 内置 | | 系统提示词及各题型提示词，留空使用默认值 |
| `answer.include_explanation` | `false` | | 是否让模型返回简短解析 |
| `answer.choice_format` | `auto` | | `auto` / `text` / `letter` |
| `answer.multiple_separator` | `#` | | 多选答案分隔符 |
| `answer.completion_separator` | `#` | | 填空多空分隔符 |
| `answer.judgement_true` / `judgement_false` | `正确` / `错误` | | 判断题返回文本 |
| `answer.step_by_step` | `true` | `OCS_STEP_BY_STEP` | 先分析后作答 |
| `answer.self_repair_attempts` | `1` | | 输出无法解析时要求重述的次数 |
| `cache.enabled` | `true` | `OCS_CACHE_ENABLED` | 答案缓存 |
| `cache.capacity` | `20000` | | 内存缓存条目上限 |
| `cache.ttl_secs` | `0` | | 有效期，0 为永久 |
| `cache.file` | 空（示例配置为 `data/cache.jsonl`） | `OCS_CACHE_FILE` | 持久化文件 |
| `ensemble.enabled` | `false` | | 多模型投票 |
| `ensemble.samples` | `3` | | 每个模型最多采样次数 |
| `ensemble.min_agreement` | `0.6` | | 低于该得票率时请求裁判 |
| `ensemble.early_stop` | `true` | | 首轮一致时提前结束 |
| `ensemble.models` | 空 | | 额外投票模型（字段同 `[ai]`） |
| `ensemble.judge` | 无 | | 裁判模型（字段同 `[ai]`） |

优先级：命令行参数 > 环境变量（含 `.env`）> `config.toml` > 默认值。启动时会校验配置，错误会逐条列出；配置文件中拼错的字段名也会直接报错。

## 常见问题排查

| 现象 | 可能原因与解决办法 |
| --- | --- |
| 启动提示 `ai.api_key 未配置` | 在 `config.toml` 填写 `api_key`，或设置环境变量 `OCS_AI_API_KEY` |
| 启动提示 `配置文件 … 格式错误` | TOML 语法错误或字段名拼写错误，按提示的行号修改 |
| 启动提示 `无法监听 … 端口可能被占用` | 修改 `server.port` 或使用 `--port 3001` |
| `provider_unauthorized` (401/403) | API Key 错误、过期、余额不足，或 provider 选错（Anthropic 的 Key 用在了 OpenAI 协议上） |
| `provider_api_error` (404) | 地址不对。检查 `base_url` 是否多写/少写了 `/v1`，或直接填写完整的 `endpoint` |
| `provider_api_error` (400) | 模型名称错误，或该模型不支持某个参数（如推理模型不支持 temperature，删除该配置行） |
| `provider_rate_limited` (429) | 触发限流，降低 OCS 的答题并发数，或增加 `max_retries` |
| `provider_connect` | 网络不通、DNS 失败、需要代理。程序会读取系统代理及 `HTTPS_PROXY` 环境变量 |
| `provider_timeout` / `timeout` | 模型响应太慢，增大 `ai.timeout_secs` 或更换更快的模型；注意总时间要小于 OCS 的 60 秒 |
| `provider_empty_content` | 推理模型的 `max_tokens` 太小，思考阶段就用完了，调大到 8192 左右 |
| `provider_invalid_response` 提示缺少 `choices`，实际返回 `object: "response"` 和 `output` | 网关返回的是 Responses 格式。确认它支持 `/v1/responses` 后，将 `provider` 改为 `openai-responses`；仅修改模型名称或 `endpoint` 不会切换请求与解析协议 |
| Responses 响应未完成，或包含 `incomplete_details` | 服务不会使用部分输出作为答案。检查上游状态、输出额度和超时；若为 `max_output_tokens` 耗尽，可调大配置中的 `max_tokens` |
| 日志提示「模型似乎不支持图片输入」 | 当前模型不支持视觉，图片题已降级为纯文本；换用视觉模型或设置 `vision = false` |
| 图片题答错 | 确认 OCS 已开启「图片题优化」且使用了最新的 `ocs_config.json`（需为 POST 且包含 `images` 字段），并使用支持图片的模型 |
| 某道题缓存了错误答案 | `POST /api/cache/forget` 删除该题，或请求时带 `"no_cache": true` |
| `answer_unrecognized` | 模型没有按格式回答。设置 `log_level = "debug"` 查看原始输出；换用指令遵循更好的模型，或调整提示词 |
| OCS 提示「不在脚本的白名单中」 | 使用了远程地址，见上文 `@connect` 说明 |
| OCS 显示「题库请求超时」或连接失败 | 服务没启动、地址填错（服务器上的服务不能用 127.0.0.1）、防火墙未放行端口 |
| OCS 搜到了答案但没有选中选项 | 把 `log_level` 设为 `debug` 查看返回内容，尝试 `answer.choice_format = "letter"`，并检查 OCS「答案分隔符」设置中是否包含 `#` |

## 开发与测试

```bash
cargo fmt --check              # 代码格式检查
cargo clippy --all-targets     # 静态检查
cargo test                     # 运行全部测试（使用 Mock 服务，不会调用付费接口）
cargo build --release          # 构建正式版
```

测试覆盖：

- `tests/provider_tests.rs`：OpenAI Chat Completions / Responses / Anthropic 请求格式、自定义 Endpoint 与请求头、额外参数、标准响应解析、401/403/429/5xx/HTTP 200 中的错误对象、超时、连接失败、非 JSON 响应、空内容、重试策略。
- `tests/answer_tests.rs`：选项解析、题型推断、提示词构建、单选/多选/判断/填空的各种输出格式、JSON 与非标准回退、误判防护、无法识别的处理，以及用 OCS 匹配算法验证返回值能被 OCS 正确选中。
- `tests/pipeline_tests.rs`：答案缓存（命中、持久化、删除、失败不缓存）、并发相同题目合并、格式自修复、图片发送与自动降级、多模型投票（提前结束、多数票、裁判、投票者失败、超时保留已有投票）、并发限制、Retry-After。
- `tests/api_tests.rs`：GET/POST、缺失参数、上游异常、超时、访问令牌、健康检查、OCS 配置生成、`ocs_config.json` 与生成结果一致、并发请求互不干扰。
- `src/answer/ocs.rs` 中的单元测试：OCS 匹配算法移植的基本行为。

## 许可证

[MIT](LICENSE)
