# 50 — Redis 键空间与控制台：相对层级、批量删除、来自服务端的补全

> 状态：**已实施**（2026-09-29）。基线 master `73c2e70`（docs/49 上线之后）。同日第二轮按 owner 的
> 「每个极端情况」把一个敌意键空间在真机上走了一遍，坏掉的都修了——见 §6。
>
> 需求原文（owner，2026-09-29，在 docs/49 的流过滤上线之后连着提的三条）：
>
> 1. 「还有这个 key 的显示，你不应该每个层级只显示一个部分吗？例如 `market:fast` 和 `market:ticks` 上级是
>    `market`，它们的子层级不应该是 `fast` 和 `ticks` 吗？怎么还要显示全部信息，完整的 key 在旁边显示不行？」
> 2. 「而且你这个折叠的，也没有批量删除功能，也没有事务什么的，反正很多东西都没有」
> 3. 「Redis 命令自动补全的功能，做得也很烂，没有 template，反正做得我不太满意，需要你来重构下」

## 0. 现状与缺口

三条都指向同一件事：redis 侧的东西是**照着 SQL 侧抄过来的**，抄到一半就停了。

- **树的每一行都在重复它所在的那条路径。** `data-view.ts` 的 `dbRedisBand` 用**父带**的命名空间做前缀
  （`prefix = parentNs ? parentNs + ":" : ""`），所以 `market` 带里的行还挂着 `market:` 本身：带头写着
  `market`，下面三行写着 `market:fast`、`market:ticks`、`market:slow`。侧栏本来就只有 220px 宽，
  一个 `swiss:runs:remote:2026:...` 这样的键，看得见的部分全是它和邻居共有的那一段。
- **折叠带的 ⋯ 里只有排序。** 一个命名空间下 40 个测试键，要清掉只能一个一个点。`redis-pipeline`
  路由（docs/22 W3.3）早就在了，没人把它接到树上。
- **补全是面板里手写的一张表。** 2026-09-28 那次（`5be034a` 的前一步）留下的是 40 条命令名、没有摘要、
  没有参数，只在**第一个词**上补全——键补不了，`NX` / `MATCH` / `WITHSCORES` 补不了，`XINFO` 的子命令
  补不了。templates 是 ⋯ 菜单里收藏和历史下面的一节，owner 直到提这条需求都没找到它（「没有 template」）。
  这张表还必然会**说谎**：它不知道对面这台 redis 是 6 还是 7，有没有装 JSON / Search 模块。

## 1. 决定

| # | 决定 | 理由 |
| --- | --- | --- |
| D1 | 树里每一行只显示它在**本层**新增的那一段，完整 key 放右槽 | 这就是 owner 的原话。左边那一列是用来区分兄弟节点的，共有的前缀在带头上已经写了一遍 |
| D2 | 批量删除挂在折叠带的 ⋯ 上，按命名空间整段删 | 折叠带就是"这一组"的自然边界；要删的东西已经被这条带圈好了 |
| D3 | 一次 `DEL` 多个键**本身就是一条命令、就是原子的**；超过 1000 个键才分片，并在确认框里说明这时只保证一个往返 | 这是对"没有事务"最诚实的回答。`MULTI/EXEC` 在这里不会让它更原子（见 §3），只会让确认框上多一句假承诺 |
| D4 | SCAN 还没走完时，确认文案换一句：删的是**已经加载的**那些 | 树里有 200 个不等于库里只有 200 个。把"我只能保证这些"写进问句，比事后解释便宜 |
| D5 | 补全的来源换成**服务端自己**：`COMMAND DOCS` + `COMMAND INFO`，面板不再维护命令表 | 与 house rule 同一条（`swiss-ui-privacy-and-libraries`：用成熟的来源，不要手写）。redis 自己知道它有哪些命令、哪些参数、哪个位置是键——问它一次就够 |
| D6 | 键的位置由 `firstKey / lastKey / step` 决定，容器命令拍平成 `XINFO STREAM` 这样的一行 | 这正是 `redis-cli` 判断"哪个词是键"的三个数字。拍平是因为人打的是 `XINFO STREAM`，没人打 `XINFO` |
| D7 | templates 从 ⋯ 里搬出来，做成 Run 旁边的独立按钮 | 一个用来回答"这命令长什么样"的入口，藏起来就等于没有 |
| D8 | **值永远不猜**：键位置补键，其余位置补 token，值的位置什么都不补 | 在值的位置列出一串别的键名是纯噪音，还会盖住正在输入的内容 |

## 2. 设计

### 2.1 相对层级（`panel/src/data-view.ts:1239` `dbRedisBand`、`:1095` `dbKeyRow`）

一处改动，两个位置：

- 前缀从**父带**换成**本带**：`const prefix = g.ns ? g.ns + ":" : ""`。`market` 带里的行于是是
  `fast` / `ticks` / `slow`。
- 带本身也按同样的规则命名（`label: under`，`under` = 去掉父带的前缀），而**折叠状态的 key 仍然是完整
  路径**——收起 `market:fast` 不会连带收起别处的 `fast`。
- 单键叶子（一个命名空间下只有一个键）走的是同一条路：`dbKeyRow(r.keys[0], r.keys[0].key.slice(prefix.length), …)`。
- 右槽（`db-table-meta`）在**名字只是 key 的一段时**显示完整 key，在根层（名字就是 key 本身）显示类型。
  类型两种情况下都没丢：它是行首的字形，也在行的 `title` 里。

### 2.2 批量删除（`data-view.ts:1292` `keysUnder`、`:1308` `dbRedisDeleteKeys`）

- `keysUnder(g)` 递归收集这条带下的所有键（自己的 + 子带的），纯函数。
- ⋯ 菜单在排序项后加一条分隔线和一条 `danger` 项：**删除 N 个键**，N 是当场算出来的。
- 确认框两版：走完的 SCAN 用 `deleteKeysConfirm`，没走完用 `deleteKeysPartialConfirm`（多一句"只删
  已加载的"）。
- 命令按 `REDIS_DEL_CHUNK = 1000` 切片，每片一条 `DEL`，一次 `POST /redis-pipeline` 发出去。
- 删完：把开在这些键上的 tab 清空（tab 不会停在一个服务端已经没有的值上），`dbLoadKeys(true)` 重走，
  `renderDbGrid()` 重画。
- `apiJson` 失败时**什么都不动**——树不会因为一次失败的请求少掉几行。

### 2.3 命令目录（`swiss-mcp/src/adapters/redis.rs:935` `read_command_catalog`）

```
COMMAND DOCS  ->  parse_command_docs   （词：summary / syntax / group / since / tokens）
COMMAND INFO  ->  parse_command_info   （数：arity / firstKey / lastKey / step）
                        \/
                 merge_command_catalog  ->  GET /api/db/{name}/redis-commands
```

- **两条命令分开发**，不合进一个 pipeline：redis 6 不认识 `COMMAND DOCS`，而 redis 对 pipeline 里的
  失败是一起回的——合着发会把 `INFO` 一起拖下水。DOCS 失败就只用 INFO，答案里 `documented: false`。
- `parse_command_docs`（`:803`）把 RESP2 的扁平 map 递归解开，`arg_syntax`（`:733`）按 redis 自己的
  记法拼出参数行：`pure-token` 就是它的 token，`oneof` 用 `|` 连，`block` 顺着排，其余是
  `token name`；`optional` 加方括号，`multiple` 写成 `key [key ...]`。深度封顶 6，畸形的嵌套不会无限递归。
- 容器命令（`XINFO`、`CONFIG`、`CLIENT`）**自己留一行**并标 `container: true`，子命令拍平成
  `XINFO STREAM` 这样的独立行——`COMMAND DOCS` 里它叫 `xinfo|stream`。
- `parse_command_info`（`:856`）也要**往下走一层**：redis 7 的每行有十个格子，第十个是这条容器的子命令，
  每个子命令又是一整行同样形状的数据（`xinfo|stream`，`first_key = 2`）。容器**自己**的三个数字是 0——
  `XINFO STREAM` 的键位置只存在于那层嵌套里。真机实测就是这么报的（`COMMAND INFO xinfo`），
  第一版把第 6 个格子当成行尾，于是 `XINFO STREAM` 的 `firstKey` 是 0，集成测试当场把它抓了出来。
- `merge_command_catalog`（`:892`）合并两半：任一半缺了另一半仍然发车（DOCS 有词没数、INFO 有数没词）；
  redis 6 不嵌套子命令，那里退回到**继承容器的键位置**（`XINFO STREAM key` 的键就在 `XINFO` 说的位置上）。
- 网关不缓存：面板每个连接问一次，连接活多久就留多久（`dbRedisLoadCommands`，`data-suggest.ts:217`）。

### 2.4 补全（`panel/src/data-suggest.ts:135` `dbRedisCandidates`）

补全读的是**光标所在的那一行**（`dbRedisLineAt`，`:91`）——控制台是多行的，拿整个框去切词会把上一行
当成这条命令的一部分。然后按光标所在的词序号分情况：

| 词序号 | 补什么 |
| --- | --- |
| 0 | 命令名（必须先有前缀：空字符串不该把整份目录倒出来） |
| 1，且第 0 词是容器 | 子命令，标签只写叶子（`STREAM`），补进去正好接上 |
| `dbRedisKeyAt` 判定为键位 | 侧栏已经走到的键名 |
| 其余 | 这条命令自己的 token（`NX`、`MATCH`、`WITHSCORES`），**已经打过的不再提** |

`dbRedisKeyAt(cmd, index, words)`（`:117`）就是 redis-cli 的那三个数字：`firstKey <= i <= lastKey`
（`lastKey` 为负数时从行尾往回数：`end = words + last`），且 `(i - first) % step == 0`。于是
`DEL k1 k2 k3` 每个词都是键，`MSET k v k v` 只有第 1、3 个词是键。

每个候选项的 `detail` 是 **`名字 + 语法 — 摘要`**：语法在前，因为那是接下来要打的东西。

### 2.5 签名行（`data-view.ts:579` `dbRedisHint`）

框下面那行原本是一句固定说明。现在只要正在打的命令能在目录里认出来，它就换成这条命令的
`名字 + 语法 — 摘要`（`dbRedisSignature`，`data-suggest.ts:191`）；认不出来（打了一半、根本不是命令）
就留着固定说明，不闪。

### 2.6 Templates 按钮（`panel/src/data-grid.ts` `dbRedisTemplateItems`）

- redis 控制台的工具栏里，Run **左边**多一个 `Templates`。
- 32 条模板，按 string / key / hash / list / set / zset / stream / server 分组，行本身就是标签，
  点了直接落进框里（`dbLoadConsoleLine`，顺手 `focus()`）。
- 模板是**手写**的，且应该手写：目录说的是一条命令**能接什么**，模板说的是**有人来这儿是要干什么**
  （`SET key value EX 60`、`SCAN 0 MATCH prefix:* COUNT 100`、`XREVRANGE key + - COUNT 20`）。
- ⋯ 菜单里只剩收藏和历史——那两份是操作者自己的东西。

## 3. 关于"事务"

owner 那句「也没有事务什么的」，落到批量删除上，正确的答案是**不需要事务**：

- **一条 `DEL k1 … kN` 本身就是原子的。** redis 单线程执行一条命令，不存在删到一半被别人插进来。
  把它拆成 N 条 `DEL` 再包一层 `MULTI/EXEC`，原子性一模一样，往返多一倍。
- **`MULTI/EXEC` 不是回滚。** 它保证的是"这批命令中间没有别人"，而**不**保证失败时撤销——
  `EXEC` 中途某条命令报错，前面的命令已经生效了。拿它当"事务"用，确认框上就会多一句做不到的承诺。
- **真正需要 `MULTI/WATCH` 的是读-改-写**（比如"读出计数器，加一，写回，期间没人动过"）。面板现在没有
  这种动作；等有了（docs/22 W3.3 的结构化编辑如果要做乐观并发），那才是引入 `WATCH` 的地方，
  届时它有自己的 spec。

超过 1000 个键的那一刀是唯一的例外，所以它被明确写进了确认框：**那时保证的是一个往返，不是一条命令**。

## 4. 测试

| 层 | 测试 |
| --- | --- |
| Rust 单测（swiss-mcp） | `command_docs_become_syntax_lines_and_tokens`（SET 的 `key value [NX\|XX] [EX seconds\|KEEPTTL]`、`DEL` 的 `key [key ...]`、token 列表）、`a_container_command_contributes_its_subcommands_as_rows`、`command_info_gives_the_key_positions_and_merges_with_the_words`（含 redis 7 的嵌套子命令行、redis 6 的继承回退、INFO-only 行）、`a_catalog_survives_a_server_that_documents_nothing`（乱七八糟的输入给空目录，不 panic） |
| 集成（swiss-it，真 redis 7） | `the_command_catalog_comes_from_the_server_itself`：`documented: true`、几百条命令、`SET` 的摘要和 `[NX\|XX]`、`DEL` 的 `lastKey: -1`、`MSET` 的 `step: 2`、`XINFO` 是容器而 `XINFO STREAM` 的 `firstKey` 是 2 |
| 路由 | `GET /api/db/{name}/redis-commands`（`dbbrowser_api.rs:1217`） |
| 面板 vitest | `admin-data-suggest.test.ts`：以服务端目录为 fixture 的 16 条——命令词、每个键位、容器子命令、token 去重、什么都不该补的位置、多行只读当前行、`dbRedisKeyAt` 的三数字、签名行；`db-tree.test.ts`：相对层级的标签、右槽的完整 key、`keysUnder`、分片的 `DEL`、部分加载的确认文案、删完清 tab |

## 5. 没做

- **不做服务端缓存的目录**：一个连接一次，够了。
- **不做参数级校验**（"这里应该是整数"）：redis 自己会报错，且报得比面板准。
- **不做模板参数占位符**（`<key>`）：要删的尖括号比要填的值还多，`SET key value` 直接改就是。
- **不做 `MULTI/EXEC`**：理由见 §3。

## 6. 极端情况（2026-09-29，第二轮）

> 需求原文（owner）：「每个极端情况，你都考虑到了吗？例如很多 key 都是同样的前缀的，还有其他的，反正有很多
> 极端的情况，希望你在这里测试下」「各个极端情况，你都需要考虑清楚」

做法：先写一个**敌意键空间**（5,110 个键，见下），在 19998 上用无头 Edge 从头走一遍，再把每个坏掉的地方
钉成单测。下表每一行都是**真的坏过**的，不是推演出来的。

敌意键空间：同一前缀下 5,000 个键（`hot:session:user:N`）加一个必须活下来的兄弟 `hot:config`；
`:lead`、`::deep`、`a::b`、`foo:` 这类开头/连续/结尾的冒号；`constructor:x`、`__proto__:x`、
`toString:1`；空名 `""`、` x`、`x `、`my key` 与它的两半 `my`、`key`；引号、反斜杠、换行、制表符；
`用户:1`、`🔑:a`、`#tag:1`；一个 5,000 字符的名字；60 层的台阶 `deep:deep:…:x`；
以及三个**不是 UTF-8** 的名字（Spring JDK 序列化器写出来的 `\xac\xed\x00\x05t…session:one`、同前缀的
`…session:two`、单独的 `\xff\xfe`）。

### 6.1 树

| 情况 | 坏成什么样 | 现在 |
| --- | --- | --- |
| 一条带下 20 万个键 | `out.push(...keysUnder(child))` 把整个列表当调用参数，过了约 12 万个 V8 就 `RangeError`，带头连数都画不出来 | `gatherKeys` 一个一个推（`data-view.ts:1311`） |
| 5,000 层的台阶、两个键共享 10 万个冒号 | `nsNode` 递归爆栈，之后每次走树都爆，整个 Data 页挂掉 | 深度封顶 `REDIS_NS_MAX_DEPTH = 32`（`data-tree.ts:130`），折叠也算一层；再往下的键都是最深那条带的行，名字是键的剩余部分 |
| 名字叫 `constructor` / `toString` / `__proto__` | 折叠表是普通对象，`collapsed["constructor"]` 回的是 Object 的构造函数——这条带永远是"已折叠"，打不开 | 折叠表无原型（`groups.ts` `loadCollapsed`）；分段本来就用 `Map` |
| ` x` 与 `x `、`line\nbreak`、零宽字符、U+202E | HTML 折叠空白，两个不同的键画成同一个 `x`；换行的名字读起来像 `line break`；U+202E 把名字后半段倒着画 | `dbKeyShown`（`data-tree.ts:121`）：会被页面或对话框悄悄改掉的名字按 redis-cli 的样子加引号转义，空名是 `""`。**只管显示**：每个动作发的都是键自己的字节。树、tab 标题、网格标题、确认框、toast 都走它 |
| 不是 UTF-8 的名字 | 网关把回复转成 JSON 时它变成 U+FFFD——一个任何命令都找不到的名字；点它打开的是空值，删它删的是另一个键（或者什么都不删） | 见 §6.4 |

### 6.2 批量删除

| 情况 | 现在 |
| --- | --- |
| 5,000 个同前缀的键 | 按 1,000 个一条切成 5 条 `DEL`，确认框说明这时保证的是一个往返；兄弟 `hot:config` 不在这条带里，活下来（真机走过：删完剩 0，`hot:config` 在） |
| 很长的名字 | 网关拒绝超过 2 MiB 的请求体（`reply.rs` `BODY_LIMIT`），长名字的带比 1,000 个键先撞上它：`dbRedisDelBatches` 另有每请求 1.5 MB 的 JSON 预算，超了就分成几个请求依次发；一个单独就超预算的键仍然发（独占一条命令）——拒发等于留下的恰好是操作者指着的那个 |
| SCAN 把同一个键给了两次（rehash 时 redis 只保证"至少一次"） | 键列表进来就去重（`dbUniqueKeys`），树、补全、`DEL` 参数都不会重复 |
| 连点两下删除 | `dbRedisDeleting` 挡住第二次——否则同一批键发两遍，报两组数字 |
| 删到一半别人先删了 | toast 报的是 **redis 回的** `DEL` 之和，不是请求的个数；差额单独说（`deletedNKeysGone`） |
| 第 3 个请求失败 | 停下，已经删掉的照实报，带上服务端给的原因；无论如何都重走一遍树 |

### 6.3 控制台与补全

| 情况 | 坏成什么样 | 现在 |
| --- | --- | --- |
| `DEL #tag:1` | Rust 的 shlex 2.0.1 把**词首的 `#`** 当注释，整行后半截被吞掉——服务端执行的是 `DEL`，redis 回参数个数不对 | `dbbrowser::split_words`（`dbbrowser.rs:926`）：切之前 `#`→0xFF，切完换回来。控制台和流过滤共用这一个函数（swiss-mcp 不再单独依赖 shlex） |
| 面板和服务端切词不一致 | 补全数的"第几个词"和 redis 实际看到的对不上 | 共用语料 `tests/fixtures/console-split.json`（39 行 + 14 个"键 → 应该怎么打"）：Rust 和 vitest 各读一遍、断言同样的结果。两个库有 3 处已知分歧（`"\$5"`、`$'a\tb'`、`\r`），语料里逐条写了 `why` |
| `GET 用户` 补全 | 旧的"光标前一串像键的字符"在 CJK 处断开，列出所有键，选中的键贴在 `用户` **后面** | `dbRedisWordSpan`（`data-suggest.ts:133`）：要替换的就是 shlex 切出来的那个词，起点是"从这儿往后恰好切成这一个词"的最近空白处 |
| 键名带空格 / 引号 / 反斜杠 | 补进去的是裸的 `my key`，服务端切成两个键 | `dbRedisQuoteArg`：切回来不变就裸写，否则双引号包起来、`"` 和 `\` 转义——面板、服务端、redis-cli 三方读回来都是同样的字节。带 `\r` / `\n` 的键不出现在补全里 |
| 贴进一个 100 KB 的 JSON 值 | 找词起点时对每个空白都把整行前缀重切一遍，O(n²)，**一次按键卡了 10 分钟以上** | 只切光标前的尾巴；回看最多 `REDIS_SPAN_LOOKBACK = 4096` 个字符；超过 `REDIS_LINE_MAX = 65536` 的行按空白数词、什么都不补 |
| `XREAD … STREAMS a b 0 0`、`ZUNION 2 a b`、`EVAL s 1 k` | 旧的 first/last/step 对这些命令说"没有键"（键的位置会动） | redis 7 的 key specs（`COMMAND INFO` 第 9 格，`parse_key_spec`）：从固定位置或关键字开始，按范围或按计数参数找键；`unknown` 类的 spec 丢掉，不替服务端猜 |
| 语法行里的 token | `[LIMIT offset count]` 印成 `[offset count]`；`SORT` 的 `GET` 不重复 | token 领头印在块、oneof 前面；`multiple_token` 时 token 跟着重复：`[GET pattern [GET pattern ...]]`（redis 7 真机上叫 `get-pattern`，集成测试按它断言） |

### 6.4 单个键上的动作

| 情况 | 坏成什么样 | 现在 |
| --- | --- | --- |
| 删 / 设 TTL / 改名 `my key` | 动作拼成一行字 `"DEL " + key` 走控制台路由，控制台按 shell 切词：删掉的是 `my` 和 `key`；名字里的反斜杠删的是另一个键 | `dbRedisExec(argv)` 走结构化的 `/redis-pipeline`，键是**一个参数**的原样字节（与控制台同一套守卫 `vet_pipeline`） |
| TTL 填 0 | `EXPIRE key 0` 当场删键——一次跳过了删除确认的删除 | sheet 和行内编辑器都拒绝 0，指向删除 |
| `EXPIRE` 回 0 | 仍然 toast "TTL 已设置"，对一个已经不存在的键 | `dbRedisTtlSaid`：0 就说键已经不在了 |
| 改名成已存在的名字 | `RENAME` 静默覆盖目标键的值 | `RENAMENX`，被占用就在输入框旁边说 |
| 改名 ` x`（首尾空白） | sheet 把值 trim 掉，改成了 `x` | sheet 新增 `exact`（`ui/sheet.ts:144`）：原样提交；只有空白仍算"必填" |
| 命令预览里以 `\` 结尾的值 | 印成 `"x\"`——一个没闭合的引号，贴进控制台就是另一条命令 | `dbRedisCommandText` 同时转义 `\` 和 `"` |
| 不是 UTF-8 的名字 | 见 §6.1 | SCAN 的名字保持**字节**（`call_raw` + `pipeline_args`，TYPE / TTL 问的是原样字节）；是 UTF-8 就照常，不是就按 redis-cli 的 `sdscatrepr` 印出来（`redis_cli_repr`，`redis.rs:1451`）并标 `binary: true`。面板上：它**永远留在根层**（印出来的文字里的 `:` 不是命名空间），右槽写"非 UTF-8"，点击只 toast "请用 redis-cli"，不开 tab；带删除、补全都跳过它。**不做**按字节寻址——那要给每个动作加一条二进制通道，而这种键在面板里只需要"看得见、不误伤" |

### 6.5 Runs 的取消

取消要等 run **真的停下**，这要几秒。这几秒里点去 Logs，停下的回复回来时 `render()` 把 Runs 列表画进了
`#pane`——盖在 Logs 上。`remote-runs.ts` 加了**访问号**：`mount` / `unmount` 都让它 +1，每个 `await`
之后号不对就什么都不写、什么都不画；取消的 toast 照样说（它是面板的，不是这一页的）。

### 6.6 测试

| 层 | 新增 |
| --- | --- |
| Rust 单测 | `a_word_leading_hash_is_a_character_not_a_comment`、`the_console_splits_as_the_shared_corpus_says`（swiss-host）；`a_name_that_is_not_utf8_prints_as_redis_cli_prints_it`、`a_token_leads_its_block_and_its_oneof_and_repeats_only_when_redis_says_so`、`key_specs_say_where_moving_keys_are_and_an_unknown_spec_is_dropped`（swiss-mcp） |
| 集成（真 redis 7，27 条全过） | `a_word_leading_hash_reaches_redis_as_itself`（`DEL` 4 个 `#` 开头的键回 4）、`a_key_name_that_is_not_utf8_is_walked_as_its_bytes_and_marked`（原始客户端写 `\xac\xed…user`，走出来恰好一行、标 binary、类型和 TTL 是这个键的） |
| 面板 vitest | `db-tree`：台阶、10 万个冒号、20 万键的带、原型名、首尾空白段、非 UTF-8 留在根层、`keysUnder` 跳过它、分请求的 `DEL`；`admin-data-suggest`：词的跨度、带空格的键补进去带引号、长行、共用语料；`data-look`：TTL 读数、`EXPIRE 0`、改名空白、空键名、点非 UTF-8 行；`db-tabs`：`""` 和 ` x` 的标题；`admin-remote-runs-view`：取消途中离开 |
| 真机（19998，无头 Edge） | 上面那个键空间：所有名字都画得出来；`用户` / `my key` 的补全；`#ff0000` 整个存进去；5,000 键的带删成 5 条 `DEL`、`hot:config` 留下；删 `my key` 后 `my`、`key` 都在；` x` 原样改名是空操作；TTL 0 被拒；非 UTF-8 名字有标记、点击被拒、redis 里原封未动；页面错误 0 |
