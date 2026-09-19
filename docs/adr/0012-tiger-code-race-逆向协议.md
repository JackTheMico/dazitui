# 0012. 虎码杯（race.tiger-code.com）使用逆向协议对接

- **状态**: Accepted
- **日期**: 2026-09-15

## 背景与上下文

`dazitui` 在线功能原先仅针对 52dazi.cn。为支持虎码输入法官方线上打字比赛「虎码杯 / 虎杯」（`https://race.tiger-code.com/`），需将该比赛平台接入系统。
通过对官方客户端「虎魄跟打器（Hyper_gendaqi）」macOS 编译产物 `main.bin` 内部 `lib.contest_api` 模块的深度逆向与实时发包验证，完全破解了虎码杯的服务端通信协议。

## 协议技术规范

### 1. 网关与服务地址
- 官方主域名：`https://race.tiger-code.com`（与 `http://tiger-code.com:8000` 兼容）
- 前缀：`/api`
- 数据交换格式：标准 JSON (`application/json`)

### 2. 核心端点规格

#### 2.1 用户登录（POST `/api/auth/login` 或 `/auth/jwt/login`）
- **请求体**：
  ```json
  { "username": "摸鱼侠", "password": "..." }
  ```
- **响应**：`{ "success": true, "token": "<JWT_TOKEN>", "user": { "id": 624, "username": "...", ... } }`

#### 2.2 当日赛文载入（POST `/api/daily-article`）
- **说明**：为了公平性，当日比赛为**生稿赛**，按北京时间当天（`YYYY-MM-DD`）每日仅允许认证用户获取一次，不可重打。
- **请求体**：
  ```json
  { "username": "...", "password": "..." }
  ```
- **成功响应**：
  ```json
  { "success": true, "data": { "title": "YYYY-MM-DD 每日赛文", "content": "..." } }
  ```
- **重打拒绝响应**：
  ```json
  { "success": false, "message": "您今天已经获取过文章了，请明天再来" }
  ```

#### 2.3 成绩上传（POST `/api/upload-score`）
- **说明**：跟打完成后自动提交成绩。
- **请求体**：
  ```json
  {
    "username": "摸鱼侠",
    "password": "...",
    "speed": 186.18,
    "keystrokes": 7.34,
    "codeLength": 2.37,
    "time": 189.82,
    "corrections": 14,
    "keyCount": 1394,
    "keyAccuracy": 95.30,
    "wordRatio": 0.5427,
    "backsCount": 0,
    "inputMethod": "虎码",
    "userArticle": "2026-09-15 每日赛文",
    "key_log": ""
  }
  ```
- **响应**：
  ```json
  { "success": true, "data": "success", "jingyan": 0 }
  ```

#### 2.4 比赛排行榜（GET `/api/leaderboard/date/{YYYY-MM-DD}?limit=50`）
- **说明**：无需登录即可查询指定日期的每日排行榜。
- **响应**：
  ```json
  {
    "success": true,
    "data": {
      "leaderboard": [
        {
          "rank": 1,
          "username": "...",
          "speed": 186.18,
          "hit_rate": 7.34,
          "kpw": 2.37,
          "time": 189.817,
          "total_time": 189.817,
          "accuracy": 95.3,
          "input_method": "虎码",
          "word_ratio": 0.5427,
          "total_keys": 1394,
          "correction_count": 14,
          "weighted_speed": 178.71,
          "tier": "💎钻石·4"
        }
      ]
    }
  }
  ```

#### 2.5 历史赛文获取（GET `/api/daily-article/date/{YYYY-MM-DD}`）
- **说明**：查询过往历史赛文（非当日赛）。

## 架构与影响

1. **统一模型**：虎码杯与 52dazi 并列为平台的两个实现子模块（`dazitui-core::online::tigercup` 与 `dazitui-core::online::dazi52`）。
2. **生稿赛约束**：与 52dazi 锦标赛一样，虎码杯载入后不可中断重打，完成时自动上传成绩并触发剪贴板分享。
