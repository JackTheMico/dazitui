//! 编码提示（遍码提示）渲染布局：将每词最优编码提示排版到正文行之上，逐词对齐。

use unicode_width::UnicodeWidthChar;

use crate::scheme::CodeHint;

/// 空格并击简词在提示区附加的空格键标记（U+2423 OPEN BOX），宽度 1、跨终端可读。
const SPACE_CHORD_MARK: char = '␣';

/// 简码/并击码的手区归属，用于提示区配色（左手粉、右手黄、双手并击青）。
///
/// 由编码的前导手区修饰符推断：`_` 左手、`+` 右手、`-` 其它；无前缀（双手并击或普通码）为
/// `TwoHand`；`None` 仅用于已打/缺失提示（留空占位，不显色）。
/// 该枚举不依赖 ratatui，颜色映射由渲染层（`dazitui`）据此施加。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintHand {
    Left,
    Right,
    Other,
    /// 无前缀的双手并击/普通全码（原 muted 灰，现单独配色）。
    TwoHand,
    /// 已全对上屏或缺失提示的留空占位（不显色）。
    None,
}

/// 提示区一个词单元的渲染单元：已按词格宽居中/留空的可视文本，与其手区归属（用于配色）。
pub struct HintCell {
    pub text: String,
    pub hand: HintHand,
}

/// 由编码推断其手区归属（用于提示区左右手/双手并击配色）。
///
/// 空格并击简词以 `%` 为前缀（`%XY` 双手+空格 / `%_X` 左手+空格 / `%+X` 右手+空格），
/// 手区归属由 `%` 之后的字符推断（与无 `%` 的并击码一致）。
pub fn hand_of_code(code: &str) -> HintHand {
    let c = code.strip_prefix('%').unwrap_or(code);
    if c.starts_with('_') {
        HintHand::Left
    } else if c.starts_with('+') {
        HintHand::Right
    } else if c.starts_with('-') {
        HintHand::Other
    } else {
        HintHand::TwoHand
    }
}

/// 空明码方案 B 权威 204 个一击单字的编码格式与手区归属表（字符、编码提示文本、手区归属）。
///
/// 涵盖 8 大槽位规范编码：
/// - 左手小写（26 字，a-z）
/// - 右手小写（22 字，b,d,e,f,g,h,i,j,k,l,m,n,o,p,q,r,s,t,u,v,w,y）
/// - 左手大写（26 字，A-Z）
/// - 右手大写（26 字，A-Z）
/// - 左手小写+空格（26 字，a␣-z␣）
/// - 右手小写+空格（26 字，a␣-z␣）
/// - 左手大写+空格（26 字，A␣-Z␣）
/// - 右手大写+空格（26 字，A␣-Z␣）
pub const KONGMING_1HIT_CHORDS: &[(char, &str, HintHand)] = &[
    // Tier 0 左手 (15) & 右手 (11)
    ('中', "f", HintHand::Left),
    ('来', "r", HintHand::Left),
    ('上', "s", HintHand::Left),
    ('大', "d", HintHand::Left),
    ('为', "w", HintHand::Left),
    ('国', "g", HintHand::Left),
    ('地', "a", HintHand::Left),
    ('要', "q", HintHand::Left),
    ('会', "v", HintHand::Left),
    ('而', "e", HintHand::Left),
    ('下', "x", HintHand::Left),
    ('成', "c", HintHand::Left),
    ('天', "t", HintHand::Left),
    ('部', "b", HintHand::Left),
    ('在', "z", HintHand::Left),
    ('的', "d", HintHand::Right),
    ('是', "s", HintHand::Right),
    ('不', "b", HintHand::Right),
    ('人', "r", HintHand::Right),
    ('我', "w", HintHand::Right),
    ('他', "t", HintHand::Right),
    ('这', "v", HintHand::Right),
    ('个', "g", HintHand::Right),
    ('发', "f", HintHand::Right),
    ('到', "e", HintHand::Right),
    ('去', "q", HintHand::Right),
    // Tier 1 跨键小写 左手 (11) & 右手 (11)
    ('以', "i", HintHand::Left),
    ('也', "y", HintHand::Left),
    ('能', "n", HintHand::Left),
    ('于', "u", HintHand::Left),
    ('过', "o", HintHand::Left),
    ('里', "l", HintHand::Left),
    ('家', "j", HintHand::Left),
    ('方', "p", HintHand::Left),
    ('么', "m", HintHand::Left),
    ('看', "k", HintHand::Left),
    ('好', "h", HintHand::Left),
    ('一', "y", HintHand::Right),
    ('了', "l", HintHand::Right),
    ('有', "i", HintHand::Right),
    ('们', "m", HintHand::Right),
    ('和', "h", HintHand::Right),
    ('就', "j", HintHand::Right),
    ('可', "k", HintHand::Right),
    ('你', "n", HintHand::Right),
    ('年', "p", HintHand::Right),
    ('用', "u", HintHand::Right),
    ('多', "o", HintHand::Right),
    // Tier 2 单手大写码元 左手 (26) & 右手 (26)
    ('性', "A", HintHand::Left),
    ('本', "B", HintHand::Left),
    ('出', "C", HintHand::Left),
    ('对', "D", HintHand::Left),
    ('二', "E", HintHand::Left),
    ('法', "F", HintHand::Left),
    ('公', "G", HintHand::Left),
    ('后', "H", HintHand::Left),
    ('知', "I", HintHand::Left),
    ('经', "J", HintHand::Left),
    ('开', "K", HintHand::Left),
    ('理', "L", HintHand::Left),
    ('没', "M", HintHand::Left),
    ('那', "N", HintHand::Left),
    ('三', "O", HintHand::Left),
    ('点', "P", HintHand::Left),
    ('起', "Q", HintHand::Left),
    ('然', "R", HintHand::Left),
    ('说', "S", HintHand::Left),
    ('同', "T", HintHand::Left),
    ('正', "U", HintHand::Left),
    ('业', "V", HintHand::Left),
    ('无', "W", HintHand::Left),
    ('学', "X", HintHand::Left),
    ('样', "Y", HintHand::Left),
    ('子', "Z", HintHand::Left),
    ('定', "A", HintHand::Right),
    ('把', "B", HintHand::Right),
    ('从', "C", HintHand::Right),
    ('得', "D", HintHand::Right),
    ('儿', "E", HintHand::Right),
    ('分', "F", HintHand::Right),
    ('工', "G", HintHand::Right),
    ('行', "H", HintHand::Right),
    ('主', "I", HintHand::Right),
    ('进', "J", HintHand::Right),
    ('但', "K", HintHand::Right),
    ('力', "L", HintHand::Right),
    ('面', "M", HintHand::Right),
    ('内', "N", HintHand::Right),
    ('者', "O", HintHand::Right),
    ('十', "P", HintHand::Right),
    ('其', "Q", HintHand::Right),
    ('如', "R", HintHand::Right),
    ('时', "S", HintHand::Right),
    ('她', "T", HintHand::Right),
    ('第', "U", HintHand::Right),
    ('使', "V", HintHand::Right),
    ('外', "W", HintHand::Right),
    ('现', "X", HintHand::Right),
    ('因', "Y", HintHand::Right),
    ('着', "Z", HintHand::Right),
    // Tier 3 空格并击 左手小写+空格 (26)
    ('等', "a␣", HintHand::Left),
    ('并', "b␣", HintHand::Left),
    ('长', "c␣", HintHand::Left),
    ('道', "d␣", HintHand::Left),
    ('新', "e␣", HintHand::Left),
    ('己', "f␣", HintHand::Left),
    ('关', "g␣", HintHand::Left),
    ('还', "h␣", HintHand::Left),
    ('制', "i␣", HintHand::Left),
    ('军', "j␣", HintHand::Left),
    ('身', "k␣", HintHand::Left),
    ('两', "l␣", HintHand::Left),
    ('民', "m␣", HintHand::Left),
    ('加', "n␣", HintHand::Left),
    ('西', "o␣", HintHand::Left),
    ('斯', "p␣", HintHand::Left),
    ('前', "q␣", HintHand::Left),
    ('日', "r␣", HintHand::Left),
    ('生', "s␣", HintHand::Left),
    ('它', "t␣", HintHand::Left),
    ('月', "u␣", HintHand::Left),
    ('回', "v␣", HintHand::Left),
    ('问', "w␣", HintHand::Left),
    ('小', "x␣", HintHand::Left),
    ('意', "y␣", HintHand::Left),
    ('自', "z␣", HintHand::Left),
    // Tier 3 空格并击 左手大写+空格 (26)
    ('教', "A␣", HintHand::Left),
    ('表', "B␣", HintHand::Left),
    ('重', "C␣", HintHand::Left),
    ('当', "D␣", HintHand::Left),
    ('原', "E␣", HintHand::Left),
    ('东', "F␣", HintHand::Left),
    ('果', "G␣", HintHand::Left),
    ('或', "H␣", HintHand::Left),
    ('声', "I␣", HintHand::Left),
    ('将', "J␣", HintHand::Left),
    ('提', "K␣", HintHand::Left),
    ('老', "L␣", HintHand::Left),
    ('美', "M␣", HintHand::Left),
    ('及', "N␣", HintHand::Left),
    ('员', "O␣", HintHand::Left),
    ('解', "P␣", HintHand::Left),
    ('情', "Q␣", HintHand::Left),
    ('水', "R␣", HintHand::Left),
    ('事', "S␣", HintHand::Left),
    ('体', "T␣", HintHand::Left),
    ('名', "U␣", HintHand::Left),
    ('真', "V␣", HintHand::Left),
    ('文', "W␣", HintHand::Left),
    ('心', "X␣", HintHand::Left),
    ('已', "Y␣", HintHand::Left),
    ('作', "Z␣", HintHand::Left),
    // Tier 3 空格并击 右手小写+空格 (26)
    ('由', "a␣", HintHand::Right),
    ('被', "b␣", HintHand::Right),
    ('此', "c␣", HintHand::Right),
    ('都', "d␣", HintHand::Right),
    ('最', "e␣", HintHand::Right),
    ('手', "f␣", HintHand::Right),
    ('高', "g␣", HintHand::Right),
    ('很', "h␣", HintHand::Right),
    ('应', "i␣", HintHand::Right),
    ('机', "j␣", HintHand::Right),
    ('战', "k␣", HintHand::Right),
    ('利', "l␣", HintHand::Right),
    ('明', "m␣", HintHand::Right),
    ('向', "n␣", HintHand::Right),
    ('政', "o␣", HintHand::Right),
    ('相', "p␣", HintHand::Right),
    ('只', "q␣", HintHand::Right),
    ('任', "r␣", HintHand::Right),
    ('所', "s␣", HintHand::Right),
    ('头', "t␣", HintHand::Right),
    ('见', "u␣", HintHand::Right),
    ('什', "v␣", HintHand::Right),
    ('物', "w␣", HintHand::Right),
    ('些', "x␣", HintHand::Right),
    ('与', "y␣", HintHand::Right),
    ('之', "z␣", HintHand::Right),
    // Tier 3 空格并击 右手大写+空格 (26)
    ('代', "A␣", HintHand::Right),
    ('比', "B␣", HintHand::Right),
    ('产', "C␣", HintHand::Right),
    ('动', "D␣", HintHand::Right),
    ('信', "E␣", HintHand::Right),
    ('化', "F␣", HintHand::Right),
    ('合', "G␣", HintHand::Right),
    ('话', "H␣", HintHand::Right),
    ('给', "I␣", HintHand::Right),
    ('间', "J␣", HintHand::Right),
    ('世', "K␣", HintHand::Right),
    ('立', "L␣", HintHand::Right),
    ('门', "M␣", HintHand::Right),
    ('次', "N␣", HintHand::Right),
    ('度', "O␣", HintHand::Right),
    ('常', "P␣", HintHand::Right),
    ('全', "Q␣", HintHand::Right),
    ('先', "R␣", HintHand::Right),
    ('实', "S␣", HintHand::Right),
    ('特', "T␣", HintHand::Right),
    ('海', "U␣", HintHand::Right),
    ('通', "V␣", HintHand::Right),
    ('位', "W␣", HintHand::Right),
    ('想', "X␣", HintHand::Right),
    ('又', "Y␣", HintHand::Right),
    ('种', "Z␣", HintHand::Right),
];

/// 查找指定字符在空明码一击字中的物理指法与手区归属。
pub fn kongming_1hit_hint(c: char) -> Option<(&'static str, HintHand)> {
    KONGMING_1HIT_CHORDS
        .iter()
        .find(|&&(ch, _, _)| ch == c)
        .map(|&(_, chord, hand)| (chord, hand))
}

pub const KONGMING_1HIT_WORDS: &[(&str, &str)] = &[
    ("可以", "k'"),
    ("一个", "Uy"),
    ("自己", "oj"),
    ("没有", "m'"),
    ("我们", "Uw"),
    ("这个", "Vg"),
    ("问题", "ot"),
    ("中国", "Uz"),
    ("不错", "B7"),
    ("什么", "Ur"),
    ("进行", "7j"),
    ("还是", "h~"),
    ("使用", "2s"),
    ("数据", ":S"),
    ("需要", "UX"),
    ("公司", "7G"),
    ("学习", "uX"),
    ("但是", "D'"),
    ("都是", "D~"),
    ("如果", "or"),
    ("如何", ":R"),
    ("时间", "os"),
    ("喜欢", "x!"),
    ("因为", "w'"),
    ("技术", "jS"),
    ("时候", "Us"),
    ("他们", "Ut"),
    ("很多", "8h"),
    ("一些", "8y"),
    ("比较", "8E"),
    ("感觉", ";g"),
    ("不是", "uu"),
    ("通过", "9T"),
    ("选择", ";X"),
    ("现在", "Ux"),
    ("非常", ";f"),
    ("所以", "y'"),
    ("也是", ":y"),
    ("工作", "Ug"),
    ("服务", "oW"),
    ("可能", ";k"),
    ("觉得", "u'"),
    ("发展", "F0"),
    ("其他", "q~"),
    ("已经", "Ua"),
    ("味道", "/w"),
    ("开始", "K'"),
    ("孩子", "EH"),
    ("知道", "oz"),
    ("分析", "f`"),
    ("市场", "9E"),
    ("美国", "/m"),
    ("应该", "9y"),
    ("价格", "j?"),
    ("这些", "Vx"),
    ("还有", "UH"),
    ("包括", ".B"),
    ("这样", "Vy"),
    ("产品", ":C"),
    ("系统", "9I"),
    ("大家", "d'"),
    ("提供", "tG"),
    ("一般", "Ey"),
    ("不过", ":B"),
    ("内容", "8I"),
    ("提高", "9t"),
    ("一下", "y`"),
    ("环境", ",H"),
    ("方法", "F8"),
    ("朋友", "p'"),
    ("生活", "H'"),
    ("语言", "Y'"),
    ("就是", "Uj"),
    ("怎么", "Ui"),
    ("情况", "8q"),
    ("影响", ";a"),
    ("企业", "9q"),
    ("网络", ",W"),
    ("根据", "G'"),
    ("作为", "8Z"),
    ("东西", ".D"),
    ("真的", ",z"),
    ("设计", "s@"),
    ("发现", "f'"),
    ("这种", "VZ"),
    ("不同", "7B"),
    ("个人", "/g"),
    ("或者", "oE"),
    ("为什么", "gi"),
    ("帮助", "B@"),
    ("质量", "z`"),
    ("而且", "8e"),
    ("那么", "N8"),
    ("方式", "F9"),
    ("活动", ":H"),
    ("对于", "7D"),
    ("管理", "ol"),
    ("主要", ":Z"),
    ("看到", ";K"),
    ("能力", ";n"),
    ("教育", "Ej"),
    ("了解", "l8"),
    ("文化", ",w"),
    ("世界", ",s"),
    ("其实", "7q"),
    ("这里", "Vl"),
    ("健康", "jK"),
    ("一直", ",y"),
    ("国家", "9G"),
    ("同时", "T8"),
    ("然后", "8R"),
    ("不能", "8B"),
    ("这是", "Vs"),
    ("地方", "9d"),
    ("不要", "7B"),
    ("日本", "r`"),
    ("成为", "8c"),
    ("方面", "7F"),
    ("结果", "j0"),
    ("训练", "Xl"),
    ("专业", "Z*"),
    ("重要", "7Z"),
    ("最后", "Z`"),
    ("能够", "7n"),
    ("安全", "A'"),
    ("目前", "uM"),
    ("经济", "Uu"),
    ("之后", "8z"),
    ("出现", "7C"),
    ("不会", "BH"),
    ("一样", ".y"),
    ("历史", "7l"),
    ("哪些", "N0"),
    ("希望", "x0"),
    ("学生", "iX"),
    ("以及", "y<"),
    ("例如", "l~"),
    ("手机", ";S"),
    ("一起", ";y"),
    ("虽然", "7S"),
    ("认为", "9r"),
    ("社会", "oh"),
    ("相关", "xG"),
    ("研究", "oY"),
    ("行业", "hy"),
    ("注意", "Z~"),
    ("出来", "C`"),
    ("实现", "s`"),
    ("一定", "7y"),
    ("大学", "D8"),
    ("支持", "!z"),
    ("关系", "oG"),
    ("其中", "9U"),
    ("无法", "?W"),
    ("里面", "Ii"),
    ("学校", "?X"),
    ("表示", "b9"),
    ("所有", "9S"),
    ("项目", "xM"),
    ("只有", "9z"),
    ("只是", "U~"),
    ("要求", "7Y"),
    ("好吃", "/H"),
    ("未来", "w0"),
    ("参考", ";C"),
    ("自然", "7z"),
    ("北京", "b'"),
    ("文章", "wZ"),
    ("为了", "Oi"),
    ("起来", "qL"),
    ("目标", ":M"),
    ("效果", ";b"),
    ("这么", "Vm"),
    ("直接", "9j"),
    ("各种", "7G"),
    ("今天", ":j"),
    ("理解", "l*"),
    ("特别", "7t"),
    ("计划", "8j"),
    ("老师", "L'"),
    ("更加", "gj"),
    ("风险", "fx"),
    ("网站", ".W"),
    ("一点", "e."),
    ("分类", "fl"),
    ("旅游", ";L"),
    ("增加", "U`"),
    ("解决", "j'"),
    ("比如", "8b"),
    ("原因", ";Y"),
    ("以上", "!y"),
    ("值得", "8z"),
    ("需求", "Xq"),
    ("因此", "9y"),
    ("金融", "jR"),
    ("基本", "UJ"),
    ("考虑", "9K"),
    ("那个", "N7"),
    ("发生", ".F"),
    ("以后", "y!"),
    ("知识", "Ez"),
    ("当时", "oD"),
    ("行为", "x:"),
    ("运动", "8Y"),
    ("有些", ",Y"),
    ("国际", "UG"),
    ("地区", "7d"),
    ("身体", ";s"),
    ("关于", "G~"),
    ("过程", "G8"),
    ("你们", "Un"),
    ("存在", "8C"),
    ("后来", "EH"),
    ("几个", "U~"),
    ("作品", "Z`"),
    ("由于", "/Y"),
    ("利用", "E~"),
    ("部分", "8B"),
    ("当然", "9D"),
    ("保护", "BH"),
    ("简单", "j8"),
    ("具体", "Jt"),
    ("速度", "S'"),
    ("政府", "oZ"),
    ("微信", ";w"),
    ("事情", "7s"),
    ("人员", "ro"),
    ("之间", "?z"),
    ("继续", "9j"),
    ("容易", "!R"),
    ("自动", ";z"),
    ("社区", "/s"),
    ("任何", "!r"),
    ("不断", "8v"),
    ("甚至", "9s"),
    ("完全", "/W"),
    ("准备", "Z~"),
    ("具有", "7J"),
    ("成功", ",S"),
    ("别人", "7b"),
    ("全球", "Qq"),
    ("家庭", ".t"),
    ("完成", ";W"),
    ("经常", "jC"),
    ("科技", "kj"),
    ("人类", "r`"),
    ("有的", ".Y"),
    ("香港", "xG"),
    ("分享", "f~"),
    ("建立", "j@"),
    ("确定", ";q"),
    ("任务", "r~"),
    ("水平", "7I"),
    ("交通", "jT"),
    ("得到", "d~"),
    ("第一", "8d"),
    ("很好", "7h"),
    ("变化", "9b"),
    ("软件", "uR"),
    ("参加", ",C"),
    ("表现", "b8"),
    ("政策", "9v"),
    ("英语", "y9"),
    ("机构", "jG"),
    ("测试", "7c"),
    ("基础", "I`"),
    ("组织", "Eo"),
    ("并不", "I~"),
    ("台湾", "TW"),
    ("人们", "Ir"),
    ("价值", "7j"),
    ("电脑", "d7"),
    ("首先", ".S"),
    ("单位", "8D"),
    ("业务", "yW"),
    ("找到", ";Z"),
    ("生成", "s7"),
    ("不好", "B8"),
    ("产生", "7C"),
    ("必须", "Eb"),
    ("同学", "T9"),
    ("怎样", "Vu"),
    ("不知道", "B2"),
    ("经验", ";j"),
    ("他的", "/T"),
    ("开发", ":K"),
    ("许多", ":X"),
    ("建设", "ov"),
    ("是不是", "@s"),
    ("结构", "9E"),
    ("只能", "z0"),
    ("机会", "8j"),
    ("达到", "7v"),
    ("答案", "iD"),
    ("决定", "J'"),
    ("接受", "jS"),
    ("那些", "8N"),
    ("生产", "7s"),
    ("并且", ":b"),
    ("空间", "uK"),
    ("销售", "xS"),
    ("看看", "?K"),
    ("多少", "DS"),
    ("代表", "9U"),
    ("免费", "m:"),
    ("政治", "Uv"),
    ("广告", "/G"),
    ("位置", "w8"),
    ("可是", ":k"),
    ("效率", "o'"),
    ("造成", "EZ"),
    ("艺术", "y<"),
    ("商品", "S`"),
    ("科学", "Ek"),
    ("越来越", "@Y"),
    ("妈妈", "?M"),
    ("晚上", "W9"),
    ("作用", "Ev"),
    ("风格", "fg"),
    ("时代", "7`"),
    ("全国", "8E"),
    ("部门", "B`"),
    ("改变", "8v"),
    ("联系", "l0"),
    ("趋势", ";Q"),
    ("只要", "8I"),
    ("实际", ":s"),
    ("好的", "H7"),
    ("大量", "8D"),
    ("确实", "!Q"),
    ("宝宝", "!B"),
    ("等等", "/d"),
    ("过去", "8G"),
    ("说明", "8S"),
    ("团队", "ET"),
    ("告诉", "iG"),
    ("电话", "d0"),
    ("更新", "g8"),
    ("整个", "z~"),
    ("状态", "/Z"),
    ("正常", "/z"),
    ("到了", "7D"),
    ("按照", "AZ"),
    ("条件", "t'"),
    ("方案", "F'"),
    ("好像", "!H"),
    ("心理", "8x"),
    ("规定", "9G"),
    ("实在", "sZ"),
    ("方向", "7U"),
    ("先生", "oX"),
    ("下来", "y~"),
    ("现场", "xC"),
    ("建筑", "jZ"),
    ("明显", "Em"),
    ("正在", ".z"),
    ("它们", "UT"),
    ("兴趣", "xQ"),
    ("普通", "P'"),
    ("法律", "8F"),
    ("不用", "/B"),
    ("女人", "EN"),
    ("真正", "<z"),
    ("受到", "9S"),
    ("精神", "oJ"),
    ("随着", "E`"),
    ("于是", "E~"),
    ("经过", "7E"),
    ("吃饭", ";c"),
    ("时期", "7`"),
    ("文件", "w7"),
    ("消费者", "@x"),
    ("人生", ":r"),
    ("关键", "G7"),
    ("有关", "I~"),
    ("特点", "8t"),
    ("总是", "C0"),
    ("形成", "9x"),
    ("意义", "7U"),
    ("程序", ":c"),
    ("优势", "iY"),
    ("如此", "7R"),
    ("而是", "7e"),
    ("计算机", "@u"),
    ("范围", "9F"),
    ("电视", "ds"),
    ("本人", "br"),
    ("男人", "N~"),
    ("相信", "x!"),
    ("决策", "Jc"),
    ("自由", ":z"),
    ("满足", "/M"),
    ("肯定", "7k"),
    ("老板", "L0"),
    ("程度", "q`"),
    ("认识", "7r"),
    ("第二", "8d"),
    ("然而", "9e"),
    ("满意", ";M"),
    ("有没有", "@i"),
    ("提出", "7t"),
    ("苹果", "Ep"),
    ("下午", "Ia"),
    ("阶段", "9I"),
    ("这次", "Vc"),
    ("课程", "kc"),
    ("调查", "dC"),
    ("欧洲", "OZ"),
    ("一切", "y!"),
    ("现实", "Ex"),
    ("核心", "hx"),
    ("不仅", "9B"),
    ("分别", "f8"),
    ("行动", "xD"),
    ("各位", "gw"),
    ("办法", "8U"),
    ("突然", "T`"),
    ("并不是", "@b"),
    ("曾经", "Ec"),
    ("目的", "M'"),
    ("生命", "!s"),
    ("结合", "7v"),
    ("现代", "xD"),
    ("判断", "/P"),
    ("负责", "?F"),
    ("全面", "Qm"),
    ("儿子", "ez"),
    ("有什么", "Y@"),
    ("制度", "Oz"),
    ("输入", "LR"),
    ("打开", ":D"),
    ("合理", "hl"),
    ("引起", "Ey"),
    ("爱情", "7a"),
    ("合适", "h`"),
    ("补充", "BC"),
    ("能源", "nY"),
    ("根本", "Eg"),
    ("以为", "ou"),
    ("搜索", "!S"),
    ("改革", "U`"),
    ("明确", "m7"),
    ("责任", "zr"),
    ("正确", "7z"),
    ("文字", "wz"),
    ("原来", "8Y"),
    ("形式", "9x"),
    ("情绪", "qX"),
    ("十分", "7E"),
    ("让人", "oR"),
    ("进一步", "@j"),
    ("练习", ";l"),
    ("平均", "p8"),
    ("力量", "El"),
    ("去年", "Qn"),
    ("距离", "Jl"),
    ("安排", "AP"),
    ("运行", "Y8"),
    ("状况", "9Z"),
    ("回来", "oH"),
    ("自我", "zW"),
    ("差不多", "@C"),
    ("声音", "7~"),
    ("我们的", "i9"),
    ("好多", "H8"),
    ("地址", "Ed"),
    ("取得", "Qd"),
    ("区域", "Qy"),
    ("领导", "l@"),
    ("平时", "p7"),
    ("清楚", "qC"),
    ("我觉得", "ri"),
    ("大部分", "@D"),
    ("排名", "oP"),
    ("安装", "@A"),
    ("女儿", "Ne"),
    ("估计", "G0"),
    ("感到", "7I"),
    ("区别", "Qb"),
    ("不如", "B'"),
    ("现象", "x7"),
    ("本来", "!b"),
    ("教学", "jX"),
    ("父亲", "fa"),
    ("幸福", ":x"),
    ("天气", "t7"),
    ("成长", "c7"),
    ("唯一", "ow"),
    ("理论", ".l"),
    ("感情", "7g"),
    ("下载", "xZ"),
    ("空气", "K7"),
    ("内部", "nB"),
    ("细节", "/x"),
    ("女孩", "N_"),
    ("掌握", "ZW"),
    ("参数", "/C"),
    ("举行", "Jx"),
    ("歌曲", "g0"),
    ("哪里", "N9"),
    ("添加", "tJ"),
    ("我国", "7W"),
    ("似乎", "s9"),
    ("心里", ",x"),
    ("让我", "RW"),
    ("即使", "j~"),
    ("利润", "lR"),
    ("思想", "8s"),
    ("形象", "x9"),
    ("期待", ":q"),
    ("看起来", "K2"),
    ("爸爸", "?b"),
    ("样子", "EV"),
    ("永远", "7Y"),
    ("也许", "i'"),
    ("句子", "Jz"),
    ("面包", "m0"),
    ("家人", "jr"),
    ("流程", "l'"),
    ("工业", "8U"),
    ("出去", "C~"),
    ("好好", "?H"),
    ("每个人", "m`"),
    ("所谓", "7S"),
    ("卫生", "ws"),
    ("怎么样", "@o"),
    ("居民", "Jm"),
    ("后面", ";H"),
    ("仍然", "Er"),
    ("认真", ".r"),
    ("医学", "yX"),
    ("意识", "Ee"),
    ("重复", "C8"),
    ("集中", "jZ"),
    ("创造", "8C"),
    ("人民", "r'"),
    ("前往", "qW"),
    ("铁路", "tL"),
    ("反正", ":F"),
    ("轻松", "qS"),
    ("实践", "s~"),
    ("往往", "7W"),
    ("看来", "@K"),
    ("一个人", "y`"),
    ("有时候", "@i"),
    ("人才", "r0"),
    ("眼睛", "oo"),
    ("物质", "9W"),
    ("对手", "DS"),
    ("发表", "/F"),
    ("监督", "jD"),
    ("投入", "tr"),
    ("不管", "BG"),
    ("机场", "jC"),
    ("休息", "ox"),
    ("心情", ";x"),
    ("病毒", "bD"),
    ("明白", "8m"),
    ("飞机", "fj"),
    ("实验", "9v"),
    ("完善", "EW"),
    ("给我", "gW"),
    ("例子", "l`"),
    ("刚刚", ":G"),
    ("小学", "xX"),
    ("提醒", "t0"),
    ("常用", ".C"),
    ("充满", "C@"),
    ("日常", "/r"),
    ("身上", "Uo"),
    ("各个", "gg"),
    ("她的", ",T"),
    ("人体", "rt"),
    ("统一", "7T"),
    ("具备", "8J"),
    ("困难", "KN"),
    ("热情", "rE"),
    ("实力", ".s"),
    ("确认", "Qr"),
    ("地位", "7d"),
    ("那样", "9N"),
    ("最多", "Z@"),
    ("分配", "fp"),
    ("周末", "Z7"),
    ("植物", "zW"),
    ("拒绝", "9J"),
    ("农业", "7N"),
    ("难以", "Ny"),
    ("强大", "q:"),
    ("第三", "8d"),
    ("让你", "Rn"),
    ("早上", "Z9"),
    ("如今", "/R"),
    ("一部分", "@y"),
    ("我想", "W'"),
    ("可爱", "ka"),
    ("访问", "Fw"),
    ("不行", "B~"),
    ("更多", ",g"),
    ("群体", "Qt"),
    ("文明", "wm"),
    ("学科", "Xk"),
    ("特殊", ":t"),
    ("才能", "8n"),
    ("指出", "z`"),
    ("外面", "W0"),
    ("中间", "Z8"),
    ("心态", "xT"),
    ("编程", "/b"),
    ("进步", "j9"),
    ("上午", "oa"),
    ("不敢", "BG"),
    ("梦想", ":m"),
    ("维持", "wc"),
    ("可惜", "9k"),
    ("看见", "K9"),
    ("千万", ",q"),
    ("农村", "NC"),
];


/// 查找指定词组在空明码一击词中的编码与手区归属。
pub fn kongming_1hit_word_hint(word: &str) -> Option<(&'static str, HintHand)> {
    KONGMING_1HIT_WORDS
        .iter()
        .find(|&&(w, _)| w == word)
        .map(|&(_, chord)| (chord, HintHand::TwoHand))
}


/// 单个字符的可视列宽（CJK 等宽字符记 2，其余记 1）。
pub fn char_width(c: char) -> usize {
    UnicodeWidthChar::width(c).unwrap_or(1).max(1)
}

/// 字符串的可视列宽（按字符累加，CJK 记 2）。
pub fn display_width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

/// 提示单元的实际显示文本：去掉手区修饰符与 `%` 前缀，空格并击简词追加空格键标记。
///
/// 上屏/选重键由 `SchemeDict` 直接写进 `code`（`␣` / `;` / `'` / 位次数字，ADR 0014 D2），
/// 渲染层原样呈现，保证「提示串 = 要按的键序列」。
fn hint_display_text(code: &str) -> String {
    let mut s = strip_hand_prefix(code).to_string();
    if code.starts_with('%') {
        s.push(SPACE_CHORD_MARK);
    }
    s
}

/// 每个词格的列宽：`max(词可视宽, 提示码可视宽)`。
///
/// 提示码必须完整可见才具备「照着打」的价值，因此词格宽度由编码而非词宽兜底：
/// 「腕间」可视宽 4 列，但其在 yoyo-pure 下逐字拼接的编码 `HjYIw` 为 5 列，故格宽取 5。
/// 列宽与「该词是否已上屏」无关（`typed_mask` 只决定提示是否留空），
/// 否则打完一词后整行会抖动。
pub fn hint_cell_widths(words: &[String], hints: &[CodeHint]) -> Vec<usize> {
    words
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let word_w = display_width(w);
            let code_w = hints
                .get(i)
                .map(|h| display_width(&hint_display_text(&h.code)))
                .unwrap_or(0);
            word_w.max(code_w).max(1)
        })
        .collect()
}

/// 将单个提示码居中到目标列宽（词格宽），返回定宽字符串。
///
/// 目标列宽由 `hint_cell_widths` 保证 ≥ 码宽，故正常不截断；
/// 截断分支仅作调用方误传窄宽度时的兜底（避免补空格时 usize 下溢）。
fn format_hint_cell(code: &str, target_width: usize) -> String {
    let code_width = display_width(code);
    if code_width == target_width {
        return code.to_string();
    }
    if code_width > target_width {
        // 超宽截断到目标列宽（编码多为 ASCII，按字符截断等价）。
        let mut out = String::new();
        let mut w = 0;
        for c in code.chars() {
            let cw = char_width(c);
            if w + cw > target_width {
                break;
            }
            out.push(c);
            w += cw;
        }
        return out;
    }
    // 居中补空格使总长度等于目标列宽。
    let pad = target_width - code_width;
    let left = pad / 2;
    let right = pad - left;
    format!("{}{}{}", " ".repeat(left), code, " ".repeat(right))
}

/// 去掉编码的手区修饰符前缀（`_` 左手 / `+` 右手 / `-` 其它）与空格并击前缀 `%`，
/// 仅保留实际按键。
///
/// 简码（单手派生形式）与并击规范形式均可能带此前缀，空格并击简词另带 `%` 前缀，
/// 提示区只展示用户真正要按的键（空格键由 `SPACE_CHORD_MARK` 另行标记）。
fn strip_hand_prefix(code: &str) -> &str {
    let c = code.strip_prefix('%').unwrap_or(code);
    c.strip_prefix(['_', '+', '-']).unwrap_or(c)
}

/// 内置赛文对照区双行词格：生成本页「提示行」的逐词渲染单元。
///
/// `words` 为本页正文词（与 `hints` 同序），每个提示按对应词格宽（`max(词宽, 码宽)`）居中，
/// 词间以单空格分隔，使提示行与正文行（同样以单空格分词、并补齐到同一词格宽）逐词对齐。提示码会先去掉
/// 手区修饰符前缀（如单手简码 `_b` → `b`），仅显示实际按键；其手区归属（`HintHand`）
/// 一并返回，交由渲染层据左右手上色（左手粉、右手黄）。
///
/// `typed_mask[i]` 为真表示该词已全部正确上屏，其提示留空（仍按词格宽占位，不影响对齐）。
pub fn layout_code_hint_line(
    words: &[String],
    hints: &[CodeHint],
    typed_mask: &[bool],
) -> Vec<HintCell> {
    let widths = hint_cell_widths(words, hints);
    words
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let target = widths.get(i).copied().unwrap_or_else(|| display_width(w));
            let typed = typed_mask.get(i).copied().unwrap_or(false);
            if typed {
                return HintCell {
                    text: " ".repeat(target),
                    hand: HintHand::None,
                };
            }
            match hints.get(i) {
                Some(h) => {
                    let hand = hand_of_code(&h.code);
                    let code = hint_display_text(&h.code);
                    HintCell {
                        text: format_hint_cell(&code, target),
                        hand,
                    }
                }
                None => HintCell {
                    text: " ".repeat(target),
                    hand: HintHand::None,
                },
            }
        })
        .collect()
}

/// 贪心按词宽打包：返回若干「行」，每行包含若干词的索引，
/// 使该行（词宽之和 + 词间单空格分隔）不超过 `max_width`。
///
/// 若单个词宽已超过 `max_width`，则独占一行（由上层按原宽溢出渲染），
/// 保证永不分词错位、也不会产生空行或无限循环。
pub fn pack_words_by_width(word_widths: &[usize], max_width: usize) -> Vec<Vec<usize>> {
    let mut rows: Vec<Vec<usize>> = Vec::new();
    let mut cur: Vec<usize> = Vec::new();
    let mut cur_w = 0usize;
    for (i, &w) in word_widths.iter().enumerate() {
        let sep = if cur_w == 0 { 0 } else { 1 };
        if cur_w > 0 && cur_w + sep + w > max_width {
            rows.push(std::mem::take(&mut cur));
            cur_w = 0;
        }
        if cur_w > 0 {
            cur_w += 1; // 词间分隔空格
        }
        cur.push(i);
        cur_w += w;
    }
    if !cur.is_empty() {
        rows.push(cur);
    }
    rows
}

/// 双行词格（长文）：将提示行与正文行按词边界锁步折行，返回逐行 `(提示单元, 正文行)`。
///
/// 每词提示按对应词格宽（= max(词宽, 码宽)）居中，词间单空格分隔；正文行为对应词原文
/// 补齐到词格宽后同样单空格分隔。两者列结构完全一致，故每行提示与其下方正文逐词对齐、
/// 永不错位。提示码超词宽时正文补空格让位（而非截断提示）。提示单元携带手区归属，
/// 交由渲染层上色（左手粉、右手黄）。`typed_mask[i]` 为真表示该词已全部正确上屏，
/// 其提示单元留空但仍占位。
///
/// 与 `layout_code_hint_line`（内置分页单页单行）不同，本函数面向非内置长文：
/// 以 `WordIndex` 词边界为最小换行单元打包，使提示行与正文行折行点锁步。
pub fn layout_code_hint_grid(
    words: &[String],
    hints: &[CodeHint],
    typed_mask: &[bool],
    max_width: usize,
) -> Vec<(Vec<HintCell>, String)> {
    let widths: Vec<usize> = hint_cell_widths(words, hints);
    let rows = pack_words_by_width(&widths, max_width);
    rows.into_iter()
        .map(|row| {
            let row_words: Vec<String> = row.iter().map(|&i| words[i].clone()).collect();
            let row_hints: Vec<CodeHint> = row
                .iter()
                .map(|&i| {
                    hints.get(i).cloned().unwrap_or_else(|| CodeHint {
                        word: String::new(),
                        code: String::new(),
                        strokes: 0,
                        is_oov: true,
                        rank: 1,
                    })
                })
                .collect();
            let row_typed: Vec<bool> = row
                .iter()
                .map(|&i| typed_mask.get(i).copied().unwrap_or(false))
                .collect();
            let hint_cells = layout_code_hint_line(&row_words, &row_hints, &row_typed);
            // 正文词补尾随空格到词格宽，使提示行与正文行列结构一致。
            let body_line = row
                .iter()
                .map(|&i| {
                    let w = &words[i];
                    format!("{w}{}", " ".repeat(widths[i] - display_width(w)))
                })
                .collect::<Vec<_>>()
                .join(" ");
            (hint_cells, body_line)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheme::CodeHint;

    fn hint(code: &str) -> CodeHint {
        CodeHint {
            word: String::new(),
            code: code.to_string(),
            strokes: 0,
            is_oov: false,
            rank: 1,
        }
    }

    /// 将提示单元拼回单行文本（词间单空格），便于与既有对齐预期比较。
    fn cells_text(cells: &[HintCell]) -> String {
        cells
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn layout_single_char_centered() {
        // 单字「中」可视宽 2，提示 "k" 居中 → 定宽 2、右侧补 1 空格。
        let words = vec!["中".to_string()];
        let hints = vec![hint("k")];
        assert_eq!(
            cells_text(&layout_code_hint_line(&words, &hints, &[])),
            "k "
        );
    }

    #[test]
    fn layout_word_centered_and_joined() {
        // 「中」(2) + 「中国」(4)；「中国」提示 lgyinay(7) 宽于词 → 词格撑到 7 列完整显示；
        // 词间单空格分隔，提示行与正文行逐词对齐（正文行由 layout_code_hint_grid 补空格让位）。
        let words = vec!["中".to_string(), "中国".to_string()];
        let hints = vec![hint("k"), hint("lgyinay")];
        assert_eq!(
            cells_text(&layout_code_hint_line(&words, &hints, &[])),
            "k  lgyinay"
        );
    }

    #[test]
    fn layout_select_key_is_part_of_displayed_code() {
        // ADR 0014 D2：上屏/选重键由 SchemeDict 直接写进 `code`，渲染层原样呈现，
        // 保证「提示串 = 要按的键序列」；渲染层不再依 rank 二次追加 `·n`。
        let words = vec!["佳人".to_string()];
        // 万象虎字母表含 `;` → 第 2 位显示 `jgjr;`。
        let hints = vec![hint("jgjr;")];
        assert_eq!(
            cells_text(&layout_code_hint_line(&words, &hints, &[])),
            "jgjr;"
        );
        // 首选且唯一 → 不追加任何键。
        let hints1 = vec![hint("igqe")];
        assert_eq!(
            cells_text(&layout_code_hint_line(&words, &hints1, &[])),
            "igqe"
        );
    }

    #[test]
    fn layout_oov_is_blank_padded() {
        // 未登录词提示留空，但定宽占位（2 空格），不破坏对齐。
        let words = vec!["中".to_string()];
        let hints = vec![CodeHint {
            word: String::new(),
            code: String::new(),
            strokes: 0,
            is_oov: true,
            rank: 1,
        }];
        assert_eq!(
            cells_text(&layout_code_hint_line(&words, &hints, &[])),
            "  "
        );
    }

    #[test]
    fn layout_typed_word_is_blank_but_aligned() {
        // T04：已全对上屏的词，其上方提示留空（按词格宽占位，手区为 None）。
        // 「中」(2) 已打 → 2 空格；词间单空格分隔符；「中国」格宽 7 未打 → 显示 lgyinay。
        // 故提示行 = "  " + " " + "lgyinay" = "   lgyinay"（3 前导空格）。
        let words = vec!["中".to_string(), "中国".to_string()];
        let hints = vec![hint("k"), hint("lgyinay")];
        let typed_mask = vec![true, false];
        let cells = layout_code_hint_line(&words, &hints, &typed_mask);
        assert_eq!(cells_text(&cells), "   lgyinay");
        assert_eq!(cells[0].hand, HintHand::None);
    }

    #[test]
    fn layout_overwide_chord_widens_cell_instead_of_truncating() {
        // T05 修正：超宽并击码（wCsA，4 列）在单字（CJK 宽 2）词格内不再被截断，
        // 词格撑到 4 列完整显示——截断后的码无法照打，提示即失去意义。
        let words = vec!["中".to_string()];
        let hints = vec![hint("wCsA")];
        assert_eq!(
            cells_text(&layout_code_hint_line(&words, &hints, &[])),
            "wCsA"
        );
    }

    #[test]
    fn layout_code_wider_than_word_is_shown_in_full() {
        // 用户报告：「腕间」(4 列) 在 yoyo-pure 下无整词条目，逐字拼接得 腕=HjY + 间=Iw
        // → HjYIw（5 列）；旧行为按词宽截断为 HjYI，末位码元 w 丢失、无法照打。
        let words = vec!["腕间".to_string()];
        let hints = vec![hint("HjYIw")];
        let cells = layout_code_hint_line(&words, &hints, &[]);
        assert_eq!(cells_text(&cells), "HjYIw");
        assert_eq!(cells[0].hand, HintHand::TwoHand);
    }

    #[test]
    fn hint_cell_widths_takes_max_of_word_and_code() {
        // 词格宽 = max(词宽, 码宽)：腕间 4 vs HjYIw 5 → 5；中 2 vs k 1 → 2；
        // 世界 4 vs %XY（显示 "XY␣" 3 列）→ 4。
        let words = vec!["腕间".to_string(), "中".to_string(), "世界".to_string()];
        let hints = vec![hint("HjYIw"), hint("k"), hint("%XY")];
        assert_eq!(hint_cell_widths(&words, &hints), vec![5, 2, 4]);
    }

    #[test]
    fn layout_jianma_strips_hand_prefix_for_display() {
        // 简码（单手派生形式）带手区修饰符 _/+/-；提示区只显示实际按键，去掉前缀，
        // 并据手区归属（左手 Left / 右手 Right / 无 None）携出供渲染层上色。
        // 「中」(宽2) 简码 _b → 去掉前缀 "b"，居中宽2（奇宽补右侧空格）→ "b "，手区 Left。
        let words = vec!["中".to_string()];
        let hints = vec![hint("_b")];
        let cells = layout_code_hint_line(&words, &hints, &[]);
        assert_eq!(cells_text(&cells), "b ");
        assert_eq!(cells[0].hand, HintHand::Left);
        // 右手简码 +e 同样去掉前缀 → "e "，手区 Right。
        let hints2 = vec![hint("+e")];
        let cells2 = layout_code_hint_line(&words, &hints2, &[]);
        assert_eq!(cells_text(&cells2), "e ");
        assert_eq!(cells2[0].hand, HintHand::Right);
        // 无前缀并击码 wCs（3 列）宽于单字词格（2 列）→ 撑宽完整显示，手区 TwoHand。
        let hints3 = vec![hint("wCs")];
        let cells3 = layout_code_hint_line(&words, &hints3, &[]);
        assert_eq!(cells_text(&cells3), "wCs");
        assert_eq!(cells3[0].hand, HintHand::TwoHand);
    }

    #[test]
    fn layout_space_chord_strips_percent_adds_space_mark_and_assigns_hand() {
        // 空格并击简词（% 前缀）：提示区去掉 % 与手区前缀，仅显示实际按键，并在末尾
        // 附加空格键标记 ␣；手区归属据 % 后字符推断（左手 Left / 右手 Right / 无 TwoHand）。
        // 单字「中」(宽2)：%_v（左手+空格）→ "v␣"（宽2），手区 Left。
        let words = vec!["中".to_string()];
        let hints = vec![hint("%_v")];
        let cells = layout_code_hint_line(&words, &hints, &[]);
        assert_eq!(cells_text(&cells), "v␣");
        assert_eq!(cells[0].hand, HintHand::Left);
        // %+X（右手+空格）→ "X␣"，手区 Right。
        let hints2 = vec![hint("%+X")];
        let cells2 = layout_code_hint_line(&words, &hints2, &[]);
        assert_eq!(cells_text(&cells2), "X␣");
        assert_eq!(cells2[0].hand, HintHand::Right);
        // 双字「世界」(宽4)：%XY（双手+空格）→ "XY␣"（宽3，居中宽4→"XY␣ "），手区 TwoHand。
        let words3 = vec!["世界".to_string()];
        let hints3 = vec![hint("%XY")];
        let cells3 = layout_code_hint_line(&words3, &hints3, &[]);
        assert_eq!(cells_text(&cells3), "XY␣ ");
        assert_eq!(cells3[0].hand, HintHand::TwoHand);
    }

    #[test]
    fn hand_of_code_recognizes_space_chord_prefix() {
        // hand_of_code 应透过 % 前缀推断手区：%_X→Left、%+X→Right、%-X→Other、%XY→TwoHand。
        assert_eq!(hand_of_code("%_v"), HintHand::Left);
        assert_eq!(hand_of_code("%+X"), HintHand::Right);
        assert_eq!(hand_of_code("%-X"), HintHand::Other);
        assert_eq!(hand_of_code("%XY"), HintHand::TwoHand);
        // 无 % 时行为不变。
        assert_eq!(hand_of_code("wCs"), HintHand::TwoHand);
    }

    #[test]
    fn layout_grid_widens_body_to_fit_overwide_code() {
        // 提示码宽于词时，正文词补尾随空格让位（而非截断提示），两行仍锁步对齐。
        // 「中」格宽 2（码 k）；「腕间」词宽 4、码 HjYIw 宽 5 → 格宽 5，正文补 1 空格。
        let words = vec!["中".to_string(), "腕间".to_string()];
        let hints = vec![hint("k"), hint("HjYIw")];
        let rows = layout_code_hint_grid(&words, &hints, &[], 20);
        assert_eq!(rows.len(), 1);
        assert_lockstep_aligned(&rows);
        assert_eq!(rows[0].1, "中 腕间 ");
        assert_eq!(cells_text(&rows[0].0), "k  HjYIw");
    }

    #[test]
    fn layout_chord_centered_in_wide_word_width() {
        // T05：3 码并击 wCs 在双字（CJK 宽 4）提示区内居中为 "wCs "（右补 1 空格）。
        let words = vec!["世界".to_string()];
        let hints = vec![hint("wCs")];
        assert_eq!(
            cells_text(&layout_code_hint_line(&words, &hints, &[])),
            "wCs "
        );
    }

    // ---- T06 长文折行对齐 ----

    /// 取字符串在显示列区间 `[start, start+cols)` 内的内容（CJK 记 2 列）。
    fn slice_cols(s: &str, start: usize, cols: usize) -> String {
        let mut out = String::new();
        let mut col = 0usize;
        for c in s.chars() {
            let cw = char_width(c);
            if col >= start && col + cw <= start + cols {
                out.push(c);
            }
            col += cw;
        }
        out
    }

    /// 断言提示行与正文行逐词锁步对齐：整行列宽一致，且按提示单元列宽切分正文行时，
    /// 每一格都恰为「词 + 尾随空格」的定宽单元（提示码超词宽时由正文补空格让位）。
    fn assert_lockstep_aligned(rows: &[(Vec<HintCell>, String)]) {
        for (cells, b) in rows {
            let h = cells_text(cells);
            assert_eq!(
                display_width(&h),
                display_width(b),
                "row width mismatch hint={:?} body={:?}",
                h,
                b
            );
            let widths: Vec<usize> = cells.iter().map(|c| display_width(&c.text)).collect();
            let mut start = 0usize;
            for (i, &w) in widths.iter().enumerate() {
                let seg = slice_cols(b, start, w);
                assert_eq!(
                    display_width(&seg),
                    w,
                    "正文第 {i} 格应占 {w} 列，实得 {:?}（body={:?}）",
                    seg,
                    b
                );
                assert!(
                    !seg.starts_with(' '),
                    "正文第 {i} 格不应以空格开头：{seg:?}"
                );
                assert!(!seg.trim().is_empty(), "正文第 {i} 格不应为空：{seg:?}");
                start += w + 1; // 跳过词间单空格分隔
            }
        }
    }

    #[test]
    fn pack_words_by_width_groups_within_max() {
        // 词宽 [4,4,4]，max=10：前两词同处一行（4+1+4=9 ≤ 10），第三词另起一行。
        let widths = vec![4usize, 4, 4];
        let rows = pack_words_by_width(&widths, 10);
        assert_eq!(rows, vec![vec![0, 1], vec![2]]);
    }

    #[test]
    fn pack_words_by_width_oversized_word_own_row() {
        // 单文宽 14 远超 max=5：独占一行（避免空行/无限循环），由上层按原宽溢出渲染。
        let widths = vec![14usize, 2, 2];
        let rows = pack_words_by_width(&widths, 5);
        assert_eq!(rows, vec![vec![0], vec![1, 2]]);
    }

    #[test]
    fn layout_code_hint_grid_narrow_keeps_alignment() {
        // 窄宽度 + 长文：6 个双字词（各宽 4），max=9 → 每行 2 词、共 3 行；
        // 每行提示与正文折行点锁步、逐词对齐。
        let words = vec![
            "中国".to_string(),
            "发展".to_string(),
            "人民".to_string(),
            "社会".to_string(),
            "主义".to_string(),
            "制度".to_string(),
        ];
        let hints = vec![
            hint("zk"),
            hint("vzoi"),
            hint("wfaa"),
            hint("pwwi"),
            hint("uyit"),
            hint("sira"),
        ];
        let rows = layout_code_hint_grid(&words, &hints, &[], 9);
        assert_eq!(rows.len(), 3, "expected 3 wrapped rows, got {}", rows.len());
        assert_lockstep_aligned(&rows);
        // 首行应包含前两词（"中国"+"发展"），提示码按词宽居中/原样。
        assert_eq!(rows[0].1, "中国 发展");
        assert_eq!(cells_text(&rows[0].0), " zk  vzoi"); // "zk"居中宽4→" zk "；"vzoi"恰为宽4原样
    }

    #[test]
    fn layout_code_hint_grid_typed_word_blank_but_aligned() {
        // 已全对上屏的词提示留空（仍按词格宽占位），折行后仍与其下方字词对齐。
        let words = vec!["中".to_string(), "中国".to_string()];
        let hints = vec![hint("k"), hint("lgyinay")];
        let typed = vec![true, false];
        let rows = layout_code_hint_grid(&words, &hints, &typed, 10);
        assert_eq!(rows.len(), 1);
        assert_lockstep_aligned(&rows);
        // 「中国」格宽 7（码 lgyinay 宽于词）→ 正文补 3 空格让位。
        assert_eq!(rows[0].1, "中 中国   ");
        assert_eq!(cells_text(&rows[0].0), "   lgyinay"); // 「中」(2) 留空→2空格 + 词间1空格 = 3 前导空格
    }

    #[test]
    fn layout_code_hint_grid_oversized_word_still_aligned() {
        // 超长单字（宽 14）超过 max：独占一行，提示码按词格宽居中、仍与正文对齐。
        let words = vec!["中华人民共和国".to_string()];
        let hints = vec![hint("abc")];
        let rows = layout_code_hint_grid(&words, &hints, &[], 5);
        assert_eq!(rows.len(), 1);
        assert_lockstep_aligned(&rows);
        assert_eq!(rows[0].1, "中华人民共和国");
        assert_eq!(display_width(&cells_text(&rows[0].0)), 14);
    }

    #[test]
    fn layout_code_hint_grid_carries_hand_for_color() {
        // 简码单元携出手区归属，供渲染层左右手上色（左粉右黄）。
        let words = vec!["是".to_string(), "有".to_string(), "中".to_string()];
        let hints = vec![hint("_w"), hint("+e"), hint("wCs")];
        let rows = layout_code_hint_grid(&words, &hints, &[], 20);
        let cells = &rows[0].0;
        assert_eq!(cells[0].hand, HintHand::Left); // 是 → _w 左手
        assert_eq!(cells[1].hand, HintHand::Right); // 有 → +e 右手
        assert_eq!(cells[2].hand, HintHand::TwoHand); // 中 → wCs 双手并击无前缀 → TwoHand 单独配色
    }

    #[test]
    fn test_kongming_1hit_chords_shape_and_coverage() {
        assert_eq!(KONGMING_1HIT_CHORDS.len(), 204);
        let mut chars = Vec::new();
        for &(c, chord, hand) in KONGMING_1HIT_CHORDS {
            assert!(!chord.is_empty(), "指法不可为空: {c}");
            assert!(
                matches!(hand, HintHand::Left | HintHand::Right),
                "一击字指法必须为 Left 或 Right: {c}"
            );
            chars.push(c);
        }
        let orig_len = chars.len();
        chars.sort_unstable();
        chars.dedup();
        assert_eq!(chars.len(), orig_len, "一击字映射表不应有重复字符");

        // 验证代表性字条：
        // Tier 0
        assert_eq!(kongming_1hit_hint('中'), Some(("f", HintHand::Left)));
        assert_eq!(kongming_1hit_hint('的'), Some(("d", HintHand::Right)));
        // Tier 1
        assert_eq!(kongming_1hit_hint('好'), Some(("h", HintHand::Left)));
        assert_eq!(kongming_1hit_hint('一'), Some(("y", HintHand::Right)));
        // Tier 2
        assert_eq!(kongming_1hit_hint('性'), Some(("A", HintHand::Left)));
        assert_eq!(kongming_1hit_hint('定'), Some(("A", HintHand::Right)));
        // Tier 3
        assert_eq!(kongming_1hit_hint('等'), Some(("a␣", HintHand::Left)));
        assert_eq!(kongming_1hit_hint('由'), Some(("a␣", HintHand::Right)));
        assert_eq!(kongming_1hit_hint('还'), Some(("h␣", HintHand::Left)));
        assert_eq!(kongming_1hit_hint('很'), Some(("h␣", HintHand::Right)));
        assert_eq!(kongming_1hit_hint('教'), Some(("A␣", HintHand::Left)));
        assert_eq!(kongming_1hit_hint('代'), Some(("A␣", HintHand::Right)));

        // 未收录字符返回 None
        assert_eq!(kongming_1hit_hint('龙'), None);
    }

    #[test]
    fn test_kongming_1hit_words_shape_and_coverage() {
        assert_eq!(KONGMING_1HIT_WORDS.len(), 618);
        for &(w, chord) in KONGMING_1HIT_WORDS {
            assert!(!chord.is_empty(), "编码不可为空: {w}");
            assert_eq!(chord.len(), 2, "一击词编码长度必须为 2: {w} -> {chord}");
            assert!(w.chars().count() >= 2, "词条必须为多字词: {w}");
        }
        let mut words: Vec<&str> = KONGMING_1HIT_WORDS.iter().map(|&(w, _)| w).collect();
        let orig_len = words.len();
        words.sort_unstable();
        words.dedup();
        assert_eq!(words.len(), orig_len, "一击词映射表不应有重复词条");

        // 验证代表性词条
        assert_eq!(kongming_1hit_word_hint("可以"), Some(("k'", HintHand::TwoHand)));
        assert_eq!(kongming_1hit_word_hint("一个"), Some(("Uy", HintHand::TwoHand)));
        assert_eq!(kongming_1hit_word_hint("没有"), Some(("m'", HintHand::TwoHand)));
        assert_eq!(kongming_1hit_word_hint("我们"), Some(("Uw", HintHand::TwoHand)));
        assert_eq!(kongming_1hit_word_hint("这个"), Some(("Vg", HintHand::TwoHand)));
        assert_eq!(kongming_1hit_word_hint("农村"), Some(("NC", HintHand::TwoHand)));

        // 未收录词条返回 None
        assert_eq!(kongming_1hit_word_hint("奥林匹克"), None);
    }
}

