use dazitui_core::{detect_and_parse_book, LoadOptions};
use std::path::Path;

#[test]
fn test_parse_daoguiyixian_file_if_exists() {
    let path = Path::new("/home/jackwy/文档/道诡异仙.txt");
    if !path.exists() {
        return;
    }
    let content = std::fs::read_to_string(path).expect("读取文件失败");
    let catalog = detect_and_parse_book(content, "道诡异仙.txt").expect("应成功解析《道诡异仙》");

    assert_eq!(catalog.book_title, "道诡异仙");
    assert_eq!(catalog.author.as_deref(), Some("狐尾的笔"));
    assert!(catalog.chapters.len() >= 960, "章节数应大于等于960，实际为: {}", catalog.chapters.len());

    // 第一章
    let ch1 = &catalog.chapters[1];
    assert!(ch1.title.contains("第一章"));
    assert!(ch1.title.contains("师傅"));

    let text = catalog
        .chapter_text(1, path, &LoadOptions::default())
        .expect("载入第一章应成功");
    assert!(text.title.contains("第一章"));
    assert!(text.content.contains("李火旺举起手中的捣药杆"));
}

#[test]
fn test_chapter_2_typing_quotes() {
    let path = Path::new("/home/jackwy/文档/道诡异仙.txt");
    if !path.exists() {
        return;
    }
    let content = std::fs::read_to_string(path).expect("读取文件失败");
    let catalog = detect_and_parse_book(content, "道诡异仙.txt").expect("应成功解析《道诡异仙》");

    // 第二章是 index 2 (因为 index 0 是前言与简介，index 1 是第一章)
    let ch2_text = catalog
        .chapter_text(2, path, &LoadOptions::default())
        .expect("载入第二章应成功");
    assert_eq!(ch2_text.title, "第二章 李火旺");

    // 验证章节文本已自动去除换行符，且第一段以双引号对话开头
    assert!(!ch2_text.content.contains('\n'), "章节正文不应包含换行符");
    assert!(ch2_text.content.starts_with("“呼，终于回来了。”"));

    let mut session = dazitui_core::Session::new(&ch2_text.content);

    // 1. 输入第一段开头的双引号：模拟输入法自动上屏成对引号 “”
    let res1 = session.type_text("“”");
    assert_eq!(res1.statuses, vec![dazitui_core::CharStatus::Correct]);
    assert_eq!(session.input_chars(), &['“']);

    // 2. 输入第一段正文
    let res2 = session.type_text("呼，终于回来了。");
    assert!(res2.statuses.iter().all(|s| *s == dazitui_core::CharStatus::Correct));

    // 3. 输入第一段闭引号：模拟输入法产生 ASCII 双引号 "
    let res3 = session.type_text("\"");
    assert_eq!(res3.statuses, vec![dazitui_core::CharStatus::Correct]);

    // 4. 输入第一段剩余部分
    let res4 = session.type_text("松了一口气的李火旺对着床头麦克风呼喊起来。");
    assert!(res4.statuses.iter().all(|s| *s == dazitui_core::CharStatus::Correct));

    // 5. 跨段落输入第二段（原文本中为换行分隔，现已平滑衔接）
    let res5 = session.type_text("没过一会，他的主治医师托着一个白色平板带着护士从病房门口走了进来。");
    assert!(res5.statuses.iter().all(|s| *s == dazitui_core::CharStatus::Correct));

    // 6. 输入第三段对话（同样以双引号开头）
    let res6 = session.type_text("“小李，感觉如何？这一次的幻觉有什么新的变化？”");
    assert!(res6.statuses.iter().all(|s| *s == dazitui_core::CharStatus::Correct));

    // 验证原文状态：已录入字符全部为 Correct，未发生错字与整体位移
    let statuses = session.original_status();
    for (i, &ic) in session.input_chars().iter().enumerate() {
        assert_eq!(
            statuses[i].1,
            Some(dazitui_core::CharStatus::Correct),
            "字符 [{}] '{}' 状态应为 Correct",
            i,
            ic
        );
    }
}

#[test]
fn test_chapter_progress_and_session_resumption() {
    use dazitui_core::{CharStatus, ChapterProgress, Session, StatsDb};

    let mut db = StatsDb::open_in_memory().unwrap();

    let file_path = "/home/jackwy/文档/道诡异仙.txt";
    let progress = ChapterProgress {
        file_path: file_path.to_string(),
        chapter_index: 1,
        char_offset: 120,
        total_chars: 2000,
        updated_at: "2026-09-19 18:00:00".to_string(),
    };

    db.save_chapter_progress(&progress).unwrap();

    let loaded = db.get_chapter_progress(file_path).unwrap().expect("应查到进度");
    assert_eq!(loaded.chapter_index, 1);
    assert_eq!(loaded.char_offset, 120);
    assert_eq!(loaded.total_chars, 2000);

    // 断点续打 Session 行为验证
    let chapter_content = "李火旺举起手中的捣药杆，狠狠地向着药臼里砸去。每一次砸下，都伴随着沉闷的声响。";
    let mut session = Session::new(chapter_content);
    let original_len = session.original_len();
    assert_eq!(session.resumed_chars(), 0);

    // 恢复到第 10 个字符
    session.set_resumed_chars(10);
    assert_eq!(session.resumed_chars(), 10);
    assert_eq!(session.input_chars().len(), 10);

    let status = session.original_status();
    // 前 10 个字符应标为 Some(CharStatus::Correct)
    for i in 0..10 {
        assert_eq!(status[i].1, Some(CharStatus::Correct));
    }
    // 第 10 个字符及之后未输入，应为 None
    for i in 10..original_len {
        assert_eq!(status[i].1, None);
    }

    // 清理进度
    db.delete_chapter_progress(file_path).unwrap();
    assert!(db.get_chapter_progress(file_path).unwrap().is_none());
}

