//! Tests for `PrintWriter::CollectString` — the output capture mode used by
//! hosts that need to intercept `print()` rather than pass it through to
//! stdout.
//!
//! Expected output is asserted via [`insta::assert_snapshot!`] with inline
//! snapshots — multi-line print output renders naturally in the snapshot
//! literal, which is substantially clearer than an escaped `"a\nb\n"` string.
//! To update after an intentional change, run `cargo insta review` (or set
//! `INSTA_UPDATE=always`).

use std::borrow::Cow;

use insta::assert_snapshot;
use monty::MontyRun;
use monty_types::{
    CollectedStreams, CompileOptions, ExcType, MontyException, PrintStream, PrintWriter, PrintWriterCallback,
    ResourceTracker,
};

/// Run `code` under Monty with a string-collecting `PrintWriter` and return
/// whatever was printed. Panics on parse/runtime errors — callers only care
/// about the captured output. `CollectString` keeps no stream labels, so
/// stderr output lands in the same buffer; see `run_and_capture_streams`.
fn run_and_capture(code: &str) -> String {
    let mut ex = MontyRun::new(code.to_owned(), "test.py", vec![], CompileOptions::default()).unwrap();
    let mut output = String::new();
    ex.run(
        vec![],
        ResourceTracker::default(),
        PrintWriter::collect_string(&mut output),
    )
    .unwrap();
    output
}

#[test]
fn print_single_string() {
    assert_snapshot!(run_and_capture("print('hello')"), @"hello
");
}

#[test]
fn print_multiple_args() {
    assert_snapshot!(run_and_capture("print('hello', 'world')"), @"hello world
");
}

#[test]
fn print_multiple_statements() {
    assert_snapshot!(
        run_and_capture("print('one')\nprint('two')\nprint('three')"),
        @r"
    one
    two
    three
    "
    );
}

#[test]
fn print_empty() {
    assert_snapshot!(run_and_capture("print()"), @"
");
}

#[test]
fn print_integers() {
    assert_snapshot!(run_and_capture("print(1, 2, 3)"), @"1 2 3
");
}

#[test]
fn print_mixed_types() {
    assert_snapshot!(run_and_capture("print('count:', 42, True)"), @"count: 42 True
");
}

#[test]
fn print_in_function() {
    let code = "
def greet(name):
    print('Hello', name)

greet('Alice')
greet('Bob')
";
    assert_snapshot!(run_and_capture(code), @r"
    Hello Alice
    Hello Bob
    ");
}

#[test]
fn print_in_loop() {
    let code = "
for i in range(3):
    print(i)
";
    assert_snapshot!(run_and_capture(code), @r"
    0
    1
    2
    ");
}

#[test]
fn collect_output_accessible_after_run() {
    assert_snapshot!(run_and_capture("print('test')"), @"test
");
}

#[test]
fn writer_reuse_accumulates() {
    let mut output = String::new();

    let mut ex1 = MontyRun::new(
        "print('first')".to_owned(),
        "test.py",
        vec![],
        CompileOptions::default(),
    )
    .unwrap();
    ex1.run(
        vec![],
        ResourceTracker::default(),
        PrintWriter::collect_string(&mut output),
    )
    .unwrap();

    let mut ex2 = MontyRun::new(
        "print('second')".to_owned(),
        "test.py",
        vec![],
        CompileOptions::default(),
    )
    .unwrap();
    ex2.run(
        vec![],
        ResourceTracker::default(),
        PrintWriter::collect_string(&mut output),
    )
    .unwrap();

    assert_snapshot!(output, @r"
    first
    second
    ");
}

#[test]
fn disabled_suppresses_output() {
    let code = "
for i in range(100):
    print('this should be suppressed', i)
";
    let mut ex = MontyRun::new(code.to_owned(), "test.py", vec![], CompileOptions::default()).unwrap();
    // Should complete without error, output is silently discarded
    let result = ex.run(vec![], ResourceTracker::default(), PrintWriter::Disabled);
    assert!(result.is_ok());
}

// === print() kwargs tests ===

#[test]
fn print_custom_sep() {
    assert_snapshot!(run_and_capture("print('a', 'b', 'c', sep='-')"), @"a-b-c
");
}

#[test]
fn print_custom_end() {
    assert_snapshot!(run_and_capture("print('hello', end='!')"), @"hello!");
}

#[test]
fn print_custom_sep_and_end() {
    assert_snapshot!(
        run_and_capture("print('x', 'y', 'z', sep=', ', end='\\n---\\n')"),
        @r"
    x, y, z
    ---
    "
    );
}

#[test]
fn print_empty_sep() {
    assert_snapshot!(run_and_capture("print('a', 'b', 'c', sep='')"), @"abc
");
}

#[test]
fn print_empty_end() {
    assert_snapshot!(
        run_and_capture("print('first', end='')\nprint('second')"),
        @"firstsecond
    "
    );
}

#[test]
fn print_sep_none() {
    // sep=None should use default space
    assert_snapshot!(run_and_capture("print('a', 'b', sep=None)"), @"a b
");
}

#[test]
fn print_end_none() {
    // end=None should use empty string (our interpretation)
    assert_snapshot!(run_and_capture("print('hello', end=None)"), @"hello
");
}

#[test]
fn print_flush_ignored() {
    // flush=True should be accepted but ignored
    assert_snapshot!(run_and_capture("print('test', flush=True)"), @"test
");
}

#[test]
fn print_kwargs_dict() {
    // Use a dict literal instead of dict() since dict builtin is not implemented
    assert_snapshot!(run_and_capture("print('a', 'b', **{'sep': '-'})"), @"a-b
");
}

#[test]
fn print_only_kwargs_no_args() {
    assert_snapshot!(run_and_capture("print(sep='-', end='!')"), @"!");
}

#[test]
fn print_multiline_sep() {
    assert_snapshot!(run_and_capture("print(1, 2, 3, sep='\\n')"), @r"
    1
    2
    3
    ");
}

// === file= routing ===

/// Run `code` and return the labelled fragments, so a test can tell which
/// stream each one went to.
fn run_and_capture_streams(code: &str) -> Vec<(PrintStream, String)> {
    let mut ex = MontyRun::new(code.to_owned(), "test.py", vec![], CompileOptions::default()).unwrap();
    let mut streams = CollectedStreams::default();
    ex.run(
        vec![],
        ResourceTracker::default(),
        PrintWriter::collect_streams(&mut streams),
    )
    .unwrap();
    streams.into_entries()
}

#[test]
fn print_file_stderr_is_labelled_stderr() {
    assert_eq!(
        run_and_capture_streams("import sys\nprint('oops', file=sys.stderr)"),
        vec![(PrintStream::Stderr, "oops\n".to_owned())]
    );
}

#[test]
fn print_file_stdout_matches_the_default() {
    assert_eq!(
        run_and_capture_streams("import sys\nprint('a', file=sys.stdout)\nprint('b')"),
        vec![(PrintStream::Stdout, "a\nb\n".to_owned())]
    );
}

/// Consecutive fragments merge per stream, so alternating between the two
/// produces one entry per switch and keeps them in the order printed.
#[test]
fn print_alternating_streams_keep_their_order() {
    assert_eq!(
        run_and_capture_streams("import sys\nprint('1')\nprint('2', file=sys.stderr)\nprint('3')"),
        vec![
            (PrintStream::Stdout, "1\n".to_owned()),
            (PrintStream::Stderr, "2\n".to_owned()),
            (PrintStream::Stdout, "3\n".to_owned()),
        ]
    );
}

/// `sep` and `end` follow the file the call names, not the default stream.
#[test]
fn print_sep_and_end_follow_the_file() {
    assert_eq!(
        run_and_capture_streams("import sys\nprint('a', 'b', sep='-', end='!', file=sys.stderr)"),
        vec![(PrintStream::Stderr, "a-b!".to_owned())]
    );
}

#[test]
fn print_file_other_than_a_sys_stream_raises() {
    let mut ex = MontyRun::new(
        "print('x', file=42)".to_owned(),
        "test.py",
        vec![],
        CompileOptions::default(),
    )
    .unwrap();
    let err = ex
        .run(vec![], ResourceTracker::default(), PrintWriter::Disabled)
        .expect_err("only sys.stdout and sys.stderr can be written to");
    assert_snapshot!(
        err.message().unwrap(),
        @"print() 'file' argument must be sys.stdout or sys.stderr, not int"
    );
}

/// 验证边界通知失败时的引用释放与原异常优先级。
#[derive(Default)]
struct BoundaryCallback {
    fail_begin: bool,
    fail_end: bool,
    events: Vec<&'static str>,
}

impl PrintWriterCallback for BoundaryCallback {
    fn print_begin(&mut self) -> Result<(), MontyException> {
        self.events.push("begin");
        if self.fail_begin {
            Err(MontyException::new(ExcType::RuntimeError, Some("begin failed".into())))
        } else {
            Ok(())
        }
    }

    fn print_end(&mut self) -> Result<(), MontyException> {
        self.events.push("end");
        if self.fail_end {
            Err(MontyException::new(ExcType::RuntimeError, Some("end failed".into())))
        } else {
            Ok(())
        }
    }

    fn stdout_write(&mut self, _text: Cow<'_, str>) -> Result<(), MontyException> {
        Ok(())
    }
    fn stdout_push(&mut self, _end: char) -> Result<(), MontyException> {
        Ok(())
    }
}

#[test]
fn print_begin_failure_releases_owned_arguments_without_end() {
    let mut callback = BoundaryCallback {
        fail_begin: true,
        ..Default::default()
    };
    let mut run = MontyRun::new(
        "print(['x' * 1000])".into(),
        "test.py",
        vec![],
        CompileOptions::default(),
    )
    .unwrap();
    let error = run
        .run(vec![], ResourceTracker::default(), PrintWriter::Callback(&mut callback))
        .unwrap_err();
    assert_eq!(error.exc_type(), ExcType::RuntimeError);
    assert_eq!(error.message(), Some("begin failed"));
    assert_eq!(callback.events, vec!["begin"]);
}

#[test]
fn print_end_failure_propagates_on_successful_print() {
    let mut callback = BoundaryCallback {
        fail_end: true,
        ..Default::default()
    };
    let mut run = MontyRun::new("print('x')".into(), "test.py", vec![], CompileOptions::default()).unwrap();
    let error = run
        .run(vec![], ResourceTracker::default(), PrintWriter::Callback(&mut callback))
        .unwrap_err();
    assert_eq!(error.exc_type(), ExcType::RuntimeError);
    assert_eq!(error.message(), Some("end failed"));
    assert_eq!(callback.events, vec!["begin", "end"]);
}

#[test]
fn print_original_exception_survives_end_notification_failure() {
    let mut callback = BoundaryCallback {
        fail_end: true,
        ..Default::default()
    };
    let mut run = MontyRun::new(
        "class Bad:\n def __str__(self):\n  raise ValueError('ordinary')\nprint(Bad())".into(),
        "test.py",
        vec![],
        CompileOptions::default(),
    )
    .unwrap();
    let error = run
        .run(vec![], ResourceTracker::default(), PrintWriter::Callback(&mut callback))
        .unwrap_err();
    assert_eq!(error.exc_type(), ExcType::ValueError);
    assert_eq!(error.message(), Some("ordinary"));
    assert_eq!(callback.events, vec!["begin", "end"]);
}
