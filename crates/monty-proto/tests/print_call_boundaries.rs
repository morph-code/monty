//! 原生调用边界的线协议探针：按事件次序读取正文，再处理边界；只归组最外层调用。
#![cfg(feature = "worker")]

use std::mem;

use monty_proto::{
    FrameReader, PROTOCOL_VERSION, ext_result_to_proto, named_values_to_proto, pb,
    worker::{Child, dispatch_frame},
    write_frame,
};
use monty_types::{ExtFunctionResult, MontyObject};

use pb::{child_event::Kind as Event, parent_request::Kind as Request};

fn request(child: &mut Child, kind: Request) -> Vec<Event> {
    let mut input = Vec::new();
    write_frame(
        &mut input,
        &pb::ParentRequest {
            kind: Some(kind),
            ..Default::default()
        },
    )
    .unwrap();
    let (output, _) = dispatch_frame(child, &input);
    let mut reader = FrameReader::new(output.as_slice());
    let mut events = Vec::new();
    while let Some(event) = reader.read::<pb::ChildEvent>().unwrap() {
        events.push(event.kind.unwrap());
    }
    events
}

fn configured(interval: Option<u32>, enabled: bool) -> Child {
    let mut child = Child::default();
    let events = request(
        &mut child,
        Request::Configure(pb::Configure {
            protocol_version: PROTOCOL_VERSION,
            print_flush_interval_ms: interval,
            print_call_boundaries: enabled,
            type_check: false,
            ..Default::default()
        }),
    );
    assert!(matches!(events.as_slice(), [Event::Ok(_)]), "{events:?}");
    child
}

fn feed(child: &mut Child, code: &str) -> Vec<Event> {
    request(
        child,
        Request::Feed(pb::Feed {
            code: code.into(),
            ..Default::default()
        }),
    )
}

fn feed_with_tool(child: &mut Child, code: &str) -> Vec<Event> {
    let (inputs, values) = named_values_to_proto(vec![("tool".into(), MontyObject::function("tool", None))].into());
    request(
        child,
        Request::Feed(pb::Feed {
            code: code.into(),
            inputs,
            values: Some(values),
            ..Default::default()
        }),
    )
}

fn text(events: &[Event]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            Event::Print(p) => Some(p),
            _ => None,
        })
        .flat_map(|p| p.segments.iter())
        .map(|s| s.text.as_str())
        .collect()
}

fn blocks(events: &[Event]) -> Vec<String> {
    let mut depth = 0;
    let mut blocks = Vec::new();
    let mut pending = String::new();
    for event in events {
        if let Event::Print(p) = event {
            for s in &p.segments {
                assert!(depth > 0, "正文必须位于调用边界内");
                pending.push_str(&s.text);
            }
            match p.boundary() {
                pb::PrintBoundary::Unspecified => {}
                pb::PrintBoundary::Begin => depth += 1,
                pb::PrintBoundary::End => {
                    assert!(depth > 0);
                    depth -= 1;
                    if depth == 0 {
                        blocks.push(mem::take(&mut pending));
                    }
                }
            }
        }
    }
    assert_eq!(depth, 0, "正常 turn 终结前调用边界必须配对");
    blocks
}

fn boundaries(events: &[Event]) -> Vec<pb::PrintBoundary> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Print(p) if p.boundary() != pb::PrintBoundary::Unspecified => Some(p.boundary()),
            _ => None,
        })
        .collect()
}

fn dump(child: &mut Child) -> Vec<u8> {
    match request(child, Request::Dump(pb::Dump {})).pop().unwrap() {
        Event::DumpResult(d) => d.state.into_inner(),
        event => panic!("{event:?}"),
    }
}

fn load(child: &mut Child, state: Vec<u8>) -> Vec<Event> {
    request(child, Request::Load(pb::Load { state: state.into() }))
}

fn resume(child: &mut Child, call_id: u32) -> Vec<Event> {
    let (result, values) = ext_result_to_proto(ExtFunctionResult::Return(MontyObject::none()));
    request(
        child,
        Request::ResumeCall(pb::ResumeCall {
            call_id,
            result: Some(result),
            values,
        }),
    )
}

#[test]
fn separate_calls_multiline_large_unicode_and_empty_output() {
    for interval in [None, Some(0), Some(60_000)] {
        let mut child = configured(interval, true);
        let events = feed(
            &mut child,
            "print('a\\nb'); print('你😀'*9000,end=''); print('C',end=''); print('D',end=''); print('',end='')",
        );
        assert_eq!(
            blocks(&events),
            vec![
                "a\nb\n".to_owned(),
                "你😀".repeat(9000),
                "C".into(),
                "D".into(),
                String::new()
            ]
        );
        assert_eq!(boundaries(&events).len(), 10);
        assert!(matches!(events.last(), Some(Event::Complete(_))));
    }
}

#[test]
fn nested_str_and_repr_preserve_byte_and_stream_order() {
    let code = "import sys\nclass Nested:\n def __repr__(self):\n  print('repr',file=sys.stderr,end='')\n  return 'R'\n def __str__(self):\n  print('inner',[self],sep=':',end='!',file=sys.stderr)\n  return 'S'\nprint('outer',Nested(),sep='/',end='.')";
    let mut child = configured(Some(60_000), true);
    let events = feed(&mut child, code);
    assert_eq!(blocks(&events), vec!["outer/inner:repr[R]!S."]);
    assert_eq!(
        boundaries(&events),
        vec![
            pb::PrintBoundary::Begin,
            pb::PrintBoundary::Begin,
            pb::PrintBoundary::Begin,
            pb::PrintBoundary::End,
            pb::PrintBoundary::End,
            pb::PrintBoundary::End
        ]
    );
    let streams: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            Event::Print(p) => Some(p),
            _ => None,
        })
        .flat_map(|p| p.segments.iter())
        .map(|s| (s.stream(), s.text.as_str()))
        .collect();
    assert_eq!(
        streams,
        vec![
            (pb::PrintStream::Stdout, "outer/"),
            (pb::PrintStream::Stderr, "inner:"),
            (pb::PrintStream::Stderr, "repr"),
            (pb::PrintStream::Stderr, "[R]!"),
            (pb::PrintStream::Stdout, "S.")
        ]
    );
    let mut plain = configured(Some(60_000), false);
    assert_eq!(text(&feed(&mut plain, code)), text(&events));
}

#[test]
fn stdout_stderr_sep_end_and_flush_do_not_change_boundaries() {
    let mut child = configured(Some(60_000), true);
    let events = feed(
        &mut child,
        "import sys\nprint('a','b',sep='::',end='|',file=sys.stdout,flush=True)\nprint('c',end='',file=sys.stderr,flush=False)\nprint('',end='',flush=None)\nprint()\nprint('d',sep=None,end=None,file=None,flush=[])",
    );
    assert_eq!(blocks(&events), vec!["a::b|", "c", "", "\n", "d\n"]);
    let streams: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            Event::Print(p) => Some(p),
            _ => None,
        })
        .flat_map(|p| p.segments.iter())
        .map(|s| (s.stream(), s.text.as_str()))
        .collect();
    assert_eq!(
        streams,
        vec![
            (pb::PrintStream::Stdout, "a::b|"),
            (pb::PrintStream::Stderr, "c"),
            (pb::PrintStream::Stdout, "\n"),
            (pb::PrintStream::Stdout, "d\n")
        ]
    );
}

#[test]
fn invalid_builtin_arguments_are_bracketed_without_text() {
    for code in [
        "print('x',sep=1)",
        "print('x',end=1)",
        "print('x',file=3)",
        "print(unexpected=1)",
    ] {
        let mut child = configured(Some(60_000), true);
        let events = feed(&mut child, code);
        assert_eq!(blocks(&events), vec![""]);
        assert_eq!(
            boundaries(&events),
            vec![pb::PrintBoundary::Begin, pb::PrintBoundary::End]
        );
        let mut plain = configured(Some(60_000), false);
        let expected = feed(&mut plain, code);
        assert_eq!(events.last(), expected.last());
        assert!(
            matches!(events.last(), Some(Event::Error(e)) if e.exception.as_ref().unwrap().exc_type == "TypeError")
        );
    }
}

#[test]
fn argument_evaluation_failure_never_enters_builtin() {
    let mut child = configured(Some(0), true);
    let events = feed(&mut child, "print(1/0)");
    assert_eq!(boundaries(&events), Vec::<pb::PrintBoundary>::new());
    assert_eq!(text(&events), "");
    assert!(
        matches!(events.last(), Some(Event::Error(e)) if e.exception.as_ref().unwrap().exc_type == "ZeroDivisionError")
    );
}

#[test]
fn ordinary_conversion_exception_closes_before_python_finally() {
    let mut child = configured(Some(60_000), true);
    let code = "class Bad:\n def __str__(self):\n  print('inner',end='|')\n  raise ValueError('ordinary')\nx=23\ntry:\n print('prefix',Bad(),sep=':')\nfinally:\n print('finally',end='|')\n x=99";
    let events = feed(&mut child, code);
    assert_eq!(blocks(&events), vec!["prefix:inner|", "finally|"]);
    assert!(
        matches!(events.last(), Some(Event::Error(e)) if e.exception.as_ref().unwrap().exc_type == "ValueError" && e.exception.as_ref().unwrap().message.as_deref() == Some("ordinary"))
    );
    let state = dump(&mut child);
    let mut restored = configured(Some(60_000), true);
    assert!(matches!(load(&mut restored, state).as_slice(), [Event::Ok(_)]));
    assert_eq!(blocks(&feed(&mut restored, "print(x)")), vec!["99\n"]);
}

#[test]
fn caught_nested_conversion_error_keeps_outer_call_open() {
    let mut child = configured(Some(60_000), true);
    let events = feed(
        &mut child,
        "class Bad:\n def __str__(self):\n  print('partial',end='|')\n  raise ValueError('ordinary')\nclass Outer:\n def __str__(self):\n  try:\n   print('try',Bad(),sep=':')\n  except ValueError:\n   print('caught',end='|')\n  return 'OUT'\nprint('pre',Outer(),sep=':',end='.')",
    );
    assert_eq!(blocks(&events), vec!["pre:try:partial|caught|OUT."]);
    assert!(matches!(events.last(), Some(Event::Complete(_))));
}

#[test]
fn conversion_external_call_remains_an_ordinary_unsupported_error() {
    let mut child = configured(Some(60_000), true);
    let events = feed_with_tool(
        &mut child,
        "class Bad:\n def __str__(self):\n  print('inner',end='|')\n  return tool()\nprint('prefix',Bad(),sep=':')",
    );
    assert_eq!(blocks(&events), vec!["prefix:inner|"]);
    assert!(
        matches!(events.last(), Some(Event::Error(e)) if e.exception.as_ref().unwrap().exc_type == "NotImplementedError")
    );
    assert!(!events.iter().any(|e| matches!(e, Event::FunctionCall(_))));
}

#[test]
fn abort_feed_skips_finally_and_restores_without_replaying_prints() {
    let mut child = configured(Some(60_000), true);
    let events = feed_with_tool(
        &mut child,
        "p=print\nx=23\ntry:\n print('before',end='|')\n tool()\nfinally:\n print('finally')\n x=99\nprint('suffix')",
    );
    assert_eq!(blocks(&events), vec!["before|"]);
    assert!(matches!(events.last(), Some(Event::FunctionCall(_))));
    let events = request(
        &mut child,
        Request::AbortFeed(pb::AbortFeed {
            exception: Some(pb::RaisedException {
                exc_type: "RuntimeError".into(),
                message: Some("original effect remains unknown".into()),
                ..Default::default()
            }),
        }),
    );
    assert_eq!(boundaries(&events), Vec::<pb::PrintBoundary>::new());
    assert!(matches!(events.last(), Some(Event::Error(_))));
    let state = dump(&mut child);
    let mut restored = configured(Some(0), true);
    assert!(matches!(load(&mut restored, state).as_slice(), [Event::Ok(_)]));
    let events = feed(&mut restored, "p(x,end='|');print('after',end='|')");
    assert_eq!(blocks(&events), vec!["23|", "after|"]);
    assert!(!events.iter().any(|e| matches!(e, Event::FunctionCall(_))));
}

#[test]
fn suspended_dump_load_uses_checkout_setting_and_does_not_replay_prefix() {
    let mut child = configured(Some(60_000), false);
    let events = feed_with_tool(
        &mut child,
        "p=print\nprint('before',end='|')\ntool()\np('after',end='|')",
    );
    assert_eq!(boundaries(&events), Vec::<pb::PrintBoundary>::new());
    assert_eq!(text(&events), "before|");
    let state = dump(&mut child);
    let mut restored = configured(Some(60_000), true);
    let loaded = load(&mut restored, state);
    let call_id = match loaded.last().unwrap() {
        Event::FunctionCall(c) => c.call_id,
        e => panic!("{e:?}"),
    };
    assert_eq!(boundaries(&loaded), Vec::<pb::PrintBoundary>::new());
    assert_eq!(blocks(&resume(&mut restored, call_id)), vec!["after|"]);
    assert_eq!(blocks(&feed(&mut restored, "p('next',end='|')")), vec!["next|"]);
}

#[test]
fn disabled_mode_keeps_original_batching() {
    let mut child = configured(Some(60_000), false);
    let events = feed(&mut child, "print('a');print('b');print('',end='')");
    assert_eq!(boundaries(&events), Vec::<pb::PrintBoundary>::new());
    assert_eq!(events.iter().filter(|e| matches!(e, Event::Print(_))).count(), 1);
    assert_eq!(text(&events), "a\nb\n");
}

#[test]
fn boundary_mode_requires_version_seven() {
    let mut child = Child::default();
    let events = request(
        &mut child,
        Request::Configure(pb::Configure {
            protocol_version: 6,
            print_call_boundaries: true,
            ..Default::default()
        }),
    );
    assert!(matches!(events.as_slice(), [Event::FatalError(_)]));
}
