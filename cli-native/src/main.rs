// 麦门终端 McGacha CLI —— 原生控制台版，直连麦当劳 MCP，零运行时依赖
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};

const MCP_URL: &str = "https://mcp.mcd.cn/mcp-servers/mcd-mcp";

// ---------- ANSI 颜色 ----------
mod color {
    pub const RESET: &str = "\x1b[0m";
    pub const DIM: &str = "\x1b[90m";
    pub const YELLOW: &str = "\x1b[93m";
    pub const GREEN: &str = "\x1b[92m";
    pub const RED: &str = "\x1b[91m";
    pub const CYAN: &str = "\x1b[96m";
}
use color::*;

#[cfg(windows)]
fn enable_ansi() {
    extern "system" {
        fn GetStdHandle(n: i32) -> isize;
        fn GetConsoleMode(h: isize, mode: *mut u32) -> i32;
        fn SetConsoleMode(h: isize, mode: u32) -> i32;
    }
    unsafe {
        let h = GetStdHandle(-11);
        let mut m: u32 = 0;
        if GetConsoleMode(h, &mut m) != 0 {
            SetConsoleMode(h, m | 0x4);
        }
    }
}
#[cfg(not(windows))]
fn enable_ansi() {}

fn p(text: &str, c: &str) {
    for line in text.split('\n') {
        println!("{}{}{}", c, line, RESET);
    }
}
fn hr() {
    p("--------------------------------------------------------", DIM);
}

// ---------- MCP 客户端 ----------
struct Mcp {
    token: String,
    session: Option<String>,
    seq: u64,
    connected: bool,
}

impl Mcp {
    fn new(token: String) -> Self {
        Mcp { token, session: None, seq: 0, connected: false }
    }

    fn rpc(&mut self, method: &str, params: Option<Value>, notify: bool) -> Result<Value, String> {
        let id = if notify { None } else { Some({ self.seq += 1; self.seq }) };
        let mut body = json!({ "jsonrpc": "2.0", "method": method });
        if let Some(i) = id { body["id"] = json!(i); }
        if let Some(pa) = params { body["params"] = pa; }

        let mut req = ureq::post(MCP_URL)
            .set("Content-Type", "application/json")
            .set("Accept", "application/json, text/event-stream")
            .set("Authorization", &format!("Bearer {}", self.token))
            .timeout(std::time::Duration::from_secs(30));
        if let Some(s) = &self.session {
            req = req.set("Mcp-Session-Id", s);
        }

        let res = match req.send_string(&body.to_string()) {
            Ok(r) => r,
            Err(ureq::Error::Status(code, r)) => {
                let text = r.into_string().unwrap_or_default();
                if code == 401 || code == 403 {
                    return Err(format!("鉴权失败（HTTP {}）：Token 无效或已过期，用 mc token 重新配置", code));
                }
                return Err(format!("HTTP {} {}", code, &text[..text.len().min(200)]));
            }
            Err(e) => return Err(format!("网络请求失败：{}", e)),
        };

        if let Some(s) = res.header("mcp-session-id") {
            self.session = Some(s.to_string());
        }
        if notify || res.status() == 202 {
            return Ok(Value::Null);
        }

        let text = res.into_string().map_err(|e| e.to_string())?;
        let obj = parse_rpc_body(&text, id).ok_or("无法解析 MCP 响应")?;
        if let Some(err) = obj.get("error") {
            return Err(format!("MCP 错误 {}: {}", err["code"], err["message"].as_str().unwrap_or("")));
        }
        Ok(obj.get("result").cloned().unwrap_or(Value::Null))
    }

    fn connect(&mut self) -> Result<(), String> {
        if self.connected { return Ok(()); }
        self.rpc("initialize", Some(json!({
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": { "name": "mcgacha-cli", "version": "1.0.0" }
        })), false)?;
        self.rpc("notifications/initialized", None, true)?;
        self.connected = true;
        Ok(())
    }

    fn call(&mut self, name: &str, args: Value) -> Result<Value, String> {
        self.connect()?;
        let result = self.rpc("tools/call", Some(json!({ "name": name, "arguments": args })), false)?;
        if result["isError"].as_bool() == Some(true) {
            let msg: String = result["content"].as_array().unwrap_or(&vec![]).iter()
                .filter_map(|c| c["text"].as_str()).collect::<Vec<_>>().join(" ");
            return Err(format!("工具 {} 返回错误: {}", name, &msg[..msg.len().min(300)]));
        }
        let text: String = result["content"].as_array().unwrap_or(&vec![]).iter()
            .filter(|c| c["type"].as_str() == Some("text"))
            .filter_map(|c| c["text"].as_str()).collect::<Vec<_>>().join("\n");
        if text.is_empty() { return Ok(result); }
        Ok(serde_json::from_str(&text).unwrap_or(json!({ "raw": text })))
    }
}

fn parse_rpc_body(text: &str, want_id: Option<u64>) -> Option<Value> {
    if text.contains("data:") {
        for line in text.lines() {
            let t = line.trim();
            if !t.starts_with("data:") { continue; }
            if let Ok(obj) = serde_json::from_str::<Value>(t[5..].trim()) {
                if want_id.is_none() || obj["id"].as_u64() == want_id { return Some(obj); }
            }
        }
        None
    } else {
        serde_json::from_str(text).ok()
    }
}

// ---------- 数据提取（防御式） ----------
fn find_array<'a>(node: &'a Value, key: &str) -> Option<&'a Vec<Value>> {
    match node {
        Value::Array(arr) => {
            if !arr.is_empty() && arr.iter().all(|x| x.get(key).is_some()) { return Some(arr); }
            arr.iter().find_map(|x| find_array(x, key))
        }
        Value::Object(map) => map.values().find_map(|v| find_array(v, key)),
        _ => None,
    }
}
fn pick<'a>(obj: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|k| obj.get(*k)).filter(|v| !v.is_null() && v.as_str() != Some(""))
}
fn yuan(v: Option<&Value>) -> String {
    match v {
        Some(Value::Number(n)) => format!("¥{:.2}", n.as_f64().unwrap_or(0.0) / 100.0),
        Some(v) => format!("¥{}", v),
        None => "--".into(),
    }
}
fn s(v: &Value) -> String {
    match v { Value::String(s) => s.clone(), other => other.to_string() }
}

#[derive(Clone)]
struct Item { code: String, name: String, price: f64, orig: Option<f64>, cat: String, qty: u32 }
struct Store { code: String, name: String }

// ---------- 全局状态 ----------
struct App {
    mcp: Mcp,
    stores: Vec<(String, String)>, // (code, 展示行)
    store: Option<Store>,
    menu: Vec<Item>,
    cart: Vec<Item>,
    last_price: Option<Value>,
    take_way: Option<String>,
    last_order: Option<String>,
}

const HELP: &str = "mc init                       初始化/查看状态
mc token <Token>              配置 MCP Token（仅首次需要，本地保存）
mc stores <城市> <关键词>      搜索附近门店（query-nearby-stores）
mc use <序号>                 选择门店
mc list [关键词]              查看在售菜单（query-meals）
mc draw                       抽一张麦门卡牌（随机推荐）
mc add <序号> [数量]          加入购物车
mc cart / mc rm <序号>        查看 / 移除购物车
mc coupon                     一键领券（available-coupons + auto-bind-coupons）
mc price                      核价（calculate-price，金额为元）
mc order                      下单（create-order，需输入 YES 确认）
mc status [订单号]            查询订单状态（query-order）
mc clear                      清屏
mc exit                       退出";

fn token_file() -> Option<std::path::PathBuf> {
    std::env::var("APPDATA").ok().map(|d| std::path::Path::new(&d).join("mcgacha").join("token.txt"))
}
fn load_token() -> String {
    if let Ok(t) = std::env::var("MC_TOKEN") { if !t.trim().is_empty() { return t.trim().into(); } }
    let saved = token_file().and_then(|f| std::fs::read_to_string(f).ok()).unwrap_or_default();
    if !saved.trim().is_empty() { return saved.trim().into(); }
    env!("MC_EMBED_TOKEN").trim().to_string()
}

fn main() {
    enable_ansi();
    let mut app = App {
        mcp: Mcp::new(load_token()),
        stores: vec![], store: None, menu: vec![], cart: vec![],
        last_price: None, take_way: None, last_order: None,
    };

    println!("{}➜ mc ${} mc init", GREEN, RESET);
    if app.mcp.token.is_empty() {
        p("[麦当劳终端点餐 v1.0] 未配置 MCP Token", YELLOW);
        p("请先运行：mc token <你的Token>（Token 按官方 https://open.mcd.cn/mcp 指引领取，仅保存在本机）", RED);
        p("配置后输入 mc init 连接", DIM);
    } else {
        cmd_init(&mut app);
    }

    let stdin = io::stdin();
    loop {
        print!("{}➜ mc ${} ", GREEN, RESET);
        io::stdout().flush().ok();
        let mut line = String::new();
        if stdin.lock().read_line(&mut line).unwrap_or(0) == 0 { break; }
        let line = line.trim();
        if line.is_empty() { continue; }
        let mut parts: Vec<&str> = line.split_whitespace().collect();
        if parts[0] == "mc" { parts.remove(0); }
        if parts.is_empty() { continue; }
        let cmd = parts.remove(0).to_lowercase();
        if run_cmd(&mut app, &cmd, &parts, line) { break; }
    }
}

// 返回 true 表示退出
fn run_cmd(app: &mut App, cmd: &str, args: &[&str], _raw: &str) -> bool {
    match cmd {
        "exit" | "quit" => { p("再见，麦门永存", YELLOW); return true; }
        "init" => cmd_init(app),
        "help" => p(HELP, ""),
        "token" => {
            if args.is_empty() { p("用法：mc token <你的MCP Token>", RED); return false; }
            let t = args[0].trim();
            if let Some(f) = token_file() {
                std::fs::create_dir_all(f.parent().unwrap()).ok();
                std::fs::write(&f, t).ok();
            }
            app.mcp = Mcp::new(t.into());
            p("Token 已保存到本地配置，后续启动自动生效", GREEN);
        }
        "clear" | "cls" => print!("\x1b[2J\x1b[H"),
        "stores" => {
            if args.len() < 2 { p("用法：mc stores <城市> <位置关键词>   例：mc stores 上海市 人民广场", RED); return false; }
            p("查询中（query-nearby-stores）...", DIM);
            match app.mcp.call("query-nearby-stores", json!({ "beType": 1, "searchType": 2, "city": args[0], "keyword": args[1..].join(" ") })) {
                Ok(d) => {
                    app.stores.clear();
                    if let Some(arr) = find_array(&d, "storeCode") {
                        for st in arr {
                            let code = s(&st["storeCode"]);
                            let name = pick(st, &["storeName", "name", "restaurantName"]).map(s).unwrap_or(format!("门店 {}", code));
                            let dist = pick(st, &["distance", "distanceText"]).map(|v| format!("{}m", s(v))).unwrap_or("--".into());
                            let addr = pick(st, &["address", "storeAddress"]).map(s).unwrap_or_default();
                            let status = pick(st, &["status", "storeStatus", "openStatus", "businessStatus"]).map(s).unwrap_or_default();
                            app.stores.push((code.clone(), format!("{}  {}  {}{}", name, dist, addr, if status.is_empty() { "".into() } else { format!("  状态={}", status) })));
                        }
                    }
                    if app.stores.is_empty() { p("未找到门店，换个关键词试试", RED); return false; }
                    p(&format!("附近门店（到店自提 · {} {}）：", args[0], args[1..].join(" ")), CYAN);
                    for (i, (code, line)) in app.stores.iter().enumerate() {
                        println!("{}  [{}] {}{}", RESET, i + 1, line, RESET);
                        p(&format!("      storeCode={}", code), DIM);
                    }
                    p("输入 mc use <序号> 选择门店", DIM);
                }
                Err(e) => p(&format!("出错：{}", e), RED),
            }
        }
        "use" => {
            let n: usize = args.first().and_then(|x| x.parse().ok()).unwrap_or(0);
            if n == 0 || n > app.stores.len() { p("用法：mc use <序号>（先 mc stores 搜索）", RED); return false; }
            let (code, line) = &app.stores[n - 1];
            let name = line.split("  ").next().unwrap_or("").to_string();
            app.store = Some(Store { code: code.clone(), name: name.clone() });
            app.menu.clear(); app.cart.clear(); app.last_price = None; app.take_way = None;
            p(&format!("已选门店：{}（storeCode={}）", name, code), GREEN);
        }
        "list" => cmd_list(app, args.join(" "), false),
        "draw" => cmd_list(app, String::new(), true),
        "add" => {
            if !need_store(app) { return false; }
            if app.menu.is_empty() { p("请先 mc list 或 mc draw 加载菜单", RED); return false; }
            let n: usize = args.first().and_then(|x| x.parse().ok()).unwrap_or(0);
            if n == 0 || n > app.menu.len() { p("用法：mc add <菜单序号> [数量]", RED); return false; }
            let qty: u32 = args.get(1).and_then(|x| x.parse().ok()).unwrap_or(1).max(1);
            let m = app.menu[n - 1].clone();
            if let Some(c) = app.cart.iter_mut().find(|c| c.code == m.code) { c.qty += qty; }
            else { let mut it = m.clone(); it.qty = qty; app.cart.push(it); }
            app.last_price = None;
            let total: u32 = app.cart.iter().map(|c| c.qty).sum();
            p(&format!("已加购：{} × {}（购物车 {} 件）", m.name, qty, total), GREEN);
        }
        "cart" => {
            if app.cart.is_empty() { p("购物车是空的（mc list 看看，或 mc draw 抽一张）", DIM); return false; }
            hr();
            for (i, c) in app.cart.iter().enumerate() {
                println!("  [{}] {} × {}  ¥{:.2}", i + 1, c.name, c.qty, c.price * c.qty as f64);
            }
            hr();
        }
        "rm" => {
            let n: usize = args.first().and_then(|x| x.parse().ok()).unwrap_or(0);
            if n == 0 || n > app.cart.len() { p("用法：mc rm <购物车序号>", RED); return false; }
            let x = app.cart.remove(n - 1); app.last_price = None;
            p(&format!("已移除：{}", x.name), GREEN);
        }
        "coupon" => {
            p("查询可领券（available-coupons）...", DIM);
            if let Err(e) = app.mcp.call("available-coupons", json!({})) { p(&format!("出错：{}", e), RED); return false; }
            p("正在一键领取（auto-bind-coupons）...", DIM);
            match app.mcp.call("auto-bind-coupons", json!({})) {
                Ok(d) => {
                    let raw = d.get("raw").map(s).unwrap_or_else(|| d.to_string());
                    p(&format!("领取结果：{}", &raw[..raw.len().min(300)]), GREEN);
                    p("已领券存于你的麦当劳账户，可在麦当劳 App 支付环节使用", DIM);
                }
                Err(e) => p(&format!("出错：{}", e), RED),
            }
        }
        "price" => {
            if !need_store(app) || app.cart.is_empty() { if app.cart.is_empty() && need_store(app) { p("购物车是空的", RED); } return false; }
            p("核价中（calculate-price）...", DIM);
            let items: Vec<Value> = app.cart.iter().map(|c| json!({ "productCode": c.code, "quantity": c.qty })).collect();
            match app.mcp.call("calculate-price", json!({ "storeCode": app.store.as_ref().unwrap().code, "orderType": 1, "beType": 1, "items": items })) {
                Ok(d) => {
                    app.take_way = find_array(&d, "code").and_then(|a| a.first()).map(|v| s(&v["code"]));
                    hr();
                    p("  核价结果（calculate-price 真实返回）：", CYAN);
                    for c in &app.cart { println!("  - {} × {}", c.name, c.qty); }
                    hr();
                    if let Some(v) = pick(&d, &["originalPrice", "totalOriginalPrice"]) { println!("  原价：{}", yuan(Some(v))); }
                    if let Some(v) = pick(&d, &["discount", "discountAmount", "totalDiscount"]) { p(&format!("  优惠：-{}", yuan(Some(v))), GREEN); }
                    p(&format!("  应付：{}", yuan(pick(&d, &["price", "totalPrice", "payPrice", "amount"]))), YELLOW);
                    if let Some(tw) = &app.take_way { p(&format!("  取餐方式：{}", tw), DIM); }
                    hr();
                    p("确认无误输入 mc order 创建真实订单（下单前还会再确认一次）", DIM);
                    app.last_price = Some(d);
                }
                Err(e) => p(&format!("出错：{}", e), RED),
            }
        }
        "order" => {
            if !need_store(app) { return false; }
            if app.cart.is_empty() { p("购物车是空的", RED); return false; }
            if app.last_price.is_none() || app.take_way.is_none() { p("请先 mc price 核价（取餐方式以核价返回为准）", RED); return false; }
            hr();
            p("  !! 即将创建真实订单，会产生真实支付 !!", RED);
            println!("  门店：{}", app.store.as_ref().unwrap().name);
            for c in &app.cart { println!("  - {} × {}", c.name, c.qty); }
            p(&format!("  应付：{}", yuan(pick(app.last_price.as_ref().unwrap(), &["price", "totalPrice", "payPrice", "amount"]))), YELLOW);
            hr();
            p("确认下单请输入 YES（大写），其他任意输入取消", RED);
            print!("{}➜ mc ${} ", GREEN, RESET);
            io::stdout().flush().ok();
            let mut confirm = String::new();
            if io::stdin().lock().read_line(&mut confirm).unwrap_or(0) == 0 { return true; }
            if confirm.trim() != "YES" { p("已取消下单", DIM); return false; }
            p("创建订单（create-order）...", DIM);
            let items: Vec<Value> = app.cart.iter().map(|c| json!({ "productCode": c.code, "quantity": c.qty })).collect();
            match app.mcp.call("create-order", json!({
                "storeCode": app.store.as_ref().unwrap().code, "orderType": 1, "beType": 1,
                "takeWayCode": app.take_way, "items": items
            })) {
                Ok(d) => {
                    let oid = pick(&d, &["orderId", "orderNo", "orderCode", "id"]).map(s);
                    if let Some(o) = &oid { app.last_order = Some(o.clone()); }
                    hr();
                    p("  订单创建成功", GREEN);
                    if let Some(o) = &oid { p(&format!("  订单号：{}", o), GREEN); }
                    if let Some(pay) = pick(&d, &["payUrl", "paymentUrl", "payLink", "cashierUrl", "qrCode", "qrUrl"]) { p(&format!("  支付链接：{}", s(pay)), YELLOW); }
                    p("  请通过支付链接或麦当劳 App 完成支付，支付后用 mc status 查状态", DIM);
                    hr();
                    app.cart.clear(); app.last_price = None; app.take_way = None;
                }
                Err(e) => p(&format!("下单失败：{}", e), RED),
            }
        }
        "status" => {
            let id = args.first().map(|x| x.to_string()).or_else(|| app.last_order.clone());
            if id.is_none() { p("用法：mc status [订单号]（或下单后直接 mc status）", RED); return false; }
            let id = id.unwrap();
            match app.mcp.call("query-order", json!({ "orderId": id })) {
                Ok(d) => {
                    hr();
                    p(&format!("  订单 {}：", id), CYAN);
                    if let Some(v) = pick(&d, &["status", "orderStatus", "statusDesc", "state"]) { p(&format!("  状态：{}", s(v)), GREEN); }
                    if let Some(v) = pick(&d, &["pickupCode", "takeCode", "fetchCode", "mealCode"]) { p(&format!("  取餐码：{}", s(v)), YELLOW); }
                    let detail = d.to_string();
                    p(&format!("  详情：{}", &detail[..detail.len().min(600)]), DIM);
                    hr();
                }
                Err(e) => p(&format!("出错：{}", e), RED),
            }
        }
        "yes" => p("没有待确认的订单，先 mc price 再 mc order", DIM),
        _ => p(&format!("未知命令：{}（输入 mc help 查看）", cmd), RED),
    }
    false
}

fn cmd_init(app: &mut App) {
    p("[麦门终端 v1.0] 已连接麦当劳 MCP（真实数据）", YELLOW);
    match app.mcp.call("now-time-info", json!({})) {
        Ok(d) => {
            let t = pick(&d, &["formatted", "dateTime", "time"]).map(s)
                .or_else(|| d.get("raw").map(s)).unwrap_or_else(|| d.to_string());
            println!("服务器时间 {}（now-time-info）", t);
        }
        Err(e) => p(&format!("时间获取失败：{}", e), RED),
    }
    let total: u32 = app.cart.iter().map(|c| c.qty).sum();
    p(&format!("[cart {} 件]{}", total, app.store.as_ref().map(|st| format!(" [门店: {}]", st.name)).unwrap_or(" [门店: 未选择]".into())), DIM);
    p("输入 mc help 查看命令", DIM);
}

fn need_store(app: &App) -> bool {
    if app.store.is_none() {
        p("请先选门店：mc stores <城市> <关键词> → mc use <序号>", RED);
        return false;
    }
    true
}

// list 与 draw 共用菜单加载
fn cmd_list(app: &mut App, kw: String, draw: bool) {
    if !need_store(app) { return; }
    if app.menu.is_empty() {
        p("加载菜单（query-meals）...", DIM);
        let d = match app.mcp.call("query-meals", json!({ "storeCode": app.store.as_ref().unwrap().code, "orderType": 1, "beType": 1 })) {
            Ok(d) => d,
            Err(e) => { p(&format!("出错：{}", e), RED); return; }
        };
        if let Some(arr) = find_array(&d, "productCode") {
            for pv in arr {
                let price = pick(pv, &["price", "salePrice", "currentPrice"]).and_then(|v| v.as_f64()).unwrap_or(0.0) / 100.0;
                let orig = pick(pv, &["originalPrice", "marketPrice", "originPrice"]).and_then(|v| v.as_f64()).map(|v| v / 100.0);
                app.menu.push(Item {
                    code: s(&pv["productCode"]),
                    name: pick(pv, &["productName", "name", "title"]).map(s).unwrap_or_else(|| s(&pv["productCode"])),
                    price, orig,
                    cat: pick(pv, &["categoryName", "category", "groupName"]).map(s).unwrap_or_default(),
                    qty: 0,
                });
            }
        }
    }
    if app.menu.is_empty() { p("菜单为空", RED); return; }

    if draw {
        let i = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().subsec_nanos() as usize) % app.menu.len();
        let m = &app.menu[i];
        hr();
        p("  * 今日抽卡 *", YELLOW);
        p(&format!("  {}", m.name), YELLOW);
        p(&format!("  ¥{:.2}{}{}", m.price,
            m.orig.filter(|o| *o != m.price).map(|o| format!(" (原价 ¥{:.2})", o)).unwrap_or_default(),
            if m.cat.is_empty() { String::new() } else { format!("  [{}]", m.cat) }), YELLOW);
        hr();
        p(&format!("不满意就 mc draw 再抽一张，满意就 mc add {}", i + 1), DIM);
        return;
    }

    let shown: Vec<(usize, &Item)> = app.menu.iter().enumerate()
        .filter(|(_, m)| kw.is_empty() || (m.name.clone() + &m.cat).contains(&kw))
        .take(60).collect();
    if shown.is_empty() { p("（无餐品）", DIM); return; }
    p(&if kw.is_empty() { format!("在售餐品（共 {} 项，显示前 60 项）：", app.menu.len()) } else { format!("匹配「{}」的餐品（{} 项）：", kw, shown.len()) }, CYAN);
    for (i, m) in shown {
        println!("  [{}] {}  ¥{:.2}{}{}", i + 1, m.name, m.price,
            m.orig.filter(|o| *o != m.price).map(|o| format!(" (原价¥{:.2})", o)).unwrap_or_default(),
            if m.cat.is_empty() { String::new() } else { format!("  [{}]", m.cat) });
    }
    p("输入 mc add <序号> [数量] 加购，或 mc draw 让命运决定", DIM);
}
