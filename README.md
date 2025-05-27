Meme 代币交易平台 - 数据服务

项目简介

这是一个基于 Rust 的数据服务，为 Meme 代币交易平台提供支持。 主要功能包括：

提供实时的 K 线 (蜡烛图) 数据，客户端可以按代币和时间间隔查询。活动的 K 线会持续更新，直至当前周期结束。
通过 WebSocket 实时广播模拟交易数据。
这些服务旨在支持前端的 K 线图和实时交易信息流等用户界面组件。

设置与运行

环境要求： 需要安装 Rust 及 Cargo。

运行步骤：

获取代码
进入项目根目录 meme_data_service。
执行 cargo run 命令编译并启动服务。
服务默认监听 127.0.0.1:3000。
API 文档

服务提供以下 API 端点：

K-线数据 API
用于获取指定 Meme 代币的 K 线数据。

端点: GET /klines/:token_id
路径参数: token_id (代币ID，例如 MEMEALPHA, TOKENBETA, COINGAMMA)。示例: /klines/MEMEALPHA
查询参数: interval (可选，K 线时间间隔，如 1s, 1m, 5m, 1h, 1d。默认 1m)。示例: /klines/MEMEALPHA?interval=5m
成功响应为 JSON 数组，包含 K 线对象 (timestamp, open, high, low, close, volume)。若无数据则返回空数组。活动 K 线 (当前为1分钟和5分钟间隔) 的最后一条会实时更新。 错误响应: 若 interval 参数无效或为零，返回纯文本错误信息。

实时交易流 API (WebSocket)
用于向客户端实时推送模拟交易数据。

端点: ws://127.0.0.1:3000/ws
连接: 客户端通过标准 WebSocket 协议连接。
推送消息: 服务器在新交易发生时，向所有客户端推送 JSON 格式的交易对象 (token_id, timestamp, price, amount)。
功能验证

为了确保服务按预期工作，可以执行以下步骤：

启动服务: 在项目根目录 `meme_data_service`下运行 `cargo run`。服务将在 `127.0.0.1:3000` 启动。

测试根路径 (可选): 在浏览器中打开 `http://127.0.0.1:3000/\`。 预期结果: 看到 "Hello, World!" 文本。

测试 K-线 API: 使用浏览器或 `curl` 等工具测试。

标准请求: `http://127.0.0.1:3000/klines/MEMEALPHA?interval=1m\` 预期结果: 返回 MEMEALPHA 代币的1分钟 K 线数据 (JSON 格式)。由于是模拟数据，列表可能为空或包含若干条目。
不同代币: `http://127.0.0.1:3000/klines/TOKENBETA?interval=5m\` 预期结果: 返回 TOKENBETA 代币的5分钟 K 线数据。
不同间隔: `http://127.0.0.1:3000/klines/COINGAMMA?interval=1s\` 预期结果: 返回 COINGAMMA 代币的1秒 K 线数据。
包含活动 K 线 (1分钟或5分钟间隔): 访问如 `http://127.0.0.1:3000/klines/MEMEALPHA?interval=1m\`。 预期结果: K 线列表的最后一条会是当前正在进行的1分钟 K 线，其收盘价、最高/最低价会随着新交易的产生而变化（刷新页面可见）。
不存在的代币: `http://127.0.0.1:3000/klines/NONEXISTENTTOKEN?interval=1m\` 预期结果: 返回空的 JSON 数组 `[]`。
无效间隔: `http://127.0.0.1:3000/klines/MEMEALPHA?interval=invalid\` 预期结果: 返回文本错误信息，例如 "Invalid interval format: invalid"。
零间隔: `http://127.0.0.1:3000/klines/MEMEALPHA?interval=0s\` 预期结果: 返回文本错误信息，例如 "Interval cannot be zero"。
测试 WebSocket 实时交易流:

确保 Rust 服务正在运行。
需要一个 WebSocket 客户端工具，例如 `websocat`。如果未安装，可以通过 `cargo install websocat` 安装。
在新的终端中运行: `websocat ws://127.0.0.1:3000/ws`
预期结果: 终端会持续打印出新生成的模拟交易数据 (JSON 格式)，每秒一条，包含 `token_id`, `timestamp`, `price`, `amount`。例如： `{"token_id":"MEMEALPHA","timestamp":...,"price":...,"amount":...}` `{"token_id":"TOKENBETA","timestamp":...,"price":...,"amount":...}` 以此类推。
架构概述

服务主要组件：

模拟数据生成器: 在 Tokio 异步任务中运行，为预定义代币生成模拟交易。新交易存入共享列表、更新活动K线，并通过 broadcast 通道分发。
共享状态 (AppState): 使用 Arc<Mutex> 等机制管理共享数据，包括历史交易 (transactions), 活动K线 (active_klines), 和交易广播通道发送端 (tx_tx)。
K 线聚合逻辑 (generate_klines 函数): 根据交易数据、时间间隔和代币ID，生成K线列表。
HTTP API 处理器 (Axum): 处理 /klines/:token_id 请求，调用 K 线聚合逻辑，并结合活动K线数据返回结果。
WebSocket 处理器: 处理 /ws 连接，订阅交易广播，并将接收到的交易序列化为 JSON 后发送给客户端。
数据流：

模拟交易 -> AppState (存储与更新) & 广播通道 -> WebSocket 客户端。
HTTP GET /klines -> 读取 AppState -> K 线聚合 -> HTTP 响应。
主要假设

内存存储: 交易和活动K线数据存储在内存中，服务重启后丢失。
模拟数据特性: 交易数据为模拟生成，价格基于简单随机模型。
K线生成: 时间戳为区间起始点。"未闭合K线"通过API请求时合并最新活动状态，非主动推送更新。
并发控制: 主要通过 Mutex。高并发写入可能成为瓶颈。
错误处理: 相对基础，主要覆盖API参数解析。
资源限制: 未显式限制客户端连接数或内存中交易数量。
潜在改进

持久化存储: 使用数据库 (如 PostgreSQL, TimescaleDB) 存储数据。
增强模拟数据: 引入更复杂的模拟模型。
主动推送活动K线更新: 通过 WebSocket 主动推送活动 Kline 对象更新。
WebSocket 按需订阅: 允许客户端订阅特定代币的交易流或K线更新流。
细化错误处理: 实现更健壮的错误处理和熔断机制。
性能优化: 考虑 RwLock，优化并发数据结构和异步流程。
配置化: 将端口、代币列表、活动K线间隔等参数移至配置文件。
增强测试: 增加集成和端到端测试。
Docker 容器化: 将服务打包为 Docker 镜像。
API 增强: 引入版本控制和安全机制 (认证授权)。
与大型平台集成

本服务可作为大型交易平台的数据微服务。

作为数据源: 为前端UI、策略回测系统、交易机器人等提供数据。
获取数据: 真实场景中应从交易所API或内部撮合引擎获取数据，再进行处理分发。
服务间通信: 通过 gRPC, REST API 或消息队列 (如 Kafka) 与其他服务交互。
客户端高效消费流数据

当前 WebSocket 会广播所有代币的交易给所有客户端，存在效率问题。

改进方向：

服务器端过滤/按主题订阅: 客户端指明感兴趣的代币，服务器按需推送。
增量更新/数据压缩: 减少数据传输量，使用更高效的序列化格式。
客户端SDK: 封装连接和消息处理逻辑，简化客户端集成。
背压处理: 管理服务器推送与客户端处理速度不匹配的情况。
另外的总结：

关于 Docker 容器化（meme.md 中的加分项），这次由于时间关系未能完成。如果后续要实现，大致思路如下： 首先，需要准备一个 Dockerfile。推荐使用 Rust 官方的多阶段构建镜像，比如先用 rust作为构建环境，编译出 release版本的可执行文件。然后，在一个非常轻量的运行时镜像（如 alpine）中，仅复制这个编译好的二进制文件进去，这样最终镜像会小很多。 之后，用 docker build 命令构建镜像，再用 docker run 就能跑起来了。如果服务多起来，用 docker-compose.yml 管理会更方便。 这样做之后，整个服务就能打包成一个独立的 Docker 镜像，部署和移植都会简单很多。
