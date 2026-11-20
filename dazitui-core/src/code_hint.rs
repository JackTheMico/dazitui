//! 编码提示（遍码提示）渲染布局：将每词最优编码提示排版到正文行之上，逐词对齐。

use unicode_width::UnicodeWidthChar;

use crate::scheme::CodeHint;

/// 空格并击简词在提示区附加的空格键标记（U+2423 OPEN BOX），宽度 1、跨终端可读。
pub(crate) const SPACE_CHORD_MARK: char = '␣';

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

/// 编码前导 `_` / `+` / `-` 在提示区的解读方式。
///
/// 同一个字符在不同方案里语义相反，必须由方案层（`SchemeDict::hand_prefix_style`）判定：
///
/// - [`HandPrefixStyle::Modifier`]：**手区修饰符，不是按键**。yoyo-pure 类方案用
///   `xform|^(单手键序列)$|_$1|` 把单手码派生成 `_d` / `+e`，并在 `chord_composer.prompt_format`
///   里用 `xform/[-=_+]//g` 自行抹掉——即方案作者本就不把它当按键展示。提示区据此剥掉前缀。
/// - [`HandPrefixStyle::Literal`]：**字面码元**。空明码类纯并击方案里 `_` 由 chord algebra 的
///   `xform/(w-d-f-|j-k-o-)/_/`、`xform/(90-)/_/` 等规则直接生成，是必须打出的一击；
///   提示区若剥掉它，用户照码打字就会漏键（词提里「同事 = `_E`」只显示 `E` 即此问题）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HandPrefixStyle {
    /// 前缀是手区修饰符，提示区剥除（yoyo-pure 等派生式方案）。
    #[default]
    Modifier,
    /// 前缀是字面码元，提示区原样显示（空明码等纯并击方案）。
    Literal,
}

/// 提示区一个词单元的渲染单元：已按词格宽居中/留空的可视文本，与其手区归属（用于配色）。
/// 支持存储多手区分段文本（如多字词一击字词提「好人」包含左手 h 与右手 r 各自分段）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HintCell {
    pub text: String,
    pub hand: HintHand,
    pub segments: Vec<(String, HintHand)>,
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

/// 按方案语义推断手区归属（见 [`HandPrefixStyle`]）。
///
/// `Literal` 模式下前导 `_` / `+` / `-` 是码元而非手区标记（空明码的 `_` 同时代表
/// `w-d-f` 左手并击与 `j-k-o` 右手并击，本身无法区分左右），故一律按双手并击上色，
/// 与其它多码元提示（如 `ZCyl`）保持一致；否则沿用 [`hand_of_code`] 的前缀推断。
pub fn hand_of_code_with(code: &str, style: HandPrefixStyle) -> HintHand {
    match style {
        HandPrefixStyle::Modifier => hand_of_code(code),
        HandPrefixStyle::Literal => HintHand::TwoHand,
    }
}

/// 空明码「一击上屏」单字表（字符、编码提示文本、手区归属），与内置一击字赛文同源。
///
/// 由 `scripts/generate_kongming_1hit.py` 从方案词库生成。前 1500 常用字中一次并击即可上屏的字，
/// 按词频排序。单手码沿用简洁记法（`f=` → `f`、`a_=` → `a␣`），双手码保留方案逻辑码。
pub const KONGMING_1HIT_CHORDS: &[(char, &str, HintHand)] = &[
    ('的', "d", HintHand::Right),
    ('了', "l", HintHand::Right),
    ('我', "w", HintHand::Right),
    ('是', "s", HintHand::Right),
    ('在', "z", HintHand::Left),
    ('你', "n", HintHand::Right),
    ('和', "h", HintHand::Right),
    ('有', "i", HintHand::Right),
    ('不', "b", HintHand::Right),
    ('就', "j", HintHand::Right),
    ('都', "d␣", HintHand::Right),
    ('也', "y", HintHand::Left),
    ('要', "q", HintHand::Left),
    ('吧', "*c", HintHand::TwoHand),
    ('好', "h", HintHand::Left),
    ('他', "t", HintHand::Right),
    ('说', "S", HintHand::Left),
    ('去', "q", HintHand::Right),
    ('到', "e", HintHand::Right),
    ('没', "M", HintHand::Left),
    ('还', "h␣", HintHand::Left),
    ('上', "s", HintHand::Left),
    ('对', "D", HintHand::Left),
    ('那', "N", HintHand::Left),
    ('人', "r", HintHand::Right),
    ('个', "g", HintHand::Right),
    ('看', "k", HintHand::Left),
    ('中', "f", HintHand::Left),
    ('呢', "Q/", HintHand::TwoHand),
    ('来', "r", HintHand::Left),
    ('吗', "O`", HintHand::TwoHand),
    ('为', "w", HintHand::Left),
    ('会', "v", HintHand::Left),
    ('她', "T", HintHand::Right),
    ('与', "y␣", HintHand::Right),
    ('给', "I␣", HintHand::Right),
    ('等', "a␣", HintHand::Left),
    ('这', "v", HintHand::Right),
    ('用', "u", HintHand::Right),
    ('把', "B", HintHand::Right),
    ('下', "x", HintHand::Left),
    ('恩', "N?", HintHand::TwoHand),
    ('能', "n", HintHand::Left),
    ('很', "h␣", HintHand::Right),
    ('多', "o", HintHand::Right),
    ('做', "iZ", HintHand::TwoHand),
    ('想', "X␣", HintHand::Right),
    ('点', "P", HintHand::Left),
    ('被', "b␣", HintHand::Right),
    ('后', "H", HintHand::Left),
    ('地', "a", HintHand::Left),
    ('而', "e", HintHand::Left),
    ('一', "y", HintHand::Right),
    ('又', "Y␣", HintHand::Right),
    ('从', "C", HintHand::Right),
    ('再', "E.", HintHand::TwoHand),
    ('将', "J␣", HintHand::Left),
    ('时', "S", HintHand::Right),
    ('但', "K", HintHand::Right),
    ('么', "m", HintHand::Left),
    ('过', "o", HintHand::Left),
    ('打', "f,", HintHand::TwoHand),
    ('大', "d", HintHand::Left),
    ('年', "p", HintHand::Right),
    ('啦', "V,", HintHand::TwoHand),
    ('以', "i", HintHand::Left),
    ('买', "P`", HintHand::TwoHand),
    ('里', "l", HintHand::Left),
    ('让', "P?", HintHand::TwoHand),
    ('嗯', "h,", HintHand::TwoHand),
    ('着', "Z", HintHand::Right),
    ('小', "x␣", HintHand::Left),
    ('得', "D", HintHand::Right),
    ('先', "R␣", HintHand::Right),
    ('啊', "<W", HintHand::TwoHand),
    ('或', "H␣", HintHand::Left),
    ('并', "b␣", HintHand::Left),
    ('呀', "Q.", HintHand::TwoHand),
    ('找', "<d", HintHand::TwoHand),
    ('新', "e␣", HintHand::Left),
    ('及', "w!", HintHand::TwoHand),
    ('于', "u", HintHand::Left),
    ('跟', "<N", HintHand::TwoHand),
    ('太', "n:", HintHand::TwoHand),
    ('才', "<v", HintHand::TwoHand),
    ('号', "g~", HintHand::TwoHand),
    ('嘛', "M:", HintHand::TwoHand),
    ('发', "f", HintHand::Right),
    ('日', "r␣", HintHand::Left),
    ('可', "k", HintHand::Right),
    ('比', "B␣", HintHand::Right),
    ('玩', "f/", HintHand::TwoHand),
    ('们', "m", HintHand::Right),
    ('最', "e␣", HintHand::Right),
    ('吃', "r!", HintHand::TwoHand),
    ('月', "u␣", HintHand::Left),
    ('真', "uo", HintHand::TwoHand),
    ('它', "t␣", HintHand::Left),
    ('其', "Q", HintHand::Right),
    ('话', "H␣", HintHand::Right),
    ('出', "C", HintHand::Left),
    ('请', "!T", HintHand::TwoHand),
    ('钱', "N!", HintHand::TwoHand),
    ('叫', "<l", HintHand::TwoHand),
    ('开', "K", HintHand::Left),
    ('谁', "!X", HintHand::TwoHand),
    ('当', "D␣", HintHand::Left),
    ('哈', "uh", HintHand::TwoHand),
    ('家', "j", HintHand::Left),
    ('向', "n␣", HintHand::Right),
    ('问', "w␣", HintHand::Left),
    ('之', "z␣", HintHand::Right),
    ('别', "r;", HintHand::TwoHand),
    ('更', "r,", HintHand::TwoHand),
    ('该', "O/", HintHand::TwoHand),
    ('走', "t?", HintHand::TwoHand),
    ('快', "<i", HintHand::TwoHand),
    ('前', "q␣", HintHand::Left),
    ('由', "a␣", HintHand::Right),
    ('高', "g␣", HintHand::Right),
    ('您', "X!", HintHand::TwoHand),
    ('加', "uj", HintHand::TwoHand),
    ('所', "s␣", HintHand::Right),
    ('天', "t", HintHand::Left),
    ('却', "Q'", HintHand::TwoHand),
    ('已', "Y␣", HintHand::Left),
    ('内', "N", HintHand::Right),
    ('写', "z!", HintHand::TwoHand),
    ('啥', "<n", HintHand::TwoHand),
    ('老', "L␣", HintHand::Left),
    ('搞', "Q`", HintHand::TwoHand),
    ('行', "H", HintHand::Right),
    ('事', "S␣", HintHand::Left),
    ('拿', "<E", HintHand::TwoHand),
    ('带', "uD", HintHand::TwoHand),
    ('如', "R", HintHand::Right),
    ('只', "q␣", HintHand::Right),
    ('死', "J/", HintHand::TwoHand),
    ('卖', "o~", HintHand::TwoHand),
    ('晕', "<G", HintHand::TwoHand),
    ('像', "u~", HintHand::TwoHand),
    ('使', "V", HintHand::Right),
    ('刚', "!D", HintHand::TwoHand),
    ('成', "c", HintHand::Left),
    ('无', "W", HintHand::Left),
    ('换', "!e", HintHand::TwoHand),
    ('回', "v␣", HintHand::Left),
    ('哪', "E/", HintHand::TwoHand),
    ('爱', "<R", HintHand::TwoHand),
    ('听', ".J", HintHand::TwoHand),
    ('少', "V!", HintHand::TwoHand),
    ('元', "uv", HintHand::TwoHand),
    ('卡', "O!", HintHand::TwoHand),
    ('靠', "d:", HintHand::TwoHand),
    ('帮', "n.", HintHand::TwoHand),
    ('算', "<C", HintHand::TwoHand),
    ('者', "O", HintHand::Right),
    ('忙', "::", HintHand::TwoHand),
    ('起', "Q", HintHand::Left),
    ('各', "*v", HintHand::TwoHand),
    ('学', "X", HintHand::Left),
    ('系', "S;", HintHand::TwoHand),
    ('分', "F", HintHand::Right),
    ('咯', "g,", HintHand::TwoHand),
    ('总', "<K", HintHand::TwoHand),
    ('则', "l:", HintHand::TwoHand),
    ('水', "R␣", HintHand::Left),
    ('张', "<c", HintHand::TwoHand),
    ('改', "!<", HintHand::TwoHand),
    ('唉', "wa", HintHand::TwoHand),
    ('三', "O", HintHand::Left),
    ('万', "e`", HintHand::TwoHand),
    ('见', "u␣", HintHand::Right),
    ('连', "p`", HintHand::TwoHand),
    ('车', "x?", HintHand::TwoHand),
    ('放', "uF", HintHand::TwoHand),
    ('全', "Q␣", HintHand::Right),
    ('按', "J;", HintHand::TwoHand),
    ('机', "j␣", HintHand::Right),
    ('区', "<S", HintHand::TwoHand),
    ('早', "t!", HintHand::TwoHand),
    ('拉', "e/", HintHand::TwoHand),
    ('弄', "g.", HintHand::TwoHand),
    ('干', "<Z", HintHand::TwoHand),
    ('本', "B", HintHand::Left),
    ('偶', "!l", HintHand::TwoHand),
    ('页', "d?", HintHand::TwoHand),
    ('进', "J", HintHand::Right),
    ('同', "T", HintHand::Left),
    ('性', "A", HintHand::Left),
    ('第', "U", HintHand::Right),
    ('完', "!c", HintHand::TwoHand),
    ('花', "!i", HintHand::TwoHand),
    ('长', "c␣", HintHand::Left),
    ('怕', "<h", HintHand::TwoHand),
    ('女', "o/", HintHand::TwoHand),
    ('讲', "P.", HintHand::TwoHand),
    ('杀', "uS", HintHand::TwoHand),
    ('组', "e,", HintHand::TwoHand),
    ('错', "v<", HintHand::TwoHand),
    ('些', "x␣", HintHand::Right),
    ('住', "<f", HintHand::TwoHand),
    ('名', "U␣", HintHand::Left),
    ('跑', "B?", HintHand::TwoHand),
    ('网', "!J", HintHand::TwoHand),
    ('次', "N␣", HintHand::Right),
    ('睡', "<F", HintHand::TwoHand),
    ('装', "ur", HintHand::TwoHand),
    ('哦', "<M", HintHand::TwoHand),
    ('挺', "<t", HintHand::TwoHand),
    ('王', "t<", HintHand::TwoHand),
    ('图', ";J", HintHand::TwoHand),
    ('掉', "u<", HintHand::TwoHand),
    ('路', "u,", HintHand::TwoHand),
    ('正', "U", HintHand::Left),
    ('强', "j<", HintHand::TwoHand),
    ('外', "W", HintHand::Right),
    ('二', "E", HintHand::Left),
    ('头', "t␣", HintHand::Right),
    ('应', "i␣", HintHand::Right),
    ('生', "s␣", HintHand::Left),
    ('心', "X␣", HintHand::Left),
    ('因', "Y", HintHand::Right),
    ('子', "Z", HintHand::Left),
    ('类', ",J", HintHand::TwoHand),
    ('作', "Z␣", HintHand::Left),
    ('送', "!Z", HintHand::TwoHand),
    ('书', "v!", HintHand::TwoHand),
    ('每', "X/", HintHand::TwoHand),
    ('表', "B␣", HintHand::Left),
    ('法', "F", HintHand::Left),
    ('乖', "g`", HintHand::TwoHand),
    ('拍', "<m", HintHand::TwoHand),
    ('市', "o`", HintHand::TwoHand),
    ('撒', "e~", HintHand::TwoHand),
    ('至', "!d", HintHand::TwoHand),
    ('接', "Iu", HintHand::TwoHand),
    ('美', "M␣", HintHand::Left),
    ('线', "B.", HintHand::TwoHand),
    ('未', "w/", HintHand::TwoHand),
    ('级', "ui", HintHand::TwoHand),
    ('哎', "o?", HintHand::TwoHand),
    ('此', "c␣", HintHand::Right),
    ('版', "IB", HintHand::TwoHand),
    ('两', "l␣", HintHand::Left),
    ('股', "<X", HintHand::TwoHand),
    ('考', "uk", HintHand::TwoHand),
    ('手', "f␣", HintHand::Right),
    ('道', "d␣", HintHand::Left),
    ('座', "z'", HintHand::TwoHand),
    ('儿', "E", HintHand::Right),
    ('聊', "!v", HintHand::TwoHand),
    ('取', "q'", HintHand::TwoHand),
    ('台', "fv", HintHand::TwoHand),
    ('金', "0s", HintHand::TwoHand),
    ('定', "A", HintHand::Right),
    ('样', "Y", HintHand::Left),
    ('面', "M", HintHand::Right),
    ('门', "M␣", HintHand::Right),
    ('交', "g;", HintHand::TwoHand),
    ('李', "Il", HintHand::TwoHand),
    ('笑', "r.", HintHand::TwoHand),
    ('据', "0H", HintHand::TwoHand),
    ('群', "iQ", HintHand::TwoHand),
    ('自', "z␣", HintHand::Left),
    ('型', "u/", HintHand::TwoHand),
    ('慢', "p!", HintHand::TwoHand),
    ('刷', "/J", HintHand::TwoHand),
    ('位', "W␣", HintHand::Right),
    ('贵', "ug", HintHand::TwoHand),
    ('部', "b", HintHand::Left),
    ('称', "<k", HintHand::TwoHand),
    ('选', "0K", HintHand::TwoHand),
    ('牛', "iN", HintHand::TwoHand),
    ('怪', "<Q", HintHand::TwoHand),
    ('倒', "<D", HintHand::TwoHand),
    ('经', "J", HintHand::Left),
    ('数', "0o", HintHand::TwoHand),
    ('汗', "<B", HintHand::TwoHand),
    ('种', "Z␣", HintHand::Right),
    ('格', "<g", HintHand::TwoHand),
    ('方', "p", HintHand::Left),
    ('唔', "B;", HintHand::TwoHand),
    ('单', "Xi", HintHand::TwoHand),
    ('海', "U␣", HintHand::Right),
    ('相', "ux", HintHand::TwoHand),
    ('咋', "<e", HintHand::TwoHand),
    ('化', "8f", HintHand::TwoHand),
    ('班', "uB", HintHand::TwoHand),
    ('关', "g␣", HintHand::Left),
    ('涨', "<L", HintHand::TwoHand),
    ('店', "<u", HintHand::TwoHand),
    ('安', "Ga", HintHand::TwoHand),
    ('坐', "f.", HintHand::TwoHand),
    ('度', "O␣", HintHand::Right),
    ('歌', "ga", HintHand::TwoHand),
    ('报', "b:", HintHand::TwoHand),
    ('信', "Ja", HintHand::TwoHand),
    ('占', "h.", HintHand::TwoHand),
    ('原', "E␣", HintHand::Left),
    ('文', "W␣", HintHand::Left),
    ('量', "8W", HintHand::TwoHand),
    ('赚', "<r", HintHand::TwoHand),
    ('重', "C␣", HintHand::Left),
    ('主', "I", HintHand::Right),
    ('间', "J␣", HintHand::Right),
    ('贴', "!t", HintHand::TwoHand),
    ('入', "8t", HintHand::TwoHand),
    ('达', "ud", HintHand::TwoHand),
    ('国', "g", HintHand::Left),
    ('教', "A␣", HintHand::Left),
    ('离', "O.", HintHand::TwoHand),
    ('西', "u:", HintHand::TwoHand),
    ('明', "m␣", HintHand::Right),
    ('期', "EJ", HintHand::TwoHand),
    ('照', "Iz", HintHand::TwoHand),
    ('动', "D␣", HintHand::Right),
    ('特', "T␣", HintHand::Right),
    ('神', "<s", HintHand::TwoHand),
    ('力', "L", HintHand::Right),
    ('建', "qI", HintHand::TwoHand),
    ('东', "u;", HintHand::TwoHand),
    ('球', "IQ", HintHand::TwoHand),
    ('提', "K␣", HintHand::Left),
    ('除', "c'", HintHand::TwoHand),
    ('通', "V␣", HintHand::Right),
    ('饿', "eE", HintHand::TwoHand),
    ('病', "<b", HintHand::TwoHand),
    ('平', "F.", HintHand::TwoHand),
    ('急', "ue", HintHand::TwoHand),
    ('任', "r␣", HintHand::Right),
    ('理', "L", HintHand::Left),
    ('蓝', "iL", HintHand::TwoHand),
    ('常', "IC", HintHand::TwoHand),
    ('感', "9G", HintHand::TwoHand),
    ('南', "u!", HintHand::TwoHand),
    ('件', "0p", HintHand::TwoHand),
    ('破', "up", HintHand::TwoHand),
    ('声', "I␣", HintHand::Left),
    ('德', "uV", HintHand::TwoHand),
    ('雪', "IX", HintHand::TwoHand),
    ('整', "uZ", HintHand::TwoHand),
    ('呃', "eo", HintHand::TwoHand),
    ('现', "X", HintHand::Right),
    ('合', "G␣", HintHand::Right),
    ('求', "Vq", HintHand::TwoHand),
    ('电', "8N", HintHand::TwoHand),
    ('知', "I", HintHand::Left),
    ('何', "iE", HintHand::TwoHand),
    ('试', "s:", HintHand::TwoHand),
    ('仅', "ea", HintHand::TwoHand),
    ('北', "u?", HintHand::TwoHand),
    ('价', "jI", HintHand::TwoHand),
    ('情', "Q␣", HintHand::Left),
    ('需', "8j", HintHand::TwoHand),
    ('论', "0I", HintHand::TwoHand),
    ('石', "Is", HintHand::TwoHand),
    ('眼', "uY", HintHand::TwoHand),
    ('易', "IY", HintHand::TwoHand),
    ('借', "J.", HintHand::TwoHand),
    ('十', "P", HintHand::Right),
    ('停', "ut", HintHand::TwoHand),
    ('军', "j␣", HintHand::Left),
    ('身', "k␣", HintHand::Left),
    ('俩', "o.", HintHand::TwoHand),
    ('场', "0U", HintHand::TwoHand),
    ('制', "i␣", HintHand::Left),
    ('体', "T␣", HintHand::Left),
    ('物', "w␣", HintHand::Right),
    ('背', "iB", HintHand::TwoHand),
    ('剑', "J,", HintHand::TwoHand),
    ('解', "IJ", HintHand::TwoHand),
    ('证', "0L", HintHand::TwoHand),
    ('业', "V", HintHand::Left),
    ('代', "ID", HintHand::TwoHand),
    ('亦', "uy", HintHand::TwoHand),
    ('旧', ":J", HintHand::TwoHand),
    ('工', "G", HintHand::Right),
    ('准', "Z'", HintHand::TwoHand),
    ('轻', "Iq", HintHand::TwoHand),
    ('直', "uz", HintHand::TwoHand),
    ('公', "G", HintHand::Left),
    ('秀', "xI", HintHand::TwoHand),
    ('利', "l␣", HintHand::Right),
    ('立', "L␣", HintHand::Right),
    ('吾', "iW", HintHand::TwoHand),
    ('民', "m␣", HintHand::Left),
    ('佛', "iF", HintHand::TwoHand),
    ('品', "8a", HintHand::TwoHand),
    ('抽', "UC", HintHand::TwoHand),
    ('项', "xU", HintHand::TwoHand),
    ('故', "uG", HintHand::TwoHand),
    ('果', "G␣", HintHand::Left),
    ('保', "0y", HintHand::TwoHand),
    ('题', "t`", HintHand::TwoHand),
    ('竟', "Jo", HintHand::TwoHand),
    ('戏', "xE", HintHand::TwoHand),
    ('思', "uf", HintHand::TwoHand),
    ('秋', "uQ", HintHand::TwoHand),
    ('然', "R", HintHand::Left),
    ('引', "Iy", HintHand::TwoHand),
    ('员', "O␣", HintHand::Left),
    ('战', "k␣", HintHand::Right),
    ('斯', "p␣", HintHand::Left),
    ('必', "ub", HintHand::TwoHand),
    ('设', "0F", HintHand::TwoHand),
    ('几', "8U", HintHand::TwoHand),
    ('意', "y␣", HintHand::Left),
    ('基', "Ie", HintHand::TwoHand),
    ('迷', "um", HintHand::TwoHand),
    ('呵', "Ha", HintHand::TwoHand),
    ('觉', "0E", HintHand::TwoHand),
    ('惨', "uI", HintHand::TwoHand),
    ('汉', "uH", HintHand::TwoHand),
    ('展', "vZ", HintHand::TwoHand),
    ('世', "K␣", HintHand::Right),
    ('舞', "uw", HintHand::TwoHand),
    ('影', "0B", HintHand::TwoHand),
    ('箱', "Xv", HintHand::TwoHand),
    ('联', "ul", HintHand::TwoHand),
    ('程', "0S", HintHand::TwoHand),
    ('实', "S␣", HintHand::Right),
    ('角', "Ix", HintHand::TwoHand),
    ('厚', "vH", HintHand::TwoHand),
    ('运', "qU", HintHand::TwoHand),
    ('奶', "un", HintHand::TwoHand),
    ('术', "oA", HintHand::TwoHand),
    ('充', "iC", HintHand::TwoHand),
    ('尔', "iO", HintHand::TwoHand),
    ('专', "0v", HintHand::TwoHand),
    ('视', "uE", HintHand::TwoHand),
    ('产', "C␣", HintHand::Right),
    ('蔡', "nV", HintHand::TwoHand),
    ('计', "uJ", HintHand::TwoHand),
    ('潘', "vW", HintHand::TwoHand),
    ('届', "IZ", HintHand::TwoHand),
    ('认', "0j", HintHand::TwoHand),
    ('士', "us", HintHand::TwoHand),
    ('射', "sE", HintHand::TwoHand),
    ('聚', "iJ", HintHand::TwoHand),
    ('胃', "wv", HintHand::TwoHand),
    ('复', "8K", HintHand::TwoHand),
    ('嘉', "tU", HintHand::TwoHand),
    ('目', "0V", HintHand::TwoHand),
    ('敲', "oU", HintHand::TwoHand),
    ('怎', "0w", HintHand::TwoHand),
    ('嫁', "tV", HintHand::TwoHand),
    ('碗', "IW", HintHand::TwoHand),
    ('健', "jV", HintHand::TwoHand),
    ('失', "sa", HintHand::TwoHand),
    ('剂', "Ve", HintHand::TwoHand),
    ('羽', "uU", HintHand::TwoHand),
    ('袋', "Da", HintHand::TwoHand),
    ('浩', "uW", HintHand::TwoHand),
    ('遂', "lE", HintHand::TwoHand),
];

/// 查找指定字符在空明码一击字中的物理指法与手区归属。
pub fn kongming_1hit_hint(c: char) -> Option<(&'static str, HintHand)> {
    KONGMING_1HIT_CHORDS
        .iter()
        .find(|&&(ch, _, _)| ch == c)
        .map(|&(_, chord, hand)| (chord, hand))
}

/// 空明码「可单并击输入」词条表（词条、方案当前码面），与内置一击词赛文同源。
///
/// 由 `scripts/generate_kongming_1hit.py` 从方案词库生成。dazitui 自有 1500 常用词中，
/// 存在「一次并击可达且该词为词库首候选」的码的词，按词频排序。
pub const KONGMING_1HIT_WORDS: &[(&str, &str)] = &[
    ("可以", "ky"),
    ("一个", "e'"),
    ("自己", "0h"),
    ("没有", "mY"),
    ("我们", "0c"),
    ("这个", "s`"),
    ("问题", "Up"),
    ("中国", "Ig"),
    ("不错", "l~"),
    ("什么", "sM"),
    ("进行", "0n"),
    ("还是", "Ij"),
    ("使用", "sY"),
    ("数据", ":S"),
    ("需要", "UX"),
    ("公司", "C'"),
    ("学习", "Id"),
    ("但是", "/S"),
    ("都是", ",F"),
    ("如果", "0f"),
    ("如何", ":R"),
    ("时间", "0G"),
    ("喜欢", "x!"),
    ("因为", "0d"),
    ("技术", "jS"),
    ("时候", "Us"),
    ("他们", "Tm"),
    ("很多", "hD"),
    ("一些", "yx"),
    ("比较", "8E"),
    ("感觉", "8g"),
    ("不是", "Bs"),
    ("通过", "0k"),
    ("选择", ";X"),
    ("现在", "B!"),
    ("非常", ";f"),
    ("所以", "0a"),
    ("也是", "ys"),
    ("用户", "wn"),
    ("工作", "GZ"),
    ("服务", "Ul"),
    ("可能", "0m"),
    ("觉得", "u'"),
    ("以下", "gh"),
    ("推荐", "w'"),
    ("发展", "9d"),
    ("其他", "0A"),
    ("已经", "yj"),
    ("味道", "/w"),
    ("开始", "K'"),
    ("孩子", "e."),
    ("知道", "zD"),
    ("分析", "f`"),
    ("市场", "I'"),
    ("美国", "mG"),
    ("应该", "9y"),
    ("价格", "j?"),
    ("这些", "zx"),
    ("还有", "HY"),
    ("包括", ".B"),
    ("这样", "0i"),
    ("产品", ":C"),
    ("系统", "V/"),
    ("大家", "0q"),
    ("提供", "tG"),
    ("一般", "Ey"),
    ("不过", "BG"),
    ("内容", "8I"),
    ("游戏", "Vv"),
    ("信息", "Xc"),
    ("提高", "9t"),
    ("投资", "jB"),
    ("一下", "0r"),
    ("环境", ",H"),
    ("朋友", "p'"),
    ("生活", "H'"),
    ("语言", "Y'"),
    ("就是", "T~"),
    ("怎么", "Ui"),
    ("情况", "8q"),
    ("影响", ";a"),
    ("企业", "9q"),
    ("网络", ",W"),
    ("根据", "G'"),
    ("作为", "Zw"),
    ("东西", ".D"),
    ("真的", "zd"),
    ("设计", "s@"),
    ("发现", "Fx"),
    ("电影", "9e"),
    ("这种", "zZ"),
    ("建议", "W'"),
    ("不同", "BT"),
    ("个人", "9c"),
    ("或者", "Hz"),
    ("为什么", "gi"),
    ("帮助", "B@"),
    ("质量", "z`"),
    ("而且", "8e"),
    ("那么", "0e"),
    ("关注", ";r"),
    ("方式", "f_="),
    ("活动", ":H"),
    ("对于", "DY"),
    ("管理", "ol"),
    ("主要", "0C"),
    ("看到", "KD"),
    ("能力", "0D"),
    ("教育", "Ej"),
    ("了解", "9C"),
    ("文化", ",w"),
    ("世界", ",s"),
    ("其实", "qs"),
    ("这里", "0O"),
    ("健康", "jK"),
    ("一直", ",y"),
    ("国家", "8R"),
    ("同时", "Ts"),
    ("然后", "RH"),
    ("不能", "Bn"),
    ("这是", "zs"),
    ("地方", "dF"),
    ("不要", "9T"),
    ("基金", "h'"),
    ("成为", "cw"),
    ("方面", "7w"),
    ("结果", "0J"),
    ("训练", "Xl"),
    ("专业", "Z*"),
    ("适合", "sh"),
    ("最后", "ZH"),
    ("能够", "7n"),
    ("城市", "cs"),
    ("安全", "A'"),
    ("目前", "uM"),
    ("经济", "Uu"),
    ("之后", "zH"),
    ("出现", "9S"),
    ("不会", "BH"),
    ("一样", "yY"),
    ("历史", "7l"),
    ("希望", "Xo"),
    ("学生", "0T"),
    ("以及", "0l"),
    ("例如", "lR"),
    ("手机", ";S"),
    ("一起", "yq"),
    ("虽然", "7S"),
    ("认为", "9r"),
    ("社会", "oh"),
    ("相关", "xG"),
    ("是否", "Xw"),
    ("研究", "oY"),
    ("平台", "x:"),
    ("注意", "!b"),
    ("出来", "X'"),
    ("品牌", "oO"),
    ("实现", "sx"),
    ("一定", "m'"),
    ("大学", "kf"),
    ("支持", "!z"),
    ("关系", "oG"),
    ("其中", "qZ"),
    ("无法", "WF"),
    ("里面", "0R"),
    ("学校", "?X"),
    ("所有", "SY"),
    ("项目", "xM"),
    ("只有", "0u"),
    ("只是", "0z"),
    ("领域", "UK"),
    ("要求", "q?"),
    ("好吃", "/H"),
    ("参考", ";C"),
    ("的话", "0g"),
    ("北京", "b'"),
    ("回答", "Vx"),
    ("文章", "wZ"),
    ("为了", "wl"),
    ("起来", "7q"),
    ("之前", "Vw"),
    ("目标", ":M"),
    ("效果", ";b"),
    ("来说", "0t"),
    ("这么", "zM"),
    ("直接", "8d"),
    ("各种", "7G"),
    ("今天", ":j"),
    ("理解", "9N"),
    ("特别", "rz"),
    ("计划", "k;"),
    ("老师", "L'"),
    ("控制", "Kz"),
    ("更加", "gj"),
    ("风险", "VC"),
    ("两个", "0W"),
    ("评价", "ih"),
    ("网站", ".W"),
    ("一点", "yd"),
    ("分类", "fl"),
    ("旅游", ";L"),
    ("增加", "U`"),
    ("故事", "ot"),
    ("功能", "xD"),
    ("解决", "j'"),
    ("比如", "bR"),
    ("以上", "0P"),
    ("值得", "8z"),
    ("需求", "Xq"),
    ("音乐", "oM"),
    ("客户", "oS"),
    ("因此", "yc"),
    ("金融", "jR"),
    ("基本", "UJ"),
    ("导致", "xX"),
    ("考虑", "9K"),
    ("那个", "0Q"),
    ("发生", "Fs"),
    ("以后", "0X"),
    ("知识", "Ez"),
    ("当时", "0N"),
    ("运动", "n'"),
    ("有些", "9D"),
    ("国际", "UG"),
    ("地区", "7d"),
    ("交易", "k'"),
    ("身体", ";s"),
    ("关于", "GY"),
    ("每天", "y~"),
    ("过程", "Uq"),
    ("避免", "bm"),
    ("你们", "nm"),
    ("存在", "8C"),
    ("治疗", "f'"),
    ("之一", "zy"),
    ("后来", "HL"),
    ("几个", "U~"),
    ("作品", "Z`"),
    ("由于", "YY"),
    ("购买", "GM"),
    ("部分", "9z"),
    ("当然", "DR"),
    ("具体", "Jt"),
    ("速度", "S'"),
    ("政府", "oZ"),
    ("微信", ";w"),
    ("事情", "7s"),
    ("人员", "rY"),
    ("之间", "?z"),
    ("继续", "9j"),
    ("容易", "!R"),
    ("自动", ";z"),
    ("最近", "om"),
    ("社区", "/s"),
    ("任何", "!r"),
    ("不断", "9f"),
    ("甚至", ";y"),
    ("完全", "/W"),
    ("准备", "Z~"),
    ("具有", "7J"),
    ("成功", ",S"),
    ("阅读", "ks"),
    ("别人", "7b"),
    ("全球", "Qq"),
    ("比赛", "bS"),
    ("家庭", ".t"),
    ("完成", ";W"),
    ("经常", "jC"),
    ("视频", "xW"),
    ("合作", "hZ"),
    ("科技", "kj"),
    ("人类", "r`"),
    ("有的", "/q"),
    ("香港", "!y"),
    ("发布", "kd"),
    ("分享", "f~"),
    ("中心", "Zx"),
    ("建立", "j@"),
    ("情感", "qG"),
    ("除了", "Vy"),
    ("确定", ";q"),
    ("任务", "rW"),
    ("水平", "7I"),
    ("交通", "jT"),
    ("得到", "dD"),
    ("很好", "hH"),
    ("变化", "9b"),
    ("资料", "gz"),
    ("软件", "uR"),
    ("图像", "Tx"),
    ("参加", ",C"),
    ("表现", "M`"),
    ("政策", "Xe"),
    ("英语", "uc"),
    ("国内", "9n"),
    ("机构", "rb"),
    ("测试", "7c"),
    ("基础", "I`"),
    ("组织", "Eo"),
    ("并不", "bB"),
    ("台湾", "TW"),
    ("资源", "oe"),
    ("人们", "q~"),
    ("价值", "?Y"),
    ("首先", ".S"),
    ("单位", "8D"),
    ("业务", "yW"),
    ("找到", ";Z"),
    ("另外", "7X"),
    ("设备", "sb"),
    ("产生", "7C"),
    ("操作", ";g"),
    ("必须", "Eb"),
    ("同学", "TX"),
    ("怎样", "VY"),
    ("不知道", "UT"),
    ("经验", ";j"),
    ("他的", "Td"),
    ("每个", "oB"),
    ("许多", ":X"),
    ("计算", "qK"),
    ("每次", "mc"),
    ("建设", "ov"),
    ("拥有", "oC"),
    ("是不是", "@s"),
    ("今年", "EB"),
    ("标准", "oQ"),
    ("结构", "rl"),
    ("只能", "zn"),
    ("属于", "Ub"),
    ("机会", "B~"),
    ("考试", ";K"),
    ("达到", "7v"),
    ("答案", "iD"),
    ("决定", "J'"),
    ("接受", "uX"),
    ("那些", "Nx"),
    ("生产", "oF"),
    ("并且", ":b"),
    ("微博", "wB"),
    ("空间", "uK"),
    ("第一次", "@r"),
    ("工具", "m`"),
    ("销售", "xS"),
    ("最大", "ZD"),
    ("口味", "Kw"),
    ("看看", "?s"),
    ("多少", "DS"),
    ("代表", "t~"),
    ("医生", "NM"),
    ("免费", "m:"),
    ("政治", "Uv"),
    ("广告", "/G"),
    ("位置", "8w"),
    ("分钟", "E~"),
    ("可是", "uT"),
    ("效率", "o'"),
    ("造成", "EZ"),
    ("谢谢", "?p"),
    ("艺术", "y<"),
    ("商品", "S`"),
    ("照片", "Zp"),
    ("科学", "Ek"),
    ("成本", "cb"),
    ("越来越", "@Y"),
    ("努力", "or"),
    ("妈妈", "?M"),
    ("超过", "wt"),
    ("作用", "l*"),
    ("风格", "fg"),
    ("时代", "J`"),
    ("最终", "gr"),
    ("经历", "vS"),
    ("改变", "8v"),
    ("制作", ":G"),
    ("带来", "UD"),
    ("趋势", ";Q"),
    ("只要", "8Y"),
    ("实际", ":s"),
    ("好的", "Hd"),
    ("检查", "B'"),
    ("系列", "9l"),
    ("大量", "oj"),
    ("作者", "K*"),
    ("确实", "!Q"),
    ("宝宝", "!B"),
    ("哈哈", "Vz"),
    ("不少", "BS"),
    ("提升", "VZ"),
    ("等等", "/d"),
    ("团队", "ET"),
    ("告诉", "iG"),
    ("更新", "VJ"),
    ("整个", "z~"),
    ("状态", "/Z"),
    ("正常", "Ux"),
    ("按照", "B:"),
    ("条件", "t'"),
    ("收入", "SR"),
    ("方案", "F'"),
    ("好像", "!H"),
    ("来自", "Lz"),
    ("下面", "xm"),
    ("患者", "hy"),
    ("持续", "cX"),
    ("描述", "mS"),
    ("场景", "m_"),
    ("从而", "Ce"),
    ("是什么", "<z"),
    ("感受", "GS"),
    ("规定", "Qu"),
    ("方向", "Uw"),
    ("先生", "oX"),
    ("现场", "xC"),
    ("附近", "Fj"),
    ("建筑", "jZ"),
    ("法国", "FG"),
    ("还可以", "@Q"),
    ("规划", "GH"),
    ("网上", "WS"),
    ("明显", "Em"),
    ("兴趣", "xQ"),
    ("对方", "DF"),
    ("普通", "P'"),
    ("尤其是", ":Y"),
    ("法律", "8F"),
    ("不用", "Pj"),
    ("女人", "EN"),
    ("真正", "zz"),
    ("受到", "?b"),
    ("精神", "oJ"),
    ("随着", "od"),
    ("于是", "Ys"),
    ("经过", "n_"),
    ("吃饭", ";c"),
    ("时期", "iR"),
    ("保证", "Xs"),
    ("记录", "jL"),
    ("消费者", "@x"),
    ("女生", "Ns"),
    ("人生", "7m"),
    ("面积", "mj"),
    ("特点", "td"),
    ("形成", "9x"),
    ("意义", "7U"),
    ("程序", ":c"),
    ("集团", "g*"),
    ("而是", "es"),
    ("计算机", "@u"),
    ("范围", "9F"),
    ("电视", "k."),
    ("相对", "EL"),
    ("本人", "br"),
    ("男人", "N~"),
    ("相信", "!G"),
    ("决策", "Jc"),
    ("自由", ":z"),
    ("满足", "/M"),
    ("肯定", "7k"),
    ("老板", "8Z"),
    ("程度", "q`"),
    ("符合", "Fh"),
    ("认识", "7r"),
    ("德国", "dG"),
    ("大概", "oT"),
    ("第二", "de"),
    ("几乎", "V."),
    ("然而", "Re"),
    ("到底", "Dd"),
    ("新的", "Iw"),
    ("满意", ";M"),
    ("有没有", "0Y"),
    ("呵呵", "Vs"),
    ("苹果", "Ep"),
    ("都有", "8S"),
    ("下午", "Ia"),
    ("阶段", "Rm"),
    ("全部", "QB"),
    ("这次", "y?"),
    ("课程", "kc"),
    ("调查", "vY"),
    ("设置", "sz"),
    ("欧洲", "OZ"),
    ("一切", "y!"),
    ("咨询", "zX"),
    ("核心", "hx"),
    ("是一种", ".Y"),
    ("不仅", "9B"),
    ("不同的", ";d"),
    ("背景", "bj"),
    ("电子", "dz"),
    ("数字", "Sz"),
    ("各位", "gw"),
    ("至少", "kn"),
    ("办法", "x~"),
    ("突然", "TR"),
    ("培训", "pX"),
    ("绝对", "JD"),
    ("整体", "V?"),
    ("并不是", "@b"),
    ("曾经", "Ec"),
    ("目的", "M'"),
    ("生命", "!s"),
    ("结合", "go"),
    ("个月", "gY"),
    ("也有", "9R"),
    ("判断", "/P"),
    ("负责", "?F"),
    ("儿子", "ez"),
    ("有什么", "Y@"),
    ("制度", "Oz"),
    ("同样", "TY"),
    ("输入", "LR"),
    ("打开", ":D"),
    ("支付", "zF"),
    ("合理", "hl"),
    ("引起", "en"),
    ("如下", "Rx"),
    ("爱情", "7a"),
    ("合适", "h`"),
    ("能源", "nY"),
    ("根本", "Eg"),
    ("来看", "LK"),
    ("多个", "Dg"),
    ("想要", "Va"),
    ("意见", "x@"),
    ("粉丝", "fs"),
    ("搜索", "!S"),
    ("玩家", "Z@"),
    ("改革", "/g"),
    ("责任", "zr"),
    ("正确", "qT"),
    ("文字", "wz"),
    ("皮肤", "pF"),
    ("搭配", "Dp"),
    ("形式", "X~"),
    ("组合", "Zh"),
    ("情绪", "qX"),
    ("让人", "Rr"),
    ("相当", "d'"),
    ("进一步", "@j"),
    ("练习", ";l"),
    ("平均", "8p"),
    ("毕竟", "AI"),
    ("运营", "G~"),
    ("力量", "El"),
    ("去年", "Qn"),
    ("距离", "9X"),
    ("安排", "AP"),
    ("变成", "bc"),
    ("宣传", "XC"),
    ("状况", "9Z"),
    ("证明", "G@"),
    ("才是", "8A"),
    ("回来", "0x"),
    ("差不多", "@C"),
    ("声音", "rh"),
    ("我们的", "9i"),
    ("地址", "Ed"),
    ("取得", "Qd"),
    ("区域", "Qy"),
    ("面对", "mD"),
    ("领导", "l@"),
    ("挑战", "tZ"),
    ("平时", "7p"),
    ("清楚", "qC"),
    ("我觉得", "ri"),
    ("大部分", "@D"),
    ("排名", "oP"),
    ("安装", "7L"),
    ("女儿", "Ne"),
    ("配置", "pz"),
    ("感到", "oc"),
    ("过来", "GL"),
    ("区别", "Qb"),
    ("本来", "bL"),
    ("动作", "d~"),
    ("父亲", "fa"),
    ("回家", "B*"),
    ("幸福", ":x"),
    ("唯一", "ow"),
    ("理论", ".l"),
    ("拍摄", "Ps"),
    ("咖啡", "Kf"),
    ("培养", "pY"),
    ("优秀", "nL"),
    ("也就是", "@Z"),
    ("过程中", "v~"),
    ("获取", "HQ"),
    ("感情", "7g"),
    ("想法", "xF"),
    ("下载", "xZ"),
    ("统计", "Tj"),
    ("内部", "nB"),
    ("细节", "/x"),
    ("都会", "km"),
    ("公布", "GB"),
    ("不足", "BZ"),
    ("有着", "YZ"),
    ("别的", "bd"),
    ("女孩", "N_"),
    ("居然", "JR"),
    ("掌握", "ZW"),
    ("参数", "/C"),
    ("举行", "Jx"),
    ("添加", "tJ"),
    ("相比", "hh"),
    ("我国", "WG"),
    ("似乎", "9s"),
    ("心里", ".Z"),
    ("年代", "nD"),
    ("让我", "RW"),
    ("学会", "XH"),
    ("有个", "C~"),
    ("机制", "Og"),
    ("即使", "j~"),
    ("利润", ",r"),
    ("思想", "8s"),
    ("尽量", "j:"),
    ("期待", ":q"),
    ("思维", "sw"),
    ("人家", "rj"),
    ("担任", "Dr"),
    ("爸爸", "?B"),
    ("分为", "fw"),
    ("反馈", "FK"),
    ("离开", "lK"),
    ("永远", "7Y"),
    ("也许", "i'"),
    ("也可以", "@h"),
    ("很少", "hS"),
    ("句子", "x<"),
    ("开心", "Kx"),
    ("寻找", "XZ"),
    ("家人", "jr"),
    ("话题", "Ht"),
    ("连续", "lX"),
    ("流程", "l'"),
    ("慢慢", "?q"),
    ("可能会", "/T"),
    ("一定要", "@M"),
    ("出去", "CQ"),
    ("好好", "?G"),
    ("所谓", "8r"),
    ("卫生", "ws"),
    ("怎么样", "@o"),
    ("居民", "Jm"),
    ("后面", "Hm"),
    ("仍然", "Er"),
    ("认真", ".r"),
    ("医学", "yX"),
    ("意识", "Ee"),
    ("前往", "qW"),
    ("定位", "dw"),
    ("铁路", "tL"),
    ("球队", "qD"),
    ("资本", "zb"),
    ("两种", "lZ"),
    ("反正", ":F"),
    ("轻松", "qS"),
    ("手术", "SS"),
    ("实践", "s~"),
    ("往往", "7W"),
    ("它的", "Ud"),
    ("一个人", "y`"),
    ("客服", "kF"),
    ("有时候", "@i"),
    ("文学", "wX"),
    ("大众", ",B"),
    ("渠道", "QD"),
    ("眼睛", "oo"),
    ("物质", "9W"),
    ("放弃", "Fq"),
    ("语法", "q!"),
    ("也会", "8h"),
    ("监督", "oz"),
    ("投入", ",T"),
    ("不管", "tC"),
    ("机场", "wy"),
    ("基本上", ";B"),
    ("休息", "ox"),
    ("心情", ";x"),
    ("病毒", "bD"),
    ("明白", "8m"),
    ("至于", "oq"),
    ("飞机", "N`"),
    ("实验", "9v"),
    ("完善", "EW"),
    ("给我", "gW"),
    ("总结", "dN"),
    ("温度", "wD"),
    ("物品", "Wp"),
    ("例子", "l`"),
    ("有了", "V;"),
    ("刚刚", "8G"),
    ("球员", "F~"),
    ("充满", "C@"),
    ("日常", "/r"),
    ("各个", "gg"),
    ("验证", "Yz"),
    ("人体", "rt"),
    ("统一", "7T"),
    ("强调", "qd"),
    ("具备", "8J"),
    ("污染", "WR"),
    ("困难", "KN"),
    ("热情", "rE"),
    ("深入", "sR"),
    ("融资", "Rz"),
    ("实力", "sl"),
    ("论文", "Lw"),
    ("品种", "pZ"),
    ("确认", "Qr"),
    ("有趣", "YQ"),
    ("本科", "bk"),
    ("分配", "fp"),
    ("有时", "H*"),
    ("植物", "zW"),
    ("回到", "HD"),
    ("失败", "sB"),
    ("拒绝", "9J"),
    ("太空", "TK"),
    ("表演", "bY"),
    ("农业", "7N"),
    ("难以", "Ny"),
    ("强大", "q:"),
    ("第三", "dS"),
    ("让你", "Rn"),
    ("如今", "/R"),
    ("一部分", "@y"),
    ("我想", "Wx"),
    ("可爱", "ka"),
    ("访问", "IG"),
    ("就可以", "@e"),
    ("更多", ",g"),
    ("群体", "Qt"),
    ("文明", "wm"),
    ("老人", "Lr"),
    ("学科", "Xk"),
    ("突破", "TP"),
    ("特殊", ":t"),
    ("才能", "8n"),
    ("浪费", "Lf"),
    ("指出", ".y"),
    ("听说", "tS"),
    ("出版", "CB"),
    ("心态", "xT"),
    ("编程", "/b"),
    ("上午", "oa"),
    ("考生", "Ks"),
    ("不敢", "wg"),
    ("美好", "mH"),
    ("专门", "Zm"),
    ("梦想", ":m"),
    ("维持", "wc"),
    ("可惜", "9k"),
    ("来到", "LD"),
    ("头发", "TF"),
    ("持有", "cY"),
    ("千万", ",q"),
    ("农村", "NC"),
    ("迅速", "XS"),
    ("原文", "Yw"),
    ("内心", "nx"),
    ("有一个", "@g"),
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

/// 提示单元的实际显示文本：按方案语义处理手区前缀，空格并击简词追加空格键标记。
///
/// 上屏/选重键由 `SchemeDict` 直接写进 `code`（`␣` / `;` / `'` / 位次数字，ADR 0014 D2），
/// 渲染层原样呈现，保证「提示串 = 要按的键序列」。
fn hint_display_text(code: &str, style: HandPrefixStyle) -> String {
    let mut s = strip_hand_prefix(code, style).to_string();
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
    hint_cell_widths_with(words, hints, HandPrefixStyle::default())
}

/// [`hint_cell_widths`] 的方案语义版本：`style` 决定编码前导 `_` / `+` / `-` 是否计入列宽。
///
/// 空明码的 `_` 是码元（`HandPrefixStyle::Literal`），必须占一列，否则提示会被压窄、
/// 或与正文列错位；yoyo-pure 的 `_` 是修饰符（`Modifier`），不占列。
pub fn hint_cell_widths_with(
    words: &[String],
    hints: &[CodeHint],
    style: HandPrefixStyle,
) -> Vec<usize> {
    words
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let word_w = display_width(w);
            let code_w = hints
                .get(i)
                .map(|h| display_width(&hint_display_text(&h.code, style)))
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
///
/// `style` 为 [`HandPrefixStyle::Literal`] 时不做剥除：此类方案里 `_` / `+` / `-` 本身就是
/// 要打的码元（如空明码 `_E`），剥掉即等于丢掉一击。`%` 是渲染层标记，始终剥除。
fn strip_hand_prefix(code: &str, style: HandPrefixStyle) -> &str {
    let c = code.strip_prefix('%').unwrap_or(code);
    match style {
        HandPrefixStyle::Modifier => c.strip_prefix(['_', '+', '-']).unwrap_or(c),
        HandPrefixStyle::Literal => c,
    }
}

/// 内置赛文对照区双行词格：生成本页「提示行」的逐词渲染单元。
///
/// `words` 为本页正文词（与 `hints` 同序），每个提示按对应词格宽（`max(词宽, 码宽)`）居中，
/// 词间以单空格分隔，使提示行与正文行（同样以单空格分词、并补齐到同一词格宽）逐词对齐。提示码会先去掉
/// 手区修饰符前缀（如单手简码 `_b` → `b`），仅显示实际按键；其手区归属（`HintHand`）
/// 一并返回，交由渲染层据左右手上色（左手粉、右手黄）。前缀是否为修饰符由 `style`
/// （[`HandPrefixStyle`]）决定：字面码元（空明码 `_E`）原样保留。
///
/// `typed_mask[i]` 为真表示该词已全部正确上屏，其提示留空（仍按词格宽占位，不影响对齐）。
pub fn layout_code_hint_line(
    words: &[String],
    hints: &[CodeHint],
    typed_mask: &[bool],
) -> Vec<HintCell> {
    layout_code_hint_line_with(words, hints, typed_mask, HandPrefixStyle::default())
}

/// [`layout_code_hint_line`] 的方案语义版本（`style` 见 [`HandPrefixStyle`]）。
pub fn layout_code_hint_line_with(
    words: &[String],
    hints: &[CodeHint],
    typed_mask: &[bool],
    style: HandPrefixStyle,
) -> Vec<HintCell> {
    let widths = hint_cell_widths_with(words, hints, style);
    words
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let target = widths.get(i).copied().unwrap_or_else(|| display_width(w));
            let typed = typed_mask.get(i).copied().unwrap_or(false);
            if typed {
                let text = " ".repeat(target);
                return HintCell {
                    text: text.clone(),
                    hand: HintHand::None,
                    segments: vec![(text, HintHand::None)],
                };
            }
            match hints.get(i) {
                Some(h) if !h.is_oov && !h.code.is_empty() => {
                    // 检查是否为一击字词提（如多字词全由一击字组成或单字一击字）
                    let chars: Vec<char> = w.chars().collect();
                    let hit_info: Option<Vec<(&'static str, HintHand)>> = if !chars.is_empty() {
                        chars.iter().map(|&c| kongming_1hit_hint(c)).collect()
                    } else {
                        None
                    };
                    if let Some(hits) = hit_info {
                        let expected = hits.iter().map(|(c, _)| *c).collect::<Vec<_>>().join(" ");
                        let clean_code = h.code.strip_prefix('%').unwrap_or(&h.code);
                        let clean_code = clean_code.strip_suffix('=').unwrap_or(clean_code);
                        if h.code == expected || clean_code == expected {
                            let code_w = display_width(&expected);
                            let cell_text = format_hint_cell(&expected, target);
                            let overall_hand = if hits.iter().all(|(_, hd)| *hd == hits[0].1) {
                                hits[0].1
                            } else {
                                HintHand::TwoHand
                            };
                            let segs = if target > code_w {
                                let pad = target - code_w;
                                let left_pad = pad / 2;
                                let right_pad = pad - left_pad;
                                let mut segs = Vec::new();
                                if left_pad > 0 {
                                    segs.push((" ".repeat(left_pad), HintHand::None));
                                }
                                for (idx, (chord, hand)) in hits.iter().enumerate() {
                                    if idx > 0 {
                                        segs.push((" ".to_string(), HintHand::None));
                                    }
                                    segs.push((chord.to_string(), *hand));
                                }
                                if right_pad > 0 {
                                    segs.push((" ".repeat(right_pad), HintHand::None));
                                }
                                segs
                            } else if target == code_w {
                                let mut segs = Vec::new();
                                for (idx, (chord, hand)) in hits.iter().enumerate() {
                                    if idx > 0 {
                                        segs.push((" ".to_string(), HintHand::None));
                                    }
                                    segs.push((chord.to_string(), *hand));
                                }
                                segs
                            } else {
                                vec![(cell_text.clone(), overall_hand)]
                            };
                            return HintCell {
                                text: cell_text,
                                hand: overall_hand,
                                segments: segs,
                            };
                        }
                    }

                    let hand = hand_of_code_with(&h.code, style);
                    let code = hint_display_text(&h.code, style);
                    let code_w = display_width(&code);
                    let cell_text = format_hint_cell(&code, target);
                    let segs = if target > code_w {
                        let pad = target - code_w;
                        let left_pad = pad / 2;
                        let right_pad = pad - left_pad;
                        let mut segs = Vec::new();
                        if left_pad > 0 {
                            segs.push((" ".repeat(left_pad), HintHand::None));
                        }
                        segs.push((code.clone(), hand));
                        if right_pad > 0 {
                            segs.push((" ".repeat(right_pad), HintHand::None));
                        }
                        segs
                    } else if target == code_w {
                        vec![(code.clone(), hand)]
                    } else {
                        vec![(cell_text.clone(), hand)]
                    };
                    HintCell {
                        text: cell_text,
                        hand,
                        segments: segs,
                    }
                }
                _ => {
                    let text = " ".repeat(target);
                    HintCell {
                        text: text.clone(),
                        hand: HintHand::None,
                        segments: vec![(text, HintHand::None)],
                    }
                }
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
    layout_code_hint_grid_with(
        words,
        hints,
        typed_mask,
        max_width,
        HandPrefixStyle::default(),
    )
}

/// [`layout_code_hint_grid`] 的方案语义版本（`style` 见 [`HandPrefixStyle`]）。
///
/// `style` 同时影响词格列宽：字面 `_` 占一列，修饰符 `_` 不占列，故宽度与提示文本
/// 必须用同一 `style` 计算，否则提示行与正文行会错位。
pub fn layout_code_hint_grid_with(
    words: &[String],
    hints: &[CodeHint],
    typed_mask: &[bool],
    max_width: usize,
    style: HandPrefixStyle,
) -> Vec<(Vec<HintCell>, String)> {
    let widths: Vec<usize> = hint_cell_widths_with(words, hints, style);
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
            let hint_cells = layout_code_hint_line_with(&row_words, &row_hints, &row_typed, style);
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
    fn layout_literal_hand_prefix_keeps_underscore_symbol() {
        // 空明码（`HandPrefixStyle::Literal`）：`_` 由 chord algebra 直接产出
        // （`xform/(w-d-f-|j-k-o-)/_/`、`xform/(90-)/_/`），是必须打出的一击。
        // 旧行为把它当手区修饰符剥掉，词「同事」的提示只剩 `E`，用户照码打字即漏键
        // （即「词提里的下划线根本看不见」）。
        let words = vec!["同事".to_string()];
        let hints = vec![hint("_E")];
        let cells = layout_code_hint_line_with(&words, &hints, &[], HandPrefixStyle::Literal);
        assert_eq!(cells_text(&cells), " _E ");
        // `_` 同时代表左手 wdf 与右手 jko 并击，无法据以判手 → 与其它多码元提示一致按双手上色。
        assert_eq!(cells[0].hand, HintHand::TwoHand);

        // 列宽必须把 `_` 计入：词宽 2 的「中」，码 `_W7` 在 Literal 下需 3 列。
        let words3 = vec!["中".to_string()];
        let hints3 = vec![hint("_W7")];
        assert_eq!(
            hint_cell_widths_with(&words3, &hints3, HandPrefixStyle::Modifier),
            vec![2],
            "Modifier 语义下 `_` 是修饰符，不占列"
        );
        assert_eq!(
            hint_cell_widths_with(&words3, &hints3, HandPrefixStyle::Literal),
            vec![3],
            "Literal 语义下 `_` 是码元，必须占一列"
        );

        // 默认（Modifier）语义不受影响：yoyo-pure 的 `_d` 仍只显示 `d`。
        let cells_m = layout_code_hint_line(&words, &hints, &[]);
        assert_eq!(cells_m[0].text.trim(), "E");
        assert_eq!(cells_m[0].hand, HintHand::Left);
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
    fn test_kongming_multi_char_hint_cell_colors_left_and_right() {
        // 空明码一击字词提：好人（好=左手 h，人=右手 r）应拆解为左手分段与右手分段，中间空格与补白为 None。
        let words = vec!["好人".to_string()];
        let hints = vec![hint("h r")];
        let cells = layout_code_hint_line(&words, &hints, &[]);
        assert_eq!(cells.len(), 1);
        let cell = &cells[0];
        // 词宽为 4，编码宽为 3（h r），居中填充 1 格在右侧 -> "h r "
        assert_eq!(cell.text, "h r ");
        assert_eq!(
            cell.segments,
            vec![
                ("h".to_string(), HintHand::Left),
                (" ".to_string(), HintHand::None),
                ("r".to_string(), HintHand::Right),
                (" ".to_string(), HintHand::None),
            ]
        );
    }

    #[test]
    fn test_kongming_single_char_hint_cell_colors_left_and_pads_none() {
        // 单字一击字在打词/常规排版流中：单字「中」对应左手 f，居中补白为 None，字本身为 Left（粉色）。
        // 且兼容编码串带 % 前缀或 = 词典后缀的情形。
        let words = vec!["中".to_string(), "的".to_string()];
        let hints = vec![hint("f="), hint("d")];
        let cells = layout_code_hint_line(&words, &hints, &[]);
        assert_eq!(cells.len(), 2);

        // 「中」字宽 2，码宽 1，居中补充 1 个空格在右侧 -> "f "
        assert_eq!(cells[0].text, "f ");
        assert_eq!(cells[0].hand, HintHand::Left);
        assert_eq!(
            cells[0].segments,
            vec![
                ("f".to_string(), HintHand::Left),
                (" ".to_string(), HintHand::None),
            ]
        );

        // 「的」字宽 2，码宽 1，右手 d
        assert_eq!(cells[1].text, "d ");
        assert_eq!(cells[1].hand, HintHand::Right);
        assert_eq!(
            cells[1].segments,
            vec![
                ("d".to_string(), HintHand::Right),
                (" ".to_string(), HintHand::None),
            ]
        );
    }

    #[test]
    fn test_hint_cell_fallback_padding_spaces_have_none_hand() {
        // 常规单手简码或词提：补白空格绝不染色，只有实际按键有手区色。
        let words = vec!["测试".to_string()];
        let hints = vec![hint("_ab")];
        let cells = layout_code_hint_line(&words, &hints, &[]);
        assert_eq!(cells.len(), 1);
        let cell = &cells[0];
        // 词宽 4，码 "ab" 宽 2，左右各补 1 空格 -> " ab "
        assert_eq!(cell.text, " ab ");
        assert_eq!(cell.hand, HintHand::Left);
        assert_eq!(
            cell.segments,
            vec![
                (" ".to_string(), HintHand::None),
                ("ab".to_string(), HintHand::Left),
                (" ".to_string(), HintHand::None),
            ]
        );
    }

    #[test]
    fn test_kongming_1hit_chords_shape_and_coverage() {
        assert_eq!(KONGMING_1HIT_CHORDS.len(), 455);
        let mut chars = Vec::new();
        for &(c, chord, hand) in KONGMING_1HIT_CHORDS {
            assert!(!chord.is_empty(), "指法不可为空: {c}");
            assert!(
                matches!(hand, HintHand::Left | HintHand::Right | HintHand::TwoHand),
                "一击字指法必须为左/右/双手: {c}"
            );
            chars.push(c);
        }
        let orig_len = chars.len();
        chars.sort_unstable();
        chars.dedup();
        assert_eq!(chars.len(), orig_len, "一击字映射表不应有重复字符");

        // 单手码沿用简洁记法；双手码保留方案逻辑码（如「代 = ID」）
        assert_eq!(kongming_1hit_hint('中'), Some(("f", HintHand::Left)));
        assert_eq!(kongming_1hit_hint('的'), Some(("d", HintHand::Right)));
        assert_eq!(kongming_1hit_hint('好'), Some(("h", HintHand::Left)));
        assert_eq!(kongming_1hit_hint('一'), Some(("y", HintHand::Right)));
        assert_eq!(kongming_1hit_hint('性'), Some(("A", HintHand::Left)));
        assert_eq!(kongming_1hit_hint('定'), Some(("A", HintHand::Right)));
        assert_eq!(kongming_1hit_hint('等'), Some(("a␣", HintHand::Left)));
        assert_eq!(kongming_1hit_hint('由'), Some(("a␣", HintHand::Right)));
        assert_eq!(kongming_1hit_hint('还'), Some(("h␣", HintHand::Left)));
        assert_eq!(kongming_1hit_hint('很'), Some(("h␣", HintHand::Right)));
        assert_eq!(kongming_1hit_hint('教'), Some(("A␣", HintHand::Left)));
        assert_eq!(kongming_1hit_hint('代'), Some(("ID", HintHand::TwoHand)));

        // 未收录字符返回 None
        assert_eq!(kongming_1hit_hint('龙'), None);
    }

    #[test]
    fn test_kongming_1hit_words_shape_and_coverage() {
        assert_eq!(KONGMING_1HIT_WORDS.len(), 765);
        for &(w, chord) in KONGMING_1HIT_WORDS {
            assert!(!chord.is_empty(), "编码不可为空: {w}");
            assert!((2..=3).contains(&chord.len()), "一击词编码长度应为 2–3: {w} -> {chord}");
            assert!(w.chars().count() >= 2, "词条必须为多字词: {w}");
        }
        let mut words: Vec<&str> = KONGMING_1HIT_WORDS.iter().map(|&(w, _)| w).collect();
        let orig_len = words.len();
        words.sort_unstable();
        words.dedup();
        assert_eq!(words.len(), orig_len, "一击词映射表不应有重复词条");

        // 验证代表性词条（方案当前码面；优先取自动上屏码）
        assert_eq!(
            kongming_1hit_word_hint("可以"),
            Some(("ky", HintHand::TwoHand))
        );
        assert_eq!(
            kongming_1hit_word_hint("一个"),
            Some(("e'", HintHand::TwoHand))
        );
        assert_eq!(
            kongming_1hit_word_hint("没有"),
            Some(("mY", HintHand::TwoHand))
        );
        assert_eq!(
            kongming_1hit_word_hint("我们"),
            Some(("0c", HintHand::TwoHand))
        );
        assert_eq!(
            kongming_1hit_word_hint("这个"),
            Some(("s`", HintHand::TwoHand))
        );
        assert_eq!(
            kongming_1hit_word_hint("农村"),
            Some(("NC", HintHand::TwoHand))
        );

        // 未收录词条返回 None
        assert_eq!(kongming_1hit_word_hint("奥林匹克"), None);
    }
}

