use criterion::{Criterion, criterion_group, criterion_main};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use hnx::{
    app::App,
    model::{Comment, Feed, Item, Source, StoryPage, Thread},
    theme::Theme,
    ui,
};
use ratatui::{Terminal, backend::TestBackend};

const COMMENT_COUNT: usize = 10_000;

fn large_thread_app() -> App {
    let item = Item {
        id: 42,
        by: Some("benchmark".to_owned()),
        title: Some("A large Hacker News discussion".to_owned()),
        url: Some("https://example.com/benchmark".to_owned()),
        descendants: COMMENT_COUNT as u64,
        ..Item::default()
    };
    let mut app = App::new(StoryPage {
        feed: Feed::Top,
        query: None,
        items: vec![item.clone()],
        source: Source::Cache,
        stale: false,
        fetched_at: 1,
    });
    let comments = (0..COMMENT_COUNT)
        .map(|index| Comment {
            id: u64::try_from(index).expect("benchmark index fits u64") + 1_000,
            by: Some(format!("reader-{index}")),
            text: Some(format!(
                "Comment {index} contains enough text to exercise sanitizing and wrapping."
            )),
            parent: Some(item.id),
            depth: u32::try_from(index % 8).expect("small depth fits u32"),
            ..Comment::default()
        })
        .collect();
    app.load_thread(Thread {
        item,
        comments,
        source: Source::Cache,
        stale: false,
        fetched_at: 1,
    });
    let _ = app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
    app
}

fn render_large_thread(c: &mut Criterion) {
    let mut group = c.benchmark_group("rendering");
    group.sample_size(100);
    group.bench_function("key_to_frame_10k_comments_120x40", |bencher| {
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).expect("test terminal initializes");
        let mut app = large_thread_app();
        let theme = Theme::classic();
        let mut move_up = true;

        bencher.iter(|| {
            let code = if move_up { KeyCode::Up } else { KeyCode::Down };
            move_up = !move_up;
            let _ = app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
            terminal
                .draw(|frame| ui::render(frame, &mut app, &theme))
                .expect("test frame renders");
            std::hint::black_box(terminal.backend().buffer().cell((0, 0)));
        });
    });
    group.finish();
}

criterion_group!(benches, render_large_thread);
criterion_main!(benches);
