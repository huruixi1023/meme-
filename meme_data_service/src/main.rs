use axum::{
    extract::{Path, Query, State, WebSocketUpgrade},
    response::IntoResponse,
    routing::get,
    Json,
    Router,
};
use futures_util::{stream::StreamExt, SinkExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio::time::interval;

// 定义核心数据结构

// 交易数据结构体
// #[derive(Debug, Serialize, Clone)] 派生宏，方便调试打印、序列化为JSON、以及克隆实例
#[derive(Debug, Serialize, Clone)]
struct Transaction {
    token_id: String, // 代币的唯一标识符
    timestamp: u64,   // 交易发生时的UTC时间戳 (秒)
    price: f64,       // 交易价格
    amount: f64,      // 交易数量
}

// K线 (蜡烛图) 数据结构体
#[derive(Debug, Serialize, Clone)]
struct Kline {
    timestamp: u64, // K线区间的开始时间戳 (秒)
    open: f64,      // 开盘价
    high: f64,      // 最高价
    low: f64,       // 最低价
    close: f64,     // 收盘价
    volume: f64,    // 成交量
}

// K线API查询参数结构体
// 用于从HTTP GET请求的查询字符串中反序列化参数
#[derive(Debug, Deserialize)]
struct KlineParams {
    #[serde(default = "default_interval")] // 如果查询参数中没有interval，则使用default_interval函数的返回值
    interval: String, // K线的时间间隔字符串，例如 "1m", "5m", "1h"
}

// 提供默认的K线时间间隔值
fn default_interval() -> String {
    "1m".to_string() // 默认1分钟
}

// 将时间间隔字符串 (例如 "1m", "5s", "1h") 解析为对应的秒数
// 返回Option<u64>，如果解析成功则为Some(秒数)，否则为None
fn parse_interval_to_seconds(interval_str: &str) -> Option<u64> {
    // 获取字符串的最后一个字符，用于判断时间单位 (s, m, h, d)
    let last_char = interval_str.chars().last()?; // '?' 如果为空则提前返回None
    // 获取最后一个字符前的数字部分
    let num_str: String = interval_str.chars().take(interval_str.len() - 1).collect();
    // 将数字部分解析为u64整数
    let num = num_str.parse::<u64>().ok()?; // '.ok()?' 如果解析失败则转换为None并提前返回

    // 根据最后一个字符匹配时间单位并计算总秒数
    match last_char {
        's' => Some(num), // 秒
        'm' => Some(num * 60), // 分钟
        'h' => Some(num * 60 * 60), // 小时
        'd' => Some(num * 60 * 60 * 24), // 天
        _ => None, // 不支持的时间单位格式，返回None
    }
}

// 定义我们想要实时追踪其活动K线的标准时间间隔 (单位：秒)
// 例如，我们希望对于1分钟和5分钟的K线，能够获取到它们在当前时间周期内的最新状态
const ACTIVE_KLINE_INTERVALS: [u64; 2] = [60, 300]; // 60秒 (1分钟), 300秒 (5分钟)

// 应用的共享状态结构体
// 包含了所有在不同请求处理器和异步任务间需要共享的数据
struct AppState {
    transactions: Mutex<Vec<Transaction>>, // 存储所有历史交易记录的向量，使用互斥锁保护并发访问
    tx_tx: broadcast::Sender<Transaction>, // Tokio广播通道的发送端，用于向所有WebSocket客户端广播新交易
    // active_klines 用于存储活动K线数据
    // 结构为: 代币ID -> (活动K线开始时间戳 -> K线数据)
    // 同样使用互斥锁保护
    active_klines: Mutex<HashMap<String, HashMap<u64, Kline>>>,
}

// K线生成函数
// 根据提供的交易列表、时间间隔 (秒) 和代币ID过滤器，生成对应的K线数据列表
// 参数:
//   - transactions: 交易数据切片
//   - interval_seconds: K线的时间间隔，单位为秒
//   - token_id_filter: 需要筛选的代币ID
// 返回:
//   - Vec<Kline>: 生成的K线数据列表，按时间戳升序排列
fn generate_klines(transactions: &[Transaction], interval_seconds: u64, token_id_filter: &str) -> Vec<Kline> {
    // 如果交易列表为空或时间间隔为0，则直接返回空K线列表
    if transactions.is_empty() || interval_seconds == 0 {
        return Vec::new();
    }

    // 使用HashMap临时存储K线，键为K线区间的开始时间戳
    // 这样可以方便地聚合落在同一时间区间的交易
    let mut klines: HashMap<u64, Kline> = HashMap::new();

    // 遍历交易数据，只处理指定token_id_filter的交易
    for tx in transactions.iter().filter(|t| t.token_id == token_id_filter) {
        // 计算当前交易所属于的K线区间的开始时间戳
        // 例如，如果interval_seconds是60 (1分钟)，时间戳为65的交易属于开始时间戳为60的K线
        let interval_start_ts = tx.timestamp - (tx.timestamp % interval_seconds);

        // 获取或创建对应时间戳的K线实体
        let kline_entry = klines.entry(interval_start_ts).or_insert_with(|| Kline {
            timestamp: interval_start_ts, // K线开始时间
            open: tx.price,               // 区间第一笔交易的价格作为开盘价
            high: tx.price,               // 初始化最高价
            low: tx.price,                // 初始化最低价
            close: tx.price,              // 初始化收盘价 (会被后续交易更新)
            volume: 0.0,                  // 初始化成交量
        });

        // 更新K线的最高价、最低价、收盘价和成交量
        kline_entry.high = kline_entry.high.max(tx.price); // 最高价取当前最高价与交易价格中的较大者
        kline_entry.low = kline_entry.low.min(tx.price);   // 最低价取当前最低价与交易价格中的较小者
        kline_entry.close = tx.price;                      // 收盘价更新为当前交易的价格
        kline_entry.volume += tx.amount;                   // 累加成交量
    }

    // 将HashMap中的K线数据收集到Vec中
    let mut sorted_klines: Vec<Kline> = klines.into_values().collect();
    // 按K线的时间戳升序排列
    sorted_klines.sort_by_key(|k| k.timestamp);
    sorted_klines // 返回排序后的K线列表
}

// Axum HTTP GET请求处理函数，用于获取K线数据
// 路径: /klines/:token_id
// 参数:
//   - Path(token_id): 从URL路径中提取的代币ID
//   - State(state): 共享的应用状态
//   - Query(params): 从URL查询字符串中提取的参数 (例如 interval)
// 返回:
//   - Result<Json<Vec<Kline>>, String>: 成功则返回K线数据的JSON表示，失败则返回错误信息字符串
async fn get_klines(
    Path(token_id): Path<String>, // 从路径中提取 token_id
    State(state): State<Arc<AppState>>, // 注入共享状态
    Query(params): Query<KlineParams>,  // 注入查询参数
) -> Result<Json<Vec<Kline>>, String> { // Axum期望Handler返回类型能通过IntoResponse转换为响应
    // 解析查询参数中的interval字符串为秒数
    let interval_seconds = parse_interval_to_seconds(&params.interval)
        .ok_or_else(|| format!("无效的时间间隔格式: {}. 支持的格式例如: 1s, 1m, 5m, 1h, 1d", params.interval))?; // 若解析失败，返回错误信息
    
    // 检查时间间隔是否为零
    if interval_seconds == 0 {
        return Err("时间间隔不能为零".to_string()); // 返回错误信息
    }

    // 用于存储从历史交易中生成的K线
    let mut historical_klines: Vec<Kline>;
    {
        // 加锁读取共享的交易历史数据
        // 这个花括号限制了transactions_guard的作用域，使得锁能尽快被释放
        let transactions_guard = state.transactions.lock().unwrap(); // .unwrap() 在Mutex中毒时会panic
        historical_klines = generate_klines(&transactions_guard, interval_seconds, &token_id);
    }

    // 如果请求的K线间隔是需要实时追踪的活动K线间隔之一
    if ACTIVE_KLINE_INTERVALS.contains(&interval_seconds) {
        // 加锁读取活动K线数据
        let active_klines_guard = state.active_klines.lock().unwrap();
        // 检查是否存在指定代币的活动K线映射
        if let Some(token_active_map) = active_klines_guard.get(&token_id) {
            // 获取当前时间的Unix时间戳 (秒)
            // 注意: 这里的 "当前时间" 实际上应该与最新交易的时间戳相关，或者服务器的当前时间
            // 对于模拟数据，使用服务器当前时间是一个可接受的简化
            let now_ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
            // 计算当前时间所属的活动K线区间的开始时间戳
            let current_interval_start_ts = now_ts - (now_ts % interval_seconds);

            // 尝试获取对应开始时间戳的活动K线
            if let Some(active_kline) = token_active_map.get(&current_interval_start_ts) {
                // 如果历史生成的K线列表不为空
                if let Some(last_historical_kline) = historical_klines.last_mut() {
                    // 并且历史K线的最后一条与当前活动K线是同一个时间区间的K线
                    if last_historical_kline.timestamp == active_kline.timestamp {
                        // 用最新的活动K线数据替换历史K线列表中的最后一条
                        // 这样就实现了"未闭合K线"的实时更新效果
                        *last_historical_kline = active_kline.clone();
                        // 可以取消注释以下行进行调试，观察活动K线替换情况
                        // tracing::debug!("用活动K线替换了历史K线 {} for token {}", params.interval, token_id);
                    } else if active_kline.timestamp > last_historical_kline.timestamp {
                        // 如果活动K线的时间戳比历史最后一条更新
                        // (例如，服务刚启动，历史数据还不足以覆盖当前的活动K线区间)
                        // 则将此活动K线添加到列表末尾
                        historical_klines.push(active_kline.clone());
                        // tracing::debug!("添加了新的活动K线 {} for token {}", params.interval, token_id);
                    }
                } else {
                    // 如果历史K线列表为空 (例如，对于一个全新的代币，或者请求的时间间隔非常短，历史交易中还没有数据)
                    // 并且我们有一个对应的活动K线，则直接使用这个活动K线
                    historical_klines.push(active_kline.clone());
                    // tracing::debug!("历史K线为空，使用了活动K线 {} for token {}", params.interval, token_id);
                }
            }
        }
    }
    // 返回成功响应，将K线数据列表序列化为JSON
    Ok(Json(historical_klines))
}

// WebSocket 连接处理器
// 当有客户端请求升级到WebSocket协议时，此函数被调用
async fn websocket_handler(
    ws: WebSocketUpgrade, // Axum提供的WebSocket升级器
    State(state): State<Arc<AppState>>, // 注入共享状态
) -> impl IntoResponse { // 返回一个可以被转换为HTTP响应的类型
    // 调用 on_upgrade 将HTTP连接升级为WebSocket连接，并传入处理具体流的异步函数 websocket_stream
    ws.on_upgrade(|socket| websocket_stream(socket, state))
}

// 处理单个WebSocket连接数据流的异步函数
// 参数:
//   - stream: 建立的WebSocket连接流
//   - state: 共享的应用状态
async fn websocket_stream(stream: axum::extract::ws::WebSocket, state: Arc<AppState>) {
    // 将WebSocket流分割为发送端(sender)和接收端(receiver)
    let (mut sender, mut receiver) = stream.split();
    // 订阅应用状态中的交易广播通道 (tx_tx)
    // 每个连接的客户端都会得到一个独立的接收器 (rx)
    let mut rx = state.tx_tx.subscribe();

    // 派生一个异步任务，专门用于将广播通道接收到的交易转发给当前WebSocket客户端
    tokio::spawn(async move {
        // 循环等待从广播通道接收新的交易 (tx)
        while let Ok(tx) = rx.recv().await {
            // 将接收到的Transaction对象序列化为JSON字符串
            if let Ok(json_tx) = serde_json::to_string(&tx) {
                // 通过WebSocket发送JSON字符串给客户端
                // 如果发送失败 (例如客户端已断开连接)，则跳出循环，任务结束
                if sender.send(axum::extract::ws::Message::Text(json_tx)).await.is_err() {
                    break; // 客户端断开连接
                }
            }
        }
    }); // 这个任务会一直运行，直到发送错误或通道关闭

    // 当前WebSocket连接的主任务，主要负责处理从客户端接收的消息
    // 在本应用中，我们主要从服务器向客户端发送数据，所以这里只简单检查客户端是否发送了关闭消息
    while let Some(Ok(msg)) = receiver.next().await {
        if let axum::extract::ws::Message::Close(_) = msg {
            // 如果收到客户端的关闭消息，则跳出循环
            break; // 客户端主动关闭连接
        }
        // 此处可以添加逻辑处理客户端发送的其他类型消息，如果需要的话
    }
    // 当循环结束 (客户端断开或发送关闭消息)，可以选择性地记录日志
    tracing::debug!("WebSocket 客户端已断开连接");
}

// 主函数，应用程序的入口点
#[tokio::main] // 声明这是一个异步主函数，使用Tokio运行时
async fn main() {
    // 初始化 tracing subscriber，用于日志和追踪输出
    // tracing_subscriber::fmt::init(); 默认配置，输出到控制台
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG) // 设置最大日志级别为DEBUG，可以看到更多信息
        .init();

    // 创建一个Tokio广播通道，用于分发Transaction对象
    // 通道容量为100，如果发送速度超过接收速度，旧消息可能会被丢弃
    let (tx_tx, _rx_tx_prototype) = broadcast::channel::<Transaction>(100); // _rx_tx_prototype 是一个初始接收器，这里我们不直接用它

    // 创建共享状态实例，使用Arc使其可以在多线程/多任务间安全共享
    let shared_state = Arc::new(AppState {
        transactions: Mutex::new(Vec::new()), // 初始化空的交易历史列表
        tx_tx: tx_tx.clone(), // 克隆广播通道的发送端，用于在AppState中持有
        active_klines: Mutex::new(HashMap::new()), // 初始化空的活动K线存储
    });

    // 克隆共享状态的Arc指针，专用于模拟数据生成任务
    let app_state_for_generator = Arc::clone(&shared_state);
    // 派生一个异步任务，用于模拟生成交易数据
    tokio::spawn(async move {
        // 创建一个定时器，每隔1秒触发一次
        let mut tick_interval = interval(Duration::from_secs(1));
        // 定义一组模拟的代币ID
        let token_ids = vec!["MEMEALPHA".to_string(), "TOKENBETA".to_string(), "COINGAMMA".to_string()];
        // 用于轮流选择代币的索引
        let mut token_index = 0;
        // 存储每个代币的当前价格，使用HashMap
        let mut prices: HashMap<String, f64> = HashMap::new();
        // 初始化每个代币的起始价格 (50到150之间的随机数)
        for token_id_in_gen in &token_ids {
            prices.insert(token_id_in_gen.clone(), 50.0 + (rand::random::<f64>() * 100.0));
        }

        // 无限循环，模拟持续生成交易
        loop {
            tick_interval.tick().await; // 等待定时器触发
            // 获取当前UTC时间戳 (秒)
            let current_timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap() // .unwrap() 在时间倒退等罕见情况可能panic
                .as_secs();
            
            // 选择当前要生成交易的代币ID
            let current_token_id = token_ids[token_index].clone();
            // 获取或初始化当前代币的价格
            // .or_insert(100.0) 如果代币首次出现，则价格为100.0 (理论上前面已初始化，此处为保险)
            let current_price_ref = prices.entry(current_token_id.clone()).or_insert(100.0);
            
            // 模拟价格波动：在当前价格的 +/- 1% 范围内随机变动
            // (rand::random::<f64>() - 0.5) 生成 [-0.5, 0.5) 的随机数
            // * 2.0 转换为 [-1.0, 1.0)
            // * (*current_price_ref * 0.01) 计算价格1%的幅度
            let price_change = (rand::random::<f64>() - 0.5) * 2.0 * (*current_price_ref * 0.01);
            *current_price_ref += price_change; // 更新价格
            // 确保价格不低于1.0
            if *current_price_ref < 1.0 { *current_price_ref = 1.0; }

            // 创建新的交易对象
            let new_transaction = Transaction {
                token_id: current_token_id.clone(),
                timestamp: current_timestamp,
                price: *current_price_ref,
                amount: rand::random::<f64>() * 10.0, // 交易量为0到10之间的随机数
            };
            
            // 1. 将新交易存储到历史交易列表
            // 加锁 -> 推入交易 -> 锁自动释放
            app_state_for_generator.transactions.lock().unwrap().push(new_transaction.clone());
            
            // 2. 更新活动K线数据
            { // 使用花括号限制active_klines_guard的作用域，尽早释放锁
                let mut active_klines_guard = app_state_for_generator.active_klines.lock().unwrap();
                // 遍历所有需要追踪的活动K线时间间隔 (例如1分钟和5分钟)
                for &interval_sec in ACTIVE_KLINE_INTERVALS.iter() {
                    // 计算新交易所属于的活动K线区间的开始时间戳
                    let interval_start_ts = new_transaction.timestamp - (new_transaction.timestamp % interval_sec);
                    
                    // 获取或创建当前代币的活动K线映射 (token_id -> [interval_start_ts -> Kline])
                    let token_active_klines = active_klines_guard
                        .entry(new_transaction.token_id.clone())
                        .or_insert_with(HashMap::new); // 如果该代币还没有活动K线记录，则创建一个新的空HashMap
                    
                    // 获取或创建对应时间戳的活动K线实体
                    let active_kline_entry = token_active_klines
                        .entry(interval_start_ts) // 使用K线起始时间戳作为键
                        .or_insert_with(|| Kline { // 如果该时间戳的K线不存在，则用当前交易数据创建它
                            timestamp: interval_start_ts,
                            open: new_transaction.price,
                            high: new_transaction.price,
                            low: new_transaction.price,
                            close: new_transaction.price,
                            volume: 0.0,
                        });
                    
                    // 用新交易数据更新活动K线的 OHLCV 值
                    active_kline_entry.high = active_kline_entry.high.max(new_transaction.price);
                    active_kline_entry.low = active_kline_entry.low.min(new_transaction.price);
                    active_kline_entry.close = new_transaction.price; // 收盘价总是更新为最新交易价格
                    active_kline_entry.volume += new_transaction.amount; // 累加成交量

                    // 可选调试日志: 打印活动K线的更新情况
                    // tracing::debug!("Updated active kline for token {}, interval {}s, ts {}: {:?}", 
                    //    new_transaction.token_id, interval_sec, interval_start_ts, active_kline_entry);
                }
            } // active_klines_guard 在此释放锁
            
            // 3. 通过广播通道发送原始交易数据
            // .send() 会将交易发送给所有当前的订阅者 (WebSocket客户端)
            // 如果发送失败 (例如没有订阅者，或者通道已关闭)，则记录警告
            if let Err(e) = app_state_for_generator.tx_tx.send(new_transaction.clone()) {
                tracing::warn!("广播交易失败 (token {}): {}", new_transaction.token_id, e);
            }

            // 更新token_index，实现代币的轮流选择
            token_index = (token_index + 1) % token_ids.len();
        }
    }); // 模拟数据生成任务结束

    // 配置Axum应用的路由
    let app = Router::new()
        .route("/", get(root)) // 根路径"/"的GET请求由root处理函数处理
        .route("/klines/:token_id", get(get_klines)) // K线API路径，:token_id是路径参数
        .route("/ws", get(websocket_handler)) // WebSocket连接路径
        .with_state(shared_state); // 将前面创建的共享状态注入到应用中，使其对所有处理器可用

    // 定义服务监听的地址和端口
    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
    tracing::debug!("服务正在监听 {}", addr); // 记录日志：服务开始监听
    
    // 绑定TCP监听器到指定地址
    let listener = TcpListener::bind(addr).await.unwrap(); // .await等待绑定完成, .unwrap()在绑定失败时panic
    // 启动Axum服务，传入监听器和应用配置
    axum::serve(listener, app.into_make_service()) // .into_make_service() 将Router转换为服务
        .await // 等待服务运行结束 (通常是Ctrl+C中断)
        .unwrap(); // .unwrap() 在服务启动失败时panic
}

// 根路径 ("/") 的简单处理函数
// 返回一个静态字符串作为响应
async fn root() -> &'static str {
    "你好, Meme世界! 这是一个数据服务。" // 修改为中文问候
}

// 测试模块，使用 #[cfg(test)] 宏，只在执行 cargo test 时编译和运行
#[cfg(test)]
mod tests {
    use super::*; // 导入外部模块 (即本文件中的所有项) 以便在测试中使用

    // 测试 parse_interval_to_seconds 函数的有效输入
    #[test]
    fn test_parse_interval_to_seconds_valid() {
        assert_eq!(parse_interval_to_seconds("1s"), Some(1));
        assert_eq!(parse_interval_to_seconds("60s"), Some(60));
        assert_eq!(parse_interval_to_seconds("1m"), Some(60));
        assert_eq!(parse_interval_to_seconds("5m"), Some(300));
        assert_eq!(parse_interval_to_seconds("1h"), Some(3600));
        assert_eq!(parse_interval_to_seconds("2h"), Some(7200));
        assert_eq!(parse_interval_to_seconds("1d"), Some(86400));
    }

    // 测试 parse_interval_to_seconds 函数的无效输入
    #[test]
    fn test_parse_interval_to_seconds_invalid() {
        assert_eq!(parse_interval_to_seconds("1"), None); // 缺少单位
        assert_eq!(parse_interval_to_seconds("m"), None); // 缺少数字
        assert_eq!(parse_interval_to_seconds("1x"), None); // 未知单位
        assert_eq!(parse_interval_to_seconds(""), None); // 空字符串
        assert_eq!(parse_interval_to_seconds("60m1s"), None); // 格式错误
        assert_eq!(parse_interval_to_seconds("-1m"), None); // 负数，parse::<u64>() 会失败
        assert_eq!(parse_interval_to_seconds("1.5m"), None); // 小数，parse::<u64>() 会失败
    }

    // 测试 generate_klines 函数：单一代币，单一时间间隔
    #[test]
    fn test_generate_klines_single_token_single_interval() {
        let transactions = vec![
            Transaction { token_id: "MEMEALPHA".to_string(), timestamp: 0, price: 10.0, amount: 1.0 },
            Transaction { token_id: "MEMEALPHA".to_string(), timestamp: 10, price: 12.0, amount: 2.0 },
            Transaction { token_id: "MEMEALPHA".to_string(), timestamp: 20, price: 11.0, amount: 1.5 },
            // 另一时间间隔或代币的交易，用于测试过滤
            Transaction { token_id: "MEMEALPHA".to_string(), timestamp: 65, price: 15.0, amount: 3.0 }, // 属于 timestamp 60 的K线
            Transaction { token_id: "TOKENBETA".to_string(), timestamp: 5, price: 100.0, amount: 0.5 }, // 其他代币
        ];

        // 测试 MEMEALPHA, 1分钟 (60秒) 间隔
        let klines = generate_klines(&transactions, 60, "MEMEALPHA");

        assert_eq!(klines.len(), 2, "应为MEMEALPHA在60秒间隔生成2条K线");

        // 验证第一条K线 (时间戳 0-59)
        let kline1 = &klines[0];
        assert_eq!(kline1.timestamp, 0); // K线开始时间戳
        assert_eq!(kline1.open, 10.0);   // 开盘价
        assert_eq!(kline1.high, 12.0);   // 最高价
        assert_eq!(kline1.low, 10.0);    // 最低价
        assert_eq!(kline1.close, 11.0);  // 收盘价
        assert_eq!(kline1.volume, 1.0 + 2.0 + 1.5); // 成交量

        // 验证第二条K线 (时间戳 60-119)
        let kline2 = &klines[1];
        assert_eq!(kline2.timestamp, 60);
        assert_eq!(kline2.open, 15.0);
        assert_eq!(kline2.high, 15.0);
        assert_eq!(kline2.low, 15.0);
        assert_eq!(kline2.close, 15.0);
        assert_eq!(kline2.volume, 3.0);
    }

    // 测试 generate_klines 函数：多时间间隔和多代币的复杂场景
    #[test]
    fn test_generate_klines_multiple_intervals_and_tokens() {
        let transactions = vec![
            // MEMEALPHA 代币的交易
            Transaction { token_id: "MEMEALPHA".to_string(), timestamp: 1, price: 10.0, amount: 1.0 }, // 区间 1 (0-9 for 10s)
            Transaction { token_id: "MEMEALPHA".to_string(), timestamp: 5, price: 12.0, amount: 2.0 }, // 区间 1
            Transaction { token_id: "MEMEALPHA".to_string(), timestamp: 11, price: 15.0, amount: 3.0 },// 区间 2 (10-19 for 10s)
            Transaction { token_id: "MEMEALPHA".to_string(), timestamp: 18, price: 13.0, amount: 1.0 },// 区间 2
            // TOKENBETA 代币的交易
            Transaction { token_id: "TOKENBETA".to_string(), timestamp: 2, price: 100.0, amount: 0.1 },
            Transaction { token_id: "TOKENBETA".to_string(), timestamp: 8, price: 102.0, amount: 0.2 },
            Transaction { token_id: "TOKENBETA".to_string(), timestamp: 12, price: 99.0, amount: 0.3 },
        ];

        // 测试 MEMEALPHA, 10秒间隔
        let klines_alpha_10s = generate_klines(&transactions, 10, "MEMEALPHA");
        assert_eq!(klines_alpha_10s.len(), 2, "MEMEALPHA应生成2条10秒K线");
        // 验证 MEMEALPHA 的第一条10秒K线 (0-9秒)
        assert_eq!(klines_alpha_10s[0].timestamp, 0);
        assert_eq!(klines_alpha_10s[0].open, 10.0);
        assert_eq!(klines_alpha_10s[0].high, 12.0);
        assert_eq!(klines_alpha_10s[0].low, 10.0);
        assert_eq!(klines_alpha_10s[0].close, 12.0); // 区间 [0,9] 内的最后价格
        assert_eq!(klines_alpha_10s[0].volume, 3.0);

        // 验证 MEMEALPHA 的第二条10秒K线 (10-19秒)
        assert_eq!(klines_alpha_10s[1].timestamp, 10);
        assert_eq!(klines_alpha_10s[1].open, 15.0);
        assert_eq!(klines_alpha_10s[1].high, 15.0);
        assert_eq!(klines_alpha_10s[1].low, 13.0);
        assert_eq!(klines_alpha_10s[1].close, 13.0); // 区间 [10,19] 内的最后价格
        assert_eq!(klines_alpha_10s[1].volume, 4.0);

        // 测试 TOKENBETA, 10秒间隔
        let klines_beta_10s = generate_klines(&transactions, 10, "TOKENBETA");
        assert_eq!(klines_beta_10s.len(), 2, "TOKENBETA应生成2条10秒K线");
        // 验证 TOKENBETA 的第一条10秒K线 (0-9秒)
        assert_eq!(klines_beta_10s[0].timestamp, 0);
        assert_eq!(klines_beta_10s[0].volume, 0.1 + 0.2);
        // 验证 TOKENBETA 的第二条10秒K线 (10-19秒)
        assert_eq!(klines_beta_10s[1].timestamp, 10);
        assert_eq!(klines_beta_10s[1].volume, 0.3);
    }

    // 测试 generate_klines 函数：没有匹配代币ID的情况
    #[test]
    fn test_generate_klines_no_matching_token() {
        let transactions = vec![
            Transaction { token_id: "MEMEALPHA".to_string(), timestamp: 0, price: 10.0, amount: 1.0 },
        ];
        // 请求TOKENBETA的数据，而交易列表中只有MEMEALPHA
        let klines = generate_klines(&transactions, 60, "TOKENBETA");
        assert!(klines.is_empty(), "如果没有匹配的代币ID，应返回空K线列表");
    }

    // 测试 generate_klines 函数：交易列表为空的情况
    #[test]
    fn test_generate_klines_empty_transactions() {
        let transactions: Vec<Transaction> = vec![]; // 空交易列表
        let klines = generate_klines(&transactions, 60, "MEMEALPHA");
        assert!(klines.is_empty(), "如果交易列表为空，应返回空K线列表");
    }

    // 测试 generate_klines 函数：时间间隔为零的情况
     #[test]
    fn test_generate_klines_zero_interval() {
        let transactions = vec![
            Transaction { token_id: "MEMEALPHA".to_string(), timestamp: 0, price: 10.0, amount: 1.0 },
        ];
        // 时间间隔为0
        let klines = generate_klines(&transactions, 0, "MEMEALPHA");
        assert!(klines.is_empty(), "如果时间间隔为零，应返回空K线列表");
    }
} 