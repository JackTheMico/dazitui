# 意图
核心业务模块实现，对外暴露赛文模型、跟打会话、配置持久化及在线客户端 API。

# 文件

## lib.rs
库根入口，定义赛文结构体 Text、来源 TextSource、内置文集 BuiltinSet 与文本过滤清洗逻辑

## session.rs
跟打练习状态机，跟踪字符状态、组边界推进门槛、错字/回改与 WPM/击键频率统计

## settings.rs
用户设置与主题预设持久化，管理色值映射与 Kitty OSC 50 终端字体大小序列

## online/
52dazi.cn 逆向协议实现与 HTTP API 交互客户端。
