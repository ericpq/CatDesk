# CatDesk 编码 Agent 分支 · 使用说明

这份文档写给已经在用**原版 CatDesk** 的人。分支 `catdesk-agent-upgrade` 在原版基础上把工具从 10 个扩到 22 个，目标是让 ChatGPT 能在一个**真实规模的代码库**上干活，而不只是改小文件。

原版的 CatDesk 是"把你的电脑接给 ChatGPT"；这个分支想做的是"接给它之后，它能自己看懂、自己验证、自己回退"。

---

## 一、和原版的区别

### 工具：10 → 22

原版 10 个：

```
catdesk_instruction  read  search  write  edit  delete
run_command  start_command  poll_command  cancel_command
```

本分支新增 12 个：

| 分组 | 工具 | 解决什么 |
|---|---|---|
| **代码导航** | `outline` `find_symbol` `read_symbol` | 大文件读不完 |
| **结果验证** | `run_checks` | 测试输出被截断，看不到结论 |
| **撤销** | `checkpoint_list` `checkpoint_restore` | 改错了拿不回来 |
| **补丁** | `apply_patch` | 多处修改要一次次 write |
| **Git** | `git_status` `git_diff` `git_log` `git_add` `git_commit` | 只能拼 shell 命令 |

### 读取上限：反而变小了

这一点要先讲清楚，否则你会以为是退步：

| 限制 | 原版 | 本分支 |
|---|---|---|
| 单文件读取 | 512 KiB | **32 KiB** |
| 一次 read 总量 | 512 KiB | **64 KiB** |
| 命令同步输出 | 1 MiB | **32 KiB** |
| poll 单页输出 | 128 KiB | **32 KiB** |
| 列目录默认条数 | 200 | **100** |
| 搜索默认条数 | 100 | **50** |

原因是：ChatGPT 的对话上下文是有限的，一次读进 512 KiB 会把后面几十轮对话的额度提前烧光，而且工具返回的内容在文本和结构化两处各占一份。**上限调小，同时给它更省的工具去拿同样的信息**——这就是新增那三个导航工具存在的理由。下面有实测数字。

---

## 二、安装

### ⚠️ 先说一个会静默装错的坑

**不要用 `npm install` 装这个分支。**

`npm/postinstall.js` 里的下载地址写死指向**上游仓库的 release**：

```js
const releaseBaseUrl = `https://github.com/Xeift/CatDesk/releases/download/${releaseTag}`;
```

也就是说，即使你从这个 fork 走 npm 安装，它也会去下载**上游那个没改过的二进制**。整个过程不会报错，装完 `catdesk` 也能跑——但你拿到的还是 10 个工具的原版，而且很难反应过来哪里不对。

**必须从源码编译。**

### 编译

需要 Rust 工具链（[rustup.rs](https://rustup.rs)）。

```bash
git clone -b catdesk-agent-upgrade https://github.com/ericpq/CatDesk.git
cd CatDesk
cargo build --release
```

`-b catdesk-agent-upgrade` 不能省——默认分支 `main` 是上游原版。

编译产物在 `target/release/catdesk`（Windows 是 `catdesk.exe`）。首次全量编译约 5 分钟（含 5 个 tree-sitter 语法的 C 代码）。

### 运行

和原版一样：在你想让 ChatGPT 操作的目录里启动，然后把它显示的 MCP URL 填进 ChatGPT 的连接器。

```bash
cd /你的/工作目录
/路径/到/CatDesk/target/release/catdesk
```

**如果你之前装过原版的连接器**：ChatGPT 会缓存工具列表。换成新二进制后，很可能仍然只显示 10 个工具。解决办法是**把连接器删掉重新添加，然后开一个新会话**。加回来之后让它列一下工具名，看到 `outline`、`run_checks`、`checkpoint_restore` 就对了。

---

## 三、新能力怎么用

大部分时候你不用手动指定工具——操作指引里已经告诉模型什么时候该用哪个。下面是它们各自在解决什么，以及你可以明确要求它做什么。

### 3.1 代码导航：读不完的文件

原版遇到大文件只能 `read`，超过上限就被截断，模型看到半个文件然后开始猜。

**实测**（对象是 CatDesk 自己的 `src/mcp.rs`，317,864 字节）：

| 做法 | 结果 |
|---|---|
| `read` | 返回 32,768 字节，`truncated: true` — **只看到 10%** |
| `outline` | **264 个符号，85 毫秒，14.9 KB** — 全文件的结构，只占 4.7% |
| `find_symbol handle_run_checks` | 79 毫秒，定位到 `mcp.rs:2530` |
| `read_symbol handle_run_checks` | 第 2530–2668 行，5 KB — 只有那一个函数 |

三个工具：

- **`outline(path)`** — 列出一个文件里所有定义（函数、类型、类、方法、trait、interface）及其行号和嵌套关系。
- **`find_symbol(name, path?)`** — 在整个工作区找某个定义在哪。它会先用普通字符串过滤候选文件再解析，所以全库搜索也很快。
- **`read_symbol(path, name)`** — 只读出某一个定义的源码。

支持 Rust、Python、JavaScript、TypeScript、Go。

**用的是 tree-sitter 做真正的语法解析，不是正则。** 这不是炫技：括号匹配那类启发式会被字符串字面量里的 `{` 骗过去，而符号范围一旦算错，模型就会把修改发到错误的行上。仓库里有一条专门测这个场景的用例。

你可以直接说："先 outline 一下 src/xxx.rs，别整个读进来。"

### 3.2 `run_checks`：测试到底过没过

**这个工具存在的理由很具体**：如果让模型用 `run_command` 跑 `cargo test`，输出缓冲只保留**前** 32 KiB，而测试框架的结论打印在**最后**——恰好是被丢掉的那部分。模型会看到几百行 `... ok` 然后不知道最终结果。

`run_checks` 在服务端抓最多 4 MiB 输出、自己解析，只把结论返回给模型：通过/失败计数、失败用例名、`file:line` 诊断。

支持 cargo / pytest / go / node+tsc，按项目的标志文件自动识别，也可以显式指定 `command`。

**一个真实例子**（就是用这个工具跑 CatDesk 自己的测试）：

```
success: false
passed: 256
failed: 1
exitCode: 101
failures[0]: checkpoints::tests::checkpoints_are_listed_newest_first_and_can_be_restored_by_id
             → src/checkpoints.rs:471
```

它精确指出了失败的用例和行号——那次确实是代码里的一个真 bug（检查点排序在同一毫秒内会退化成任意顺序），已经修掉了。

**一个刻意的设计**：测试挂了算**成功的调用返回红色结论**，不算工具错误。只有"根本没跑起来"（命令不存在、超时）才报错。否则一次失败的测试会被模型当成传输故障去重试。

### 3.3 检查点：改错了能拿回来

`write`、`edit`、`delete`、`apply_patch` 在动手之前，会先把要覆盖的文件存一份。

- **`checkpoint_list`** — 看最近的记录（最新的在前）
- **`checkpoint_restore`** — 把文件恢复成某个检查点当时的样子，不给 id 就是撤销最近一次

为什么不直接用 git：工作区经常不是干净的 git 树，甚至根本不是仓库。检查点不依赖这些。

**重要边界：`run_command` 不在覆盖范围内。** shell 命令会碰什么，在它跑起来之前是无法知道的，所以没做兜底。**让模型做有风险的改动时，优先让它走编辑工具而不是 shell。**

上限：单文件 4 MiB、单次 500 个条目 / 16 MiB、保留最近 12 个 / 总共 128 MiB。超出的会被标记为"未捕获"并如实告诉你，不会假装能恢复。

---

## 四、哪些命令会被拒绝

这是你**最可能感到意外**的地方，所以单独列出来。

原版对 shell 命令没有任何限制。本分支在代码层面拒绝这些会丢失工作的写法：

| 被拒绝 | 仍然可用 |
|---|---|
| `git reset --hard` / `--merge` / `--keep` | `git reset --soft`、`git reset HEAD <file>` |
| `git checkout <路径>` / `git checkout .` / `-f` | `git checkout -b`、`git switch` |
| `git restore <路径>` | `git restore --staged` |
| `git clean`（无 `--dry-run`） | `git clean -n` / `--dry-run` |
| `git stash` / `push` / `save` / `drop` / `clear` | `git stash list` / `show` / `apply` / `pop` |
| `git branch -D` | `git branch -d` |
| `git push --force` / `-f` / `--force-with-lease` | `git push` |
| `rm -r` / `-rf` | `rm <单个文件>`、`delete` 工具 |

设计上的取舍：**只拦真正会丢工作的形式，不拦整个子命令**。切分支、取消暂存、只读地查看 stash，这些日常操作都必须还能用，否则这工具没法拿来正经开发。

被拒绝时会返回明确的原因和替代做法，模型一般会自己改用对应的专用工具。

**另外**：`apply_patch` 的补丁路径会先检查 `diff --git`、`---`/`+++`、`rename`、`copy` 各类头部，确认不越出工作区，再跑一次 `git apply --check` 试运行，最后才真正应用。

---

## 五、已知边界

诚实说明，免得你踩了以为是自己配错了：

1. **只在 Linux ARM64 上构建和测试过。** 代码本身应该是跨平台的（上游支持 5 个目标，新增的三个模块没用任何操作系统特定 API），但 Windows / macOS 没验证过。如果在 Windows 上遇到问题，最可能是这几处：`run_checks` 的 Python 默认命令是 `python3 -m pytest`（Windows 通常要写 `python`）、Git 工具的参数转义用的是 POSIX 单引号、命令拦截走的是 bash 语法解析。这些都可以用显式 `command` 参数绕过。

2. **`run_command` 没有检查点覆盖**（见 3.3）。

3. **`clippy` 没在 CI 里跑过**，只有 `cargo test` 和 `cargo fmt`。当前状态：258 个测试通过，0 警告。

4. **`docs/AGENT_UPGRADE.md` 那份文档是特定机器的部署笔记**，里面的路径和 SHA 是另一台服务器的，你可以只看它的 "Tools" 和 "Safety" 两节，其余忽略。

5. **这是上游的一个分叉。** 上游 [Xeift/CatDesk](https://github.com/Xeift/CatDesk) 仍在活跃更新，本分支不会自动跟进。

---

## 六、一句话总结

原版给的是"手"——能读能写能跑命令。这个分支想补上的是"眼睛"（看得懂大文件）、"验证"（知道改对没有）和"后悔药"（改错了能退回来）。工具变多了，但单次读取上限反而更小，这是刻意的：**把省下来的上下文额度，花在让它看对地方上。**
