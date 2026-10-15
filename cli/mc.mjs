#!/usr/bin/env node
/**
 * 麦当劳抽卡机 · 真实 CLI 客户端（零依赖，Node.js >= 18）
 * 直连麦当劳 MCP（streamable HTTP / JSON-RPC 2.0），不经过 AI 客户端。
 *
 * 用法：
 *   设置环境变量 MCD_MCP_TOKEN（大赛官方发放的 MCP Token）
 *   node cli/mc.mjs --probe   自检（initialize + 服务器时间，不产生任何写操作）
 *   node cli/mc.mjs           进入交互终端
 *
 * 安全约定：
 *   - Token 只从环境变量读取，不落盘、不打印
 *   - mc order 创建真实订单前必须手输 YES 确认
 */

import readline from 'node:readline';

const MCP_URL = process.env.MCD_MCP_URL || 'https://mcp.mcd.cn/mcp-servers/mcd-mcp';
const TOKEN = process.env.MCD_MCP_TOKEN || '';
const VERSION = '1.0.0';

// ---------- MCP 协议层 ----------

let sessionId = null;
let rpcSeq = 0;

/** 解析响应体：兼容 application/json 与 text/event-stream（SSE） */
function parseRpcBody(text, wantId) {
  if (!text) return null;
  // SSE：逐行找 data: 负载
  if (text.includes('data:')) {
    for (const line of text.split('\n')) {
      const t = line.trim();
      if (!t.startsWith('data:')) continue;
      try {
        const obj = JSON.parse(t.slice(5).trim());
        if (obj && (wantId == null || obj.id === wantId)) return obj;
      } catch { /* 忽略非 JSON 行 */ }
    }
    return null;
  }
  try { return JSON.parse(text); } catch { return null; }
}

async function rpc(method, params, { notify = false } = {}) {
  const id = notify ? undefined : ++rpcSeq;
  const body = JSON.stringify({ jsonrpc: '2.0', ...(id != null ? { id } : {}), method, ...(params ? { params } : {}) });
  const headers = {
    'Content-Type': 'application/json',
    'Accept': 'application/json, text/event-stream',
    'Authorization': `Bearer ${TOKEN}`,
  };
  if (sessionId) headers['Mcp-Session-Id'] = sessionId;

  const ctrl = new AbortController();
  const timer = setTimeout(() => ctrl.abort(), 30000);
  let res;
  try {
    res = await fetch(MCP_URL, { method: 'POST', headers, body, signal: ctrl.signal });
  } finally {
    clearTimeout(timer);
  }

  const sid = res.headers.get('mcp-session-id');
  if (sid) sessionId = sid;

  if (res.status === 401 || res.status === 403) {
    throw new Error(`鉴权失败（HTTP ${res.status}）：请检查 MCD_MCP_TOKEN 是否正确或已过期`);
  }
  if (!res.ok) {
    const detail = (await res.text().catch(() => '')).slice(0, 200);
    throw new Error(`HTTP ${res.status} ${res.statusText} ${detail}`.trim());
  }
  if (notify || res.status === 202) return null;

  const obj = parseRpcBody(await res.text(), id);
  if (!obj) throw new Error('无法解析 MCP 响应');
  if (obj.error) throw new Error(`MCP 错误 ${obj.error.code}: ${obj.error.message}`);
  return obj.result;
}

/** 调用 MCP 工具，返回解析后的数据（工具 text 负载通常是 JSON 字符串） */
async function callTool(name, args = {}) {
  const result = await rpc('tools/call', { name, arguments: args });
  if (!result) return null;
  if (result.isError) {
    const msg = (result.content || []).map(c => c.text || '').join(' ').slice(0, 300);
    throw new Error(`工具 ${name} 返回错误: ${msg}`);
  }
  const text = (result.content || []).filter(c => c.type === 'text').map(c => c.text).join('\n');
  if (!text) return result;
  try { return JSON.parse(text); } catch { return { raw: text }; }
}

async function connect() {
  await rpc('initialize', {
    protocolVersion: '2025-03-26',
    capabilities: {},
    clientInfo: { name: 'mcgacha-cli', version: VERSION },
  });
  await rpc('notifications/initialized', null, { notify: true });
}

// ---------- 数据提取（对返回结构做防御式解析） ----------

/** 深度遍历，找到第一个“元素都含指定字段”的对象数组 */
function findArray(node, keys) {
  if (!node || typeof node !== 'object') return null;
  if (Array.isArray(node)) {
    if (node.length && node.every(x => x && typeof x === 'object' && keys.every(k => k in x))) return node;
    for (const item of node) { const hit = findArray(item, keys); if (hit) return hit; }
    return null;
  }
  for (const v of Object.values(node)) { const hit = findArray(v, keys); if (hit) return hit; }
  return null;
}

const pick = (obj, ...keys) => { for (const k of keys) if (obj && obj[k] != null && obj[k] !== '') return obj[k]; return undefined; };
const fen2yuan = v => (typeof v === 'number' ? (v / 100).toFixed(2) : v);

function extractStores(data) {
  const arr = findArray(data, ['storeCode']) || [];
  return arr.map(s => ({
    storeCode: String(s.storeCode),
    beCode: pick(s, 'beCode'),
    name: pick(s, 'storeName', 'name', 'restaurantName') || `门店 ${s.storeCode}`,
    address: pick(s, 'address', 'storeAddress') || '',
    distance: pick(s, 'distance', 'distanceText'),
    status: pick(s, 'status', 'storeStatus', 'openStatus', 'businessStatus'),
    raw: s,
  }));
}

function extractMeals(data) {
  const arr = findArray(data, ['productCode']) || [];
  return arr.map(p => ({
    productCode: String(p.productCode),
    name: pick(p, 'productName', 'name', 'title') || p.productCode,
    price: fen2yuan(pick(p, 'price', 'salePrice', 'currentPrice')),
    originalPrice: fen2yuan(pick(p, 'originalPrice', 'marketPrice', 'originPrice')),
    category: pick(p, 'categoryName', 'category', 'groupName'),
  }));
}

// ---------- 会话状态（仅内存，退出即清） ----------

const state = {
  stores: [],        // 最近一次门店搜索结果
  store: null,       // 已选门店
  menu: [],          // 已选门店的菜单缓存
  lastDraw: null,    // 最近一次抽卡在 menu 中的下标
  cart: [],          // [{ productCode, name, quantity, price }]
  takeWayList: null, // calculate-price 返回的取餐方式
  takeWayCode: null,
  lastPrice: null,   // 最近一次核价结果
  lastOrderId: null,
};

// ---------- 输出 ----------

const out = (s = '') => console.log(s);
const hr = () => out('-'.repeat(56));

function printStores() {
  if (!state.stores.length) return out('（无门店结果）');
  state.stores.forEach((s, i) => {
    const dist = s.distance != null ? `${s.distance}m` : '--';
    const addr = s.address ? `  ${s.address}` : '';
    out(`  [${i + 1}] ${s.name}  ${dist}${addr}`);
    out(`      storeCode=${s.storeCode}${s.status != null ? `  状态=${s.status}` : ''}`);
  });
}

function printCart() {
  if (!state.cart.length) return out('购物车是空的，先 mc list 或 mc draw 再 mc add <序号>');
  hr();
  state.cart.forEach((c, i) => out(`  [${i + 1}] ${c.name} x${c.quantity}${c.price != null ? `  ¥${c.price}` : ''}`));
  hr();
}

// ---------- 命令 ----------

async function cmdTime() {
  const d = await callTool('now-time-info');
  out(`服务器时间: ${pick(d, 'formatted', 'dateTime', 'time') || d.raw || JSON.stringify(d)}`);
}

async function cmdStores(args) {
  const [city, ...kw] = args;
  if (!city || !kw.length) return out('用法: mc stores <城市> <位置关键词>   例: mc stores 上海市 人民广场');
  const d = await callTool('query-nearby-stores', { beType: 1, searchType: 2, city, keyword: kw.join(' ') });
  state.stores = extractStores(d);
  out(`附近门店（到店自提 · ${city} ${kw.join(' ')}）:`);
  printStores();
  if (state.stores.length) out('输入 mc use <序号> 选择门店');
}

async function cmdUse(args) {
  const n = parseInt(args[0], 10);
  if (!n || n < 1 || n > state.stores.length) return out('用法: mc use <序号>（先 mc stores 搜索）');
  state.store = state.stores[n - 1];
  state.menu = []; state.cart = []; state.lastPrice = null; state.takeWayCode = null;
  out(`已选门店: ${state.store.name}（storeCode=${state.store.storeCode}）`);
}

async function ensureStoreAndMenu(refresh = false) {
  if (!state.store) throw new Error('请先选择门店: mc stores <城市> <关键词> 然后 mc use <序号>');
  if (refresh || !state.menu.length) {
    const d = await callTool('query-meals', { storeCode: state.store.storeCode, orderType: 1, beType: 1 });
    state.menu = extractMeals(d);
  }
  return state.menu;
}

async function cmdList(args) {
  const menu = await ensureStoreAndMenu(!state.menu.length);
  const kw = args.join(' ');
  // 带关键词时保留全量菜单中的原始序号，保证 mc add 序号始终有效
  const indexed = menu.map((m, i) => ({ ...m, _no: i + 1 })).filter(m => !kw || (m.name + (m.category || '')).includes(kw));
  const shown = indexed.slice(0, 60);
  if (!shown.length) return out('（无餐品）');
  out(kw ? `匹配「${kw}」的餐品（共 ${indexed.length} 项）:` : `在售餐品（共 ${menu.length} 项${menu.length > 60 ? '，显示前 60 项' : ''}）:`);
  shown.forEach(m => {
    const price = m.price != null ? `¥${m.price}` : '¥--';
    const orig = m.originalPrice != null && m.originalPrice !== m.price ? ` (原价¥${m.originalPrice})` : '';
    const cat = m.category ? `  [${m.category}]` : '';
    out(`  [${m._no}] ${m.name}  ${price}${orig}${cat}`);
  });
  out('输入 mc add <序号> [数量] 加入购物车，或 mc draw 让命运决定');
}

async function cmdDraw() {
  const menu = await ensureStoreAndMenu();
  if (!menu.length) return out('菜单为空');
  const i = Math.floor(Math.random() * menu.length);
  state.lastDraw = i;
  const m = menu[i];
  hr();
  out('  * 今日抽卡 *');
  out(`  ${m.name}`);
  out(`  ${m.price != null ? `¥${m.price}` : ''}${m.originalPrice != null && m.originalPrice !== m.price ? ` (原价 ¥${m.originalPrice})` : ''}${m.category ? `  [${m.category}]` : ''}`);
  hr();
  out(`不满意就 mc draw 再抽一张，满意就 mc add ${i + 1}`);
}

async function cmdAdd(args) {
  const n = parseInt(args[0], 10);
  const qty = Math.max(1, parseInt(args[1], 10) || 1);
  const menu = await ensureStoreAndMenu();
  if (!n || n < 1 || n > menu.length) return out('用法: mc add <序号> [数量]（序号见 mc list / mc draw）');
  const m = menu[n - 1];
  const hit = state.cart.find(c => c.productCode === m.productCode);
  if (hit) hit.quantity += qty; else state.cart.push({ ...m, quantity: qty });
  state.lastPrice = null;
  out(`已加入: ${m.name} x${qty}（购物车 ${state.cart.reduce((s, c) => s + c.quantity, 0)} 件）`);
}

async function cmdCoupon() {
  const avail = await callTool('available-coupons');
  const coupons = findArray(avail, []) || (Array.isArray(avail) ? avail : []);
  out(`当前可领取优惠券 ${Array.isArray(coupons) ? coupons.length : 0} 张，正在一键领取...`);
  const d = await callTool('auto-bind-coupons');
  out(`领取结果: ${d && d.raw ? d.raw : JSON.stringify(d).slice(0, 300)}`);
  out('提示: 已领券存于你的麦当劳账户，核价/下单默认按官方促销价计算；券可在麦当劳 App 支付环节使用');
}

async function cmdPrice() {
  if (!state.store) return out('请先选择门店');
  if (!state.cart.length) return out('购物车是空的');
  const d = await callTool('calculate-price', {
    storeCode: state.store.storeCode, orderType: 1, beType: 1,
    items: state.cart.map(c => ({ productCode: c.productCode, quantity: c.quantity })),
  });
  state.lastPrice = d;
  const tw = findArray(d, ['code']);
  state.takeWayList = tw || null;
  state.takeWayCode = tw && tw.length ? String(pick(tw[0], 'code')) : null;
  hr();
  out('  核价结果（calculate-price 真实返回）:');
  state.cart.forEach(c => out(`  - ${c.name} x${c.quantity}`));
  const orig = fen2yuan(pick(d, 'originalPrice', 'totalOriginalPrice'));
  const disc = fen2yuan(pick(d, 'discount', 'discountAmount', 'totalDiscount'));
  const price = fen2yuan(pick(d, 'price', 'totalPrice', 'payPrice', 'amount'));
  hr();
  if (orig != null) out(`  原价: ¥${orig}`);
  if (disc != null) out(`  优惠: -¥${disc}`);
  out(`  应付: ¥${price != null ? price : '(以返回为准)'}`);
  if (state.takeWayCode) out(`  取餐方式: ${pick(tw[0], 'name', 'label', 'desc') || state.takeWayCode}`);
  hr();
  out('确认无误输入 mc order 创建真实订单（下单前还会再确认一次）');
}

async function cmdOrder() {
  if (!state.store) return out('请先选择门店');
  if (!state.cart.length) return out('购物车是空的');
  if (!state.lastPrice || !state.takeWayCode) return out('请先 mc price 核价（取餐方式以核价返回为准）');
  hr();
  out('  !! 即将创建真实订单，会产生真实支付 !!');
  out(`  门店: ${state.store.name}`);
  state.cart.forEach(c => out(`  - ${c.name} x${c.quantity}`));
  const price = fen2yuan(pick(state.lastPrice, 'price', 'totalPrice', 'payPrice', 'amount'));
  out(`  应付: ¥${price != null ? price : '?'}`);
  hr();
  out('确认下单请输入 YES（大写），其他任意输入取消');
  state.pendingOrder = true;
}

async function cmdYes() {
  if (!state.pendingOrder) return out('没有待确认的订单，先 mc price 再 mc order');
  state.pendingOrder = false;
  const d = await callTool('create-order', {
    storeCode: state.store.storeCode, orderType: 1, beType: 1,
    takeWayCode: state.takeWayCode,
    items: state.cart.map(c => ({ productCode: c.productCode, quantity: c.quantity })),
  });
  const orderId = pick(d, 'orderId', 'orderNo', 'orderCode', 'id');
  if (orderId) state.lastOrderId = String(orderId);
  hr();
  out('  订单创建成功');
  if (orderId) out(`  订单号: ${orderId}`);
  const pay = pick(d, 'payUrl', 'paymentUrl', 'payLink', 'cashierUrl', 'qrCode', 'qrUrl');
  if (pay) out(`  支付链接: ${pay}`);
  out('  请通过支付链接或麦当劳 App 完成支付，支付后用 mc status 查最新状态');
  if (!orderId && !pay) out(`  原始返回: ${JSON.stringify(d).slice(0, 500)}`);
  hr();
  state.cart = []; state.lastPrice = null; state.takeWayCode = null;
}

async function cmdStatus(args) {
  const orderId = args[0] || state.lastOrderId;
  if (!orderId) return out('用法: mc status [订单号]（或下单后直接 mc status）');
  const d = await callTool('query-order', { orderId: String(orderId) });
  hr();
  out(`  订单 ${orderId}:`);
  const status = pick(d, 'status', 'orderStatus', 'statusDesc', 'state');
  const code = pick(d, 'pickupCode', 'takeCode', 'fetchCode', 'mealCode');
  if (status != null) out(`  状态: ${status}`);
  if (code != null) out(`  取餐码: ${code}`);
  out(`  详情: ${JSON.stringify(d).slice(0, 600)}`);
  hr();
}

async function cmdHistory() {
  const d = await callTool('order-list');
  out(JSON.stringify(d, null, 2).slice(0, 1200));
}

function cmdHelp() {
  out(`
麦当劳抽卡机 CLI v${VERSION}（真实 MCP 数据 · 到店自提模式）
  mc time                      服务器时间
  mc stores <城市> <关键词>     搜索附近门店   例: mc stores 上海市 人民广场
  mc use <序号>                 选择门店
  mc list [关键词]              查看在售餐品（真实价格）
  mc draw                       随机抽一张（不服再抽）
  mc add <序号> [数量]          加入购物车
  mc cart / mc rm <序号>        查看 / 移除购物车
  mc coupon                     查看并一键领取全部可领券
  mc price                      核价（含优惠，金额为元）
  mc order                      创建真实订单（需再输 YES 确认）
  mc status [订单号]            查询订单状态 / 取餐码
  mc history                    历史订单
  mc clear                      清空购物车
  mc quit                       退出
输入时可省略 mc 前缀。Token 从环境变量 MCD_MCP_TOKEN 读取。`);
}

const COMMANDS = {
  time: cmdTime, stores: cmdStores, use: cmdUse, list: cmdList, draw: cmdDraw,
  add: cmdAdd, cart: async () => printCart(), coupon: cmdCoupon, price: cmdPrice,
  order: cmdOrder, yes: cmdYes, status: cmdStatus, history: cmdHistory, help: async () => cmdHelp(),
  rm: async (args) => {
    const n = parseInt(args[0], 10);
    if (!n || n < 1 || n > state.cart.length) return out('用法: mc rm <购物车序号>');
    const [x] = state.cart.splice(n - 1, 1); state.lastPrice = null;
    out(`已移除: ${x.name}`);
  },
  clear: async () => { state.cart = []; state.lastPrice = null; out('购物车已清空'); },
};

async function dispatch(line) {
  const parts = line.trim().split(/\s+/).filter(Boolean);
  if (!parts.length) return;
  if (parts[0] === 'mc') parts.shift();
  const cmd = (parts.shift() || '').toLowerCase();
  if (!cmd) return;
  if (cmd === 'quit' || cmd === 'exit') process.exit(0);
  if (cmd === 'yes') return cmdYes();
  const fn = COMMANDS[cmd];
  if (!fn) return out(`未知命令: ${cmd}（输入 mc help 查看命令）`);
  try { await fn(parts); } catch (e) { out(`出错: ${e.message}`); }
}

// ---------- 入口 ----------

async function main() {
  out(`麦当劳抽卡机 CLI v${VERSION} · 直连麦当劳 MCP`);
  if (!TOKEN) {
    out('\n缺少 MCD_MCP_TOKEN 环境变量。设置方法:');
    out('  PowerShell:  $env:MCD_MCP_TOKEN = "你的Token"');
    out('  CMD:         set MCD_MCP_TOKEN=你的Token');
    out('  macOS/Linux: export MCD_MCP_TOKEN=你的Token');
    process.exit(1);
  }
  process.stdout.write('正在连接 MCP ... ');
  await connect();
  out('OK');

  if (process.argv.includes('--probe')) {
    await cmdTime();
    out('自检通过: 鉴权与协议层正常（未执行任何写操作）');
    process.exit(0);
  }

  await cmdTime().catch(() => {});
  out('输入 mc help 查看命令\n');
  const rl = readline.createInterface({ input: process.stdin, output: process.stdout, prompt: 'mc> ' });
  rl.prompt();
  rl.on('line', async line => { await dispatch(line); rl.prompt(); });
  rl.on('close', () => process.exit(0));
}

main().catch(e => { console.error(`启动失败: ${e.message}`); process.exit(1); });
