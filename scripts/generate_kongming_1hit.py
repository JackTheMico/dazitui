#!/usr/bin/env python3
"""从空明码方案（kmrime）当前词库重算 dazitui 的一击字/一击词内置数据。

用法：
    python3 scripts/generate_kongming_1hit.py [KMRIME_ROOT]

默认 KMRIME_ROOT=/home/jackwy/Nextcloud/kmrime。口径与方案 tools/km_chord_model.py 一致：

- 一击字：前 1500 常用字中「一击上屏」的字（一次并击、唯一候选、无延伸），按词频排序；
- 一击词：dazitui 自有 1500 常用词中，存在「一次并击可达且该词为词库首候选」的码的词，
  按词频排序；同等条件优先无需空格的自动上屏码。
- 全库一击词：方案全词库中「一次并击可达且该词为首候选」的多字纯中文词条，
  按方案 essay 词频排序，作为独立内置练习集，不替换上面的常用子集。

同时重写：
    dazitui-core/data/kongming-1hit-chars.txt
    dazitui-core/data/kongming-1hit-words.txt
    dazitui-core/data/kongming-all-1hit-words.txt
    dazitui-core/src/code_hint.rs 中三张静态表
"""
import collections
import importlib.util
import json
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
K = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else '/home/jackwy/Nextcloud/kmrime')


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, str(path))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


chord = load('km_chord_model', K / 'tools/km_chord_model.py')
kmopt = load('km_jianma_opt', K / 'tools/km_jianma_opt.py')
charmod = load('km_char_onehit', K / 'tools/km_char_onehit.py')

entries = charmod.load_entries(charmod.DICT_S)
codes, word_codes, prefix = charmod.build(entries)
one = chord.one_press_codes()
freq = kmopt.load_freq()
rank, cf = charmod.char_rank(freq)

# 一次并击码的物理手区（与 km_chord_model.one_press_codes 同一枚举口径）
real = chord.realizations()
lefts, rights = [frozenset()], [frozenset()]
for sym, h in real.items():
    lefts.extend(h['L'])
    rights.extend(h['R'])
lefts.extend(frozenset({k}) for k in chord.LEFT_KEYS)
rights.extend(frozenset({k}) for k in chord.RIGHT_KEYS if k not in chord.UNXLIT)
hands = collections.defaultdict(set)
for space in (frozenset(), frozenset({' '})):
    for lk in lefts:
        for rk in rights:
            code = chord.press(lk | rk | space)
            if code:
                hands[code].add(('L' if lk else '') + ('R' if rk else ''))


def hand_of(code):
    pattern = hands.get(code)
    if pattern == {'L'}:
        return 'Left'
    if pattern == {'R'}:
        return 'Right'
    return 'TwoHand'


def display_chord(code):
    """单手码沿用简洁记法；双手码保留方案逻辑码。"""
    if len(code) == 2 and code[1] == '=' and code[0].isalpha():
        return code[0]
    if len(code) == 2 and code[0] == '=' and code[1].isalpha():
        return code[1]
    if len(code) == 3 and code[1:] == '_=' and code[0].isalpha():
        return code[0] + '\u2423'
    if len(code) == 3 and code[:2] == '=_' and code[2].isalpha():
        return code[2] + '\u2423'
    return code


rows, _ = charmod.analyze(rank[:1500], codes, word_codes, prefix, cf, one)
chars = [(r[1], r[3]) for r in rows if r[2] == '一击']

common = []
for name in ('common-words-qian.txt', 'common-words-zhong.txt', 'common-words-hou.txt'):
    common.extend(w for w in (ROOT / 'dazitui-core/data' / name).read_text(encoding='utf-8')
                  .replace('\n', '').split('，') if w)
words = []
for w in common:
    hits = [c for c in word_codes.get(w, []) if c in one and codes[c] and codes[c][0] == w]
    if hits:
        hits.sort(key=lambda c: (0 if charmod.tier(c, codes, prefix) == 'auto' else 1, len(c), c))
        words.append((w, hits[0]))

# 全库一击词：每个一次并击可达码的首候选，仅保留多字纯中文词条。
# 同一词有多个一击码时按 auto > 短码 > 字典序择优；排序用方案 essay 词频。
all_candidates = {}
for code in one:
    if code not in codes:
        continue
    word = codes[code][0]
    all_candidates.setdefault(word, []).append(code)
all_words = []
for word, code_list in all_candidates.items():
    if len(word) < 2 or not all(charmod.is_cjk(ch) for ch in word):
        continue
    code = min(
        code_list,
        key=lambda c: (0 if charmod.tier(c, codes, prefix) == 'auto' else 1, len(c), c),
    )
    all_words.append((word, code))
all_words.sort(key=lambda p: (-freq.get(p[0], 0), p[0]))

if not chars or not words or not all_words:
    raise SystemExit('未生成任何一击字/词，请检查方案词库路径')
print(f'一击字 {len(chars)} 个，一击词 {len(words)} 个，全库一击词 {len(all_words)} 个')

(ROOT / 'dazitui-core/data/kongming-1hit-chars.txt').write_text(
    ''.join(c for c, _ in chars) + '\n', encoding='utf-8')
(ROOT / 'dazitui-core/data/kongming-1hit-words.txt').write_text(
    '，'.join(w for w, _ in words) + '\n', encoding='utf-8')
(ROOT / 'dazitui-core/data/kongming-all-1hit-words.txt').write_text(
    '，'.join(w for w, _ in all_words) + '\n', encoding='utf-8')

path = ROOT / 'dazitui-core/src/code_hint.rs'
src = path.read_text(encoding='utf-8')

char_start = src.find('/// 空明码「一击上屏」单字表')
if char_start < 0:
    char_start = src.index('/// 空明码方案 B 权威 204 个一击单字')
char_end = src.index('\n];', src.index('pub const KONGMING_1HIT_CHORDS', char_start)) + len('\n];')
char_block = f'''/// 空明码「一击上屏」单字表（字符、编码提示文本、手区归属），与内置一击字赛文同源。
///
/// 由 `scripts/generate_kongming_1hit.py` 从方案词库生成。前 1500 常用字中一次并击即可上屏的字，
/// 按词频排序。单手码沿用简洁记法（`f=` → `f`、`a_=` → `a␣`），双手码保留方案逻辑码。
pub const KONGMING_1HIT_CHORDS: &[(char, &str, HintHand)] = &[
''' + ''.join(
    f"    ('{c}', {json.dumps(display_chord(code), ensure_ascii=False)}, HintHand::{hand_of(code)}),\n"
    for c, code in chars
) + '];'
src = src[:char_start] + char_block + src[char_end:]

word_start = src.find('/// 空明码「可单并击输入」词条表')
if word_start < 0:
    word_start = src.index('pub const KONGMING_1HIT_WORDS')
word_end = src.index('\n];', word_start) + len('\n];')
word_block = f'''/// 空明码「可单并击输入」词条表（词条、方案当前码面），与内置一击词赛文同源。
///
/// 由 `scripts/generate_kongming_1hit.py` 从方案词库生成。dazitui 自有 1500 常用词中，
/// 存在「一次并击可达且该词为词库首候选」的码的词，按词频排序。
pub const KONGMING_1HIT_WORDS: &[(&str, &str)] = &[
''' + ''.join(
    f"    ({json.dumps(w, ensure_ascii=False)}, {json.dumps(code, ensure_ascii=False)}),\n"
    for w, code in words
) + '];'
src = src[:word_start] + word_block + src[word_end:]

all_start = src.find('/// 空明码「全库一击词」词条表')
if all_start >= 0:
    all_end = src.index('\n];', src.index('pub const KONGMING_ALL_1HIT_WORDS', all_start)) + len('\n];')
else:
    all_end = src.index('\n];', src.index('pub const KONGMING_1HIT_WORDS')) + len('\n];')
all_block = f'''/// 空明码「全库一击词」词条表（词条、方案当前码面），与内置全库一击词赛文同源。
///
/// 由 `scripts/generate_kongming_1hit.py` 从方案词库生成。全库一次并击可达码的首候选中，
/// 多字纯中文词条按方案词频排序；与上面的 1500 常用子集互不影响。
pub const KONGMING_ALL_1HIT_WORDS: &[(&str, &str)] = &[
''' + ''.join(
    f"    ({json.dumps(w, ensure_ascii=False)}, {json.dumps(code, ensure_ascii=False)}),\n"
    for w, code in all_words
) + '];'
if all_start >= 0:
    src = src[:all_start] + all_block + src[all_end:]
else:
    src = src[:all_end] + '\n\n' + all_block + src[all_end:]
path.write_text(src, encoding='utf-8')

# 字模来自所有 data/*.txt，数据变了必须同步重生成
print('请运行 python3 scripts/generate_font16.py 更新字模')
