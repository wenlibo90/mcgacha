# MCP 接入说明 · 麦门抽卡机 McGacha

本项目**真实使用麦当劳官方 MCP** 提供的全部核心数据，无任何硬编码餐品/价格/门店信息。本文档说明接入方式、实际调用的 Tool、调用流程与业务价值。

## 一、MCP Server 信息

| 项 | 值 |
|---|---|
| Server 名称 | mcd-mcp |
| 传输方式 | streamableHttp |
| Endpoint | `https://mcp.mcd.cn/mcp-servers/mcd-mcp` |
| 认证 | HTTP Header `Authorization: Bearer <Token>` |
| Token 获取 | 按大赛官方指引领取（见官方仓库 mcd-mcp-server 说明） |
| 限流 | 600 次/分钟 |
| 价格单位 | 分（展示层统一 ÷100 转元） |

配置示例见根目录 `mcp-config.example.json`，Token 一律用环境变量 `${MCD_MCP_TOKEN}` 占位，不落盘、不进代码。

## 二、实际调用的 Tool 清单

### 功能1 · 今日抽卡（吃什么）

| 调用顺序 | Tool | 用途 |
|---|---|---|
| 1 | now-time-info | 取服务器时间判断餐段（早餐/正餐/下午茶/夜宵） |
| 2 | query-nearby-stores | 按用户定位取附近门店（距离、营业状态） |
| 3 | query-meals | 拉取门店当前时段真实在售菜单 |
| 4 | query-meal-detail | 抽中套餐时取套餐组成与可替换项 |
| 5 | list-nutrition-foods | 匹配抽中餐品的热量/蛋白质/脂肪/钠，绘制卡牌数值 |
| 6 | available-coupons → auto-bind-coupons | 列出可领券并一键领取，结果展示在卡牌上 |
| 7 | calculate-price | 用户决定"就吃这个"时用券后真实核价 |
| 8 | create-order | 用户确认后真实下单（beType=1 到店自提），返回支付链接 |

### 功能2 · 徽章图鉴与麦门年度故事

| 调用顺序 | Tool | 用途 |
|---|---|---|
| 1 | order-list | 拉取历史订单作为徽章铸造与故事素材 |
| 2 | list-nutrition-foods | 汇总年度摄入趣味数据（总热量≈跑了多少圈等） |
| 3 | query-nearby-stores | 校准常去门店信息（名称、地址） |
| 4 | campaign-calendar | 对照当年官方活动，标注"你参与过的麦门大事件" |
| 5 | query-my-account | 积分余额与会员等级，写入故事页尾 |

### 功能3 · 麦门DNA

| 调用顺序 | Tool | 用途 |
|---|---|---|
| 1 | order-list | 血统成分推算的原始样本 |
| 2 | list-nutrition-foods | 营养健康风险项（脂肪/钠/蛋白质结构） |
| 3 | query-nearby-stores | 祖源迁徙路线：历年取餐门店足迹 |
| 4 | query-my-account | 麦门资历（会员年限、积分）写入报告头 |

### 功能4 · CLI 终端下单

| 调用顺序 | Tool | 用途 |
|---|---|---|
| 1 | now-time-info | CLI 启动横幅显示麦门时间 |
| 2 | query-nearby-stores | `mc stores` 列出门店供选择 |
| 3 | query-meals / query-meal-detail | `mc list` / `mc info <id>` 浏览菜单与套餐详情 |
| 4 | available-coupons / auto-bind-coupons | `mc coupon` 领券 |
| 5 | calculate-price | `mc price` 核价（券后价明细） |
| 6 | create-order | `mc order` 下单（需用户在 CLI 内输入 YES 确认） |
| 7 | query-order / cancel-order | `mc status` 查单 / `mc cancel` 取消 |

## 三、关键调用流程（以抽卡下单为例）

```
用户：今天吃什么？
  │
  ├─ now-time-info            判断餐段
  ├─ query-nearby-stores      附近门店（取最近且营业中）
  ├─ query-meals(storeId)     该店当前在售菜单
  ├─ 随机抽取 1 个餐品 ──┐
  ├─ list-nutrition-foods     ├─ 补充营养数值
  ├─ query-meal-detail        ┘（若为套餐）补充组成
  ├─ available-coupons → auto-bind-coupons   顺手领券
  │
  ▼ 输出卡牌（餐品/真实价格/热量/梗文案 + 门店 + 领券结果）
用户：就吃这个
  │
  ├─ calculate-price          券后核价，向用户展示明细
  ├─ 用户确认 ── create-order(beType=1)
  ▼ 返回支付链接，提示取餐方式
```

## 四、业务价值

1. **决策减负**：把"吃什么"从 10 分钟纠结变成 1 次抽卡，推荐基于真实在售与真实价格，不产生幻觉菜单。
2. **优惠不漏**：抽卡即领券，券后价透明，切实省钱。
3. **数据情感化**：历史订单从冷数据变成徽章、故事与 DNA 报告，提升会员粘性与分享欲。
4. **开发者友好**：CLI 模式让点餐融入程序员工作流，拓展 MCP 使用场景边界。

## 五、合规与安全

- 下单、取消订单等写操作均**先展示明细、经用户显式确认**后执行。
- 历史订单等个人数据仅用于本地生成报告页，不上传、不持久化到任何服务器。
- 仓库与配置中不含真实 Token，仅使用环境变量占位符。
- 报告页所有营养表述为趣味展示，附"不构成专业建议"声明。
