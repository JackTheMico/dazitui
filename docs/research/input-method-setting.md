# 52dazi 输入法（inputMethod）字段逆向分析与 dazitui 设置支持调研

## 1. 调研背景与问题定义

用户在 [52dazi.cn 极速杯排行榜](https://www.52dazi.cn/competitionRank/0) 观察到如下成绩条目：

```text
门派: 空明门
用户名: 摸鱼侠
武力值: -233
速度: 32.76
击键: 0.58
码长: 1.06838
时间: 10:42.775
回改: 12
键数: 375
键准: 100.00%
打词: 0%
输入法: 虎码
设备: 极速打字通v2.1.6
```

核心问题：**52dazi 排行榜中的「输入法」这一栏是如何产生和上报的？能否在 dazitui 中支持配置该输入法名称并在上传成绩时展示在排行榜中？**

---

## 2. 第一方事实与逆向溯源（Primary Sources）

通过对 52dazi.cn 前端生产环境 JS 资产（`setting.418a55e0.js`、`app.c22a91a0.js`、`chunk-d855c1b8.cf63ec1b.js`、`chunk-bf5de8c8.cb472a1f.js`）的逆向分析，确认了该字段的完整流转链路：

### 2.1 排行榜前端渲染

在 `chunk-d855c1b8.cf63ec1b.js`（`competitionRank` 比赛排行榜组件）与 `chunk-bf5de8c8.cb472a1f.js`（记录排行榜组件）中：

```javascript
// 排行榜表格列定义
a("el-table-column", {
  attrs: {
    prop: "inputMethod",
    label: "输入法",
    align: "center"
  }
})
```

- 服务端接口 `POST /Api/Rank/getCompetitionRank` 返回的列表项中包含 `inputMethod` 字符串字段。
- 若成绩上传时携带了该值（如 `"虎码"`），排行榜会原样显示；若为空串则显示为空白。

### 2.2 官方 Web 客户端的配置界面

在 `setting.418a55e0.js`（设置页面 - 成绩设置 Tab）中：

```javascript
// 成绩设置 Tab
o("el-tab-pane", { attrs: { label: "成绩设置", name: "result" } }, [
  // ...
  o("el-form-item", { attrs: { label: "输入法" } }, [
    o("el-switch", {
      model: {
        value: e.form.inputMethod,
        callback: function(t) { e.$set(e.form, "inputMethod", t); }
      }
    }),
    e.form.inputMethod
      ? o("el-input", {
          attrs: { maxlength: "20" },
          model: {
            value: e.form.inputMethodName,
            callback: function(t) { e.$set(e.form, "inputMethodName", t); }
          }
        })
      : e._e()
  ]),
  // ...
])
```

- 字段名：`inputMethod`（布尔开关）+ `inputMethodName`（字符串，最大长度 20 字符）。
- 存储：保存在本地 IndexedDB（`QuickTyping.configs` 表中的 `"setting"` 项）。

### 2.3 成绩上传 Payload 构造

在 `app.c22a91a0.js` 的 `resultPostData` getter（以及 `typingRecordPostData`）中：

```javascript
resultPostData: function(t, e, a) {
  var r = a.article,
      o = a.setting,
      n = a.globalVariable,
      i = o.inputMethodName,
      s = e.result,
      c = { challengeFlag: 0 };
  // ...
  c.inputMethod = i; // 直接将设置中的 inputMethodName 赋给 payload 的 inputMethod 字段
  // ...
  return c;
}
```

- 请求接口：`POST /Api/Rank/uploadResult`（经 AES-128-CBC 加密）。
- 只要 payload 中的 `inputMethod` 为非空字符串（如 `"虎码"`、`"小鹤音形"`、`"五笔86"` 等），52dazi 后端就会入库，并在该成绩对应的排行榜行中渲染该输入法名称。

### 2.4 剪贴板分享文本中的表现

在 `app.c22a91a0.js` 的 `getShareText` 中：

```javascript
// 当开关开启且名称非空时，将输入法名称加入成绩分享串
["inputMethod", "输入法:".concat(i)]
```

---

## 3. dazitui 现有实现比对

| 项目 | dazitui 现状 (`dazitui-core` / `dazitui`) | 52dazi 官方协议 |
| :--- | :--- | :--- |
| **上传 Payload (`share.rs`)** | `build_upload_payload` 硬编码 `"inputMethod": ""` | 支持任意 UTF-8 字符串（如 `"虎码"`，最大长度 20） |
| **持久化设置 (`settings.rs`)** | 目前包含 `theme`, `reference_ratio`, `bold`, `font` | 包含 `input_method`（或 `input_method_name`） |
| **设置存储文件** | `~/.config/dazitui/settings` (极简 key=value 格式) | IndexedDB (key=value/json) |
| **设置视图 (`main.rs`)** | 目前有 4 个焦点项（主题/占比/粗体/字体） | 成绩设置中可配置输入法 |
| **剪贴板分享 (`lib.rs`)** | `format_stats_share_text` 统一富格式，含 速度/击键/码长/正确字数/错字/回改/键数/键准/打词率/用时，在线比赛额外追加排名 | 可选包含输入法名称 |

---

## 4. 在 dazitui 中支持设置的可行性与方案设计

**结论：完全可行且改动清晰轻量。**

### 4.1 核心改动点

#### 1) `dazitui-core/src/settings.rs`
- 在 `Settings` 结构体中增加字段：
  ```rust
  pub struct Settings {
      pub theme: ThemePreset,
      pub reference_ratio: u8,
      pub bold: bool,
      pub font: bool,
      /// 用户输入法名称（上传 52dazi 时携带，限长 20 字符）。
      pub input_method: String,
  }
  ```
- 默认值：`input_method: String::new()`（或根据需要预设）。
- `SettingsStore` 的 `save` 与 `load` 支持 `input_method=<name>`（自动截断至 20 字符）。

#### 2) `dazitui-core/src/online/share.rs` 与 `client.rs`
- 修改 `build_upload_payload`，接受 `input_method: &str` 参数（或通过 `Settings` 传入）：
  ```rust
  "inputMethod": input_method,
  ```
- 修改 `upload_session`（或透传参数），使上传时从应用设置中读取 `settings.input_method`。

#### 3) `dazitui` TUI 交互 (`dazitui/src/main.rs`)
- 在 `Settings` 视图（`F3` 或 `AppState::Settings`）中增加一行「输入法」：
  - 方案 A（轻量）：允许直接在设置文件中配置 `~/.config/dazitui/settings` 中写 `input_method=虎码`，TUI 视图仅展示当前已配置的输入法名称。
  - 方案 B（完整交互）：在 Settings 视图中按 Enter/编辑键弹窗或进入行编辑模式，输入法名称可直接在 TUI 中输入/修改并即时保存。

#### 4) 剪贴板分享文本（可选联动）
- `format_stats_share_text` 统一所有来源（离线/自由发文/剪贴板/内置/在线比赛）的复制文本口径，在线比赛上传成功时额外在来源名后追加排名（如 `极速杯 第5名《...》 · 🚀WPM 85.2 · ... · 🎯键准 97.29% · ... · 虎码 🖥️dazitui`）。

---

## 5. 总结

1. 52dazi 排行榜中的「输入法」栏数据来自于客户端在 `/Api/Rank/uploadResult` 请求体中上报的 `inputMethod` 字段。
2. 目前 dazitui 已在 `dazitui-core/src/online/share.rs` 的 `build_upload_payload` 中保留了该字段位置（占位为 `""`）。
3. 只要在 `Settings` 中增加 `input_method` 配置项，并在 `build_upload_payload` 中填入该值，用户通过 dazitui 完成在线赛文跟打并上传成绩后，52dazi 排行榜即会正常显示用户的输入法名称（如「虎码」）。
