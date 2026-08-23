# 意图
52dazi.cn 逆向协议实现与 HTTP API 交互客户端。

# 文件

## mod.rs
在线服务子模块聚合与条件编译暴露

## auth.rs
52dazi 认证状态判断与自动重登决策

## token.rs
登录 Token 本地文件存取与状态检测

## protocol.rs
52dazi AES-128-CBC 加解密与网络包封包解析

## client.rs
52dazi HTTP API 客户端，封装登录、获取赛文与上传成绩请求

## share.rs
成绩上传载荷组装、分享文本格式化与 OSC 52 剪贴板复制序列
