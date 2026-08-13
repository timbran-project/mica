use super::{
    AuthorityContext, BuiltinResultKind, CompileError, Emission, Instruction, Operand, Program,
    RuntimeError, SYSTEM_ENDPOINT, SourceTaskError, SpawnRequest, SpawnTarget,
    SubscriptionInitialDelivery, SubscriptionRequest, SubscriptionSubject, SuspendKind, TaskError,
    TaskManagerError, TaskOutcome, endpoint_open_relation, param_relation,
};
use super::{FileinMode, SourceRunner, TaskInput, TaskRequest};
use super::{relation_name_relation, subject_fact_relation};
use mica_relation_kernel::RelationDurability;
use mica_var::{Identity, Symbol, Tuple, Value, ValueKind};
use std::sync::{Arc, Mutex, OnceLock};

// Tests that mutate process-global environment variables (MICA_SOURCE_ROOT)
// must serialize against each other because workspaces.mica reads the value
// at filein time, and parallel test threads can race on the env var.
fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn assert_relation_query_is_true(report: &super::RunReport) {
    let TaskOutcome::Complete { value, .. } = &report.outcome else {
        panic!("expected query to complete");
    };
    assert_eq!(
        *value,
        Value::bool(true),
        "unexpected query result: {}",
        report.render()
    );
}

fn assert_completed_value(report: &super::RunReport, expected: Value) {
    let TaskOutcome::Complete { value, .. } = &report.outcome else {
        panic!("expected task to complete");
    };
    assert_eq!(
        *value,
        expected,
        "unexpected task result: {}",
        report.render()
    );
}

fn query_relation<const COLUMNS: usize, const ROWS: usize>(
    heading: [&str; COLUMNS],
    rows: [[Value; COLUMNS]; ROWS],
) -> Value {
    Value::relation(
        heading.map(Symbol::intern),
        rows.into_iter().map(Tuple::from),
    )
    .unwrap()
}

#[test]
fn runner_executes_source_against_empty_kernel() {
    let mut runner = SourceRunner::new_empty();
    let report = runner.run_source("return 1 + 2").unwrap();

    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::int(3).unwrap()
    ));
}

#[test]
fn runner_installs_default_emit_builtin() {
    let mut runner = SourceRunner::new_empty();
    runner.run_source("make_identity(:target)").unwrap();
    let report = runner
        .run_source("return emit(#target, \"hello\")")
        .unwrap();
    let target = Identity::new(0x00e0_0000_0000_0000).unwrap();

    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, effects, .. }
            if value == Value::string("hello")
                && effects == vec![Emission::new(target, Value::string("hello"))]
    ));
}

#[test]
fn runner_emit_requires_target_identity() {
    let mut runner = SourceRunner::new_empty();

    let missing_target = runner.run_source("return emit(\"hello\")").unwrap_err();
    assert!(format!("{missing_target:?}").contains("emit expects target identity and value"));

    let non_identity = runner
        .run_source("return emit(:target, \"hello\")")
        .unwrap_err();
    assert!(format!("{non_identity:?}").contains("InvalidEffectTarget"));
}

#[test]
fn runner_string_primitives_support_character_level_munging() {
    let mut runner = SourceRunner::new_empty();

    assert!(matches!(
        runner.run_source("return string_len(\"hé\")").unwrap().outcome,
        TaskOutcome::Complete { value, .. } if value == Value::int(2).unwrap()
    ));
    assert!(matches!(
        runner.run_source("return string_chars(\"ab\")").unwrap().outcome,
        TaskOutcome::Complete { value, .. }
            if value == Value::list([Value::string("a"), Value::string("b")])
    ));
    assert!(matches!(
        runner
            .run_source("return string_slice(\"héllo\", 1, 4)")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. } if value == Value::string("éll")
    ));
    for (source, message, payload) in [
        (
            "string_slice(\"abc\", 2, 1)",
            "string_slice bounds 2..1 are invalid for a string of length 3",
            Value::list([
                Value::string("abc"),
                Value::int(2).unwrap(),
                Value::int(1).unwrap(),
            ]),
        ),
        (
            "string_slice(\"abc\", 0, 4)",
            "string_slice bounds 0..4 are invalid for a string of length 3",
            Value::list([
                Value::string("abc"),
                Value::int(0).unwrap(),
                Value::int(4).unwrap(),
            ]),
        ),
    ] {
        let report = runner
            .run_source(&format!(
                "try\n  return {source}\ncatch E_INDEX as err\n  return [err.message, err.value]\nend"
            ))
            .unwrap();
        assert!(matches!(
            report.outcome,
            TaskOutcome::Complete { value, .. }
                if value == Value::list([Value::string(message), payload])
        ));
    }
    assert!(matches!(
        runner
            .run_source(
                "let args = [\"abc\", 0, 4]
                 try
                   return string_slice(@args)
                 catch E_INDEX as err
                   return err.value
                 end"
            )
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. }
            if value == Value::list([
                Value::string("abc"),
                Value::int(0).unwrap(),
                Value::int(4).unwrap(),
            ])
    ));
    assert!(matches!(
        runner
            .run_source("return string_from_chars([\"h\", \"é\"])")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. } if value == Value::string("hé")
    ));
    assert!(matches!(
        runner
            .run_source("return string_concat(\"ab\", \"cd\", \"é\")")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. } if value == Value::string("abcdé")
    ));
    assert!(matches!(
        runner
            .run_source("return string_join([\"a\", \"b\", \"c\"], \"/\")")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. } if value == Value::string("a/b/c")
    ));
    assert!(matches!(
        runner
            .run_source("return url_encode_component(\"refs/heads/main I+é\")")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. }
            if value == Value::string("refs%2Fheads%2Fmain%20I%2B%C3%A9")
    ));
    assert!(matches!(
        runner
            .run_source("return url_decode_component(\"refs%2Fheads%2Fmain%20I%2B%C3%A9\")")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. } if value == Value::string("refs/heads/main I+é")
    ));
    assert!(matches!(
        runner
            .run_source("return url_decode_component(\"hello+world\")")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. } if value == Value::string("hello world")
    ));
    assert!(matches!(
        runner.run_source("return sort([3, 1, 2])").unwrap().outcome,
        TaskOutcome::Complete { value, .. }
            if value == Value::list([
                Value::int(1).unwrap(),
                Value::int(2).unwrap(),
                Value::int(3).unwrap(),
            ])
    ));
    assert!(matches!(
        runner
            .run_source("return sort([[\"b\", 2], [\"a\", 3], [\"a\", 1]])")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. }
            if value == Value::list([
                Value::list([Value::string("a"), Value::int(1).unwrap()]),
                Value::list([Value::string("a"), Value::int(3).unwrap()]),
                Value::list([Value::string("b"), Value::int(2).unwrap()]),
            ])
    ));
    assert!(matches!(
        runner
            .run_source("return words(\"say \\\"hello world\\\" north\\\\ east\")")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. }
            if value == Value::list([
                Value::string("say"),
                Value::string("hello world"),
                Value::string("north east"),
            ])
    ));
    assert!(matches!(
        runner
            .run_source("return string_equal_fold(\"North\", \"north\")")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. } if value == Value::bool(true)
    ));
    assert!(matches!(
        runner
            .run_source("return string_starts_with(\"north\", \"nor\")")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. } if value == Value::bool(true)
    ));
    assert!(matches!(
        runner
            .run_source("return string_contains(\"brass coin\", \"coin\")")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. } if value == Value::bool(true)
    ));
    assert!(matches!(
        runner
            .run_source("return edit_distance(\"coin\", \"coiin\")")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. } if value == Value::int(1).unwrap()
    ));
    assert!(matches!(
        runner
            .run_source("return parse_ordinal(\"twenty-first\")")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. } if value == Value::int(21).unwrap()
    ));
    assert!(matches!(
        runner.run_source("return lower(\"North\")").unwrap().outcome,
        TaskOutcome::Complete { value, .. } if value == Value::string("north")
    ));
    assert!(matches!(
        runner
            .run_source("try\n  return 1 / 0\ncatch E_DIV as err\n  return 42\nend")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. } if value == Value::int(42).unwrap()
    ));
    assert!(matches!(
        runner.run_source("return os_getenv(\"PATH\")").unwrap().outcome,
        TaskOutcome::Complete { value, .. } if value.with_str(|s| !s.is_empty()).unwrap_or(false)
    ));
    assert!(matches!(
        runner
            .run_source("return os_getenv(\"MICA_DEFINITELY_NOT_SET_xyzzy\")")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. } if value == Value::nothing()
    ));
}

#[test]
fn installed_builtin_result_contracts_reach_source_compilation() {
    let mut runner = SourceRunner::new_empty();

    assert!(matches!(
        runner
            .run_source("let value: string = string_slice(\"abc\", 0, 1)\nreturn value")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. } if value == Value::string("a")
    ));
    assert!(matches!(
        runner.run_source("let value: int = string_slice(\"abc\", 0, 1)"),
        Err(SourceTaskError::Compile(CompileError::ValueKindMismatch {
            expected: ValueKind::Int,
            inferred,
            ..
        })) if inferred == "string"
    ));
}

#[test]
fn default_builtin_result_contracts_classify_exact_and_dynamic_calls() {
    let runner = SourceRunner::new_empty();

    for (name, kind) in [
        ("string_slice", ValueKind::String),
        ("string_len", ValueKind::Int),
        ("is_frob", ValueKind::Bool),
        ("project", ValueKind::Relation),
        ("make_identity", ValueKind::Identity),
    ] {
        assert_eq!(
            runner.context.runtime_function_result(name),
            Some(BuiltinResultKind::Exact(kind)),
            "unexpected result contract for {name}"
        );
    }

    for name in [
        "json_decode",
        "from_literal",
        "frob_value",
        "emit",
        "mailbox_send",
        "index_or",
        "parse_ordinal",
        "os_getenv",
        "frob_delegate",
        "actor",
        "principal",
        "openai_chat_completion",
    ] {
        assert_eq!(
            runner.context.runtime_function_result(name),
            Some(BuiltinResultKind::Dynamic),
            "unexpected result contract for {name}"
        );
    }
}

#[test]
fn runner_url_decode_rejects_malformed_components() {
    let mut runner = SourceRunner::new_empty();

    let incomplete = runner
        .run_source("return url_decode_component(\"abc%\")")
        .unwrap_err();
    assert!(format!("{incomplete:?}").contains("incomplete percent escape"));

    let invalid_escape = runner
        .run_source("return url_decode_component(\"abc%xx\")")
        .unwrap_err();
    assert!(format!("{invalid_escape:?}").contains("invalid percent escape"));

    let invalid_utf8 = runner
        .run_source("return url_decode_component(\"%FF\")")
        .unwrap_err();
    assert!(format!("{invalid_utf8:?}").contains("decoded component is not valid UTF-8"));
}

#[test]
fn runner_json_and_dom_primitives_build_wire_values() {
    let mut runner = SourceRunner::new_empty();

    assert!(matches!(
        runner
            .run_source(
                "return json_encode({:message -> \"hello\", :values -> [1, true, nothing]})"
            )
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. }
            if value == Value::string("{\"message\":\"hello\",\"values\":[1,true,null]}")
    ));
    assert!(matches!(
        runner
            .run_source(
                "return json_decode(\"{\\\"message\\\":\\\"hello\\\",\\\"values\\\":[1,true,null]}\")"
            )
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. }
            if value == Value::map([
                (Value::symbol(Symbol::intern("message")), Value::string("hello")),
                (
                    Value::symbol(Symbol::intern("values")),
                    Value::list([Value::int(1).unwrap(), Value::bool(true), Value::nothing()])
                ),
            ])
    ));
    assert!(matches!(
        runner
            .run_source(
                "return json_encode(dom_element(\"button\", {:id -> \"send\", :type -> \"submit\"}, [dom_text(\"Send\")]))"
            )
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. }
            if value == Value::string("{\"attrs\":{\"id\":\"send\",\"type\":\"submit\"},\"children\":[{\"text\":\"Send\"}],\"tag\":\"button\"}")
    ));
    assert!(matches!(
        runner
            .run_source(
                "return dom_html(dom_element(\"button\", {:id -> \"send\", :type -> \"submit\"}, [dom_text(\"Send & go\")]))"
            )
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. }
            if value == Value::string("<button id=\"send\" type=\"submit\">Send &amp; go</button>")
    ));
    assert!(matches!(
        runner
            .run_source("return dom_html(dom_element(\"h4\", {}, [dom_text(\"References\")]))")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. }
            if value == Value::string("<h4>References</h4>")
    ));
    assert!(matches!(
        runner
            .run_source(
                "let label = \"Send & go\"\n\
                 let extra = [dom <span class=\"note\">!</span>]\n\
                 return dom_html(dom <button id=\"send\" type=\"submit\">{label}{@extra}</button>)"
            )
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. }
            if value == Value::string("<button id=\"send\" type=\"submit\">Send &amp; go<span class=\"note\">!</span></button>")
    ));
    let expanded_dom = runner
            .run_source(
                "return dom_html(dom_element(\"img\", {:alt -> \"Logo\", \"aria-describedby\" -> \"caption\", \"data-route\" -> \"home\", :loading -> \"lazy\", :src -> \"/logo.png\"}, []))",
            )
            .unwrap()
            .outcome;
    let TaskOutcome::Complete { value, .. } = expanded_dom else {
        panic!("expanded DOM primitive did not complete");
    };
    let html = value.with_str(str::to_owned).unwrap();
    assert!(html.starts_with("<img "));
    assert!(html.contains("alt=\"Logo\""));
    assert!(html.contains("aria-describedby=\"caption\""));
    assert!(html.contains("data-route=\"home\""));
    assert!(html.contains("loading=\"lazy\""));
    assert!(html.contains("src=\"/logo.png\""));
    assert!(matches!(
        runner
            .run_source(
                "return to_xml(dom_element(\"widget\", {\"data-id\" -> 7}, [dom_text(\"Send & go\")]))"
            )
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. }
            if value == Value::string("<widget data-id=\"7\">Send &amp; go</widget>")
    ));
    assert!(matches!(
        runner
            .run_source(
                "return to_xml([dom_raw(\"<!doctype html>\"), dom_element(\"script\", {:type -> \"module\"}, [dom_raw(\"if (a < b) go();\")])])"
            )
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. }
            if value == Value::string("<!doctype html><script type=\"module\">if (a < b) go();</script>")
    ));
    assert!(matches!(
        runner
            .run_source(
                "return json_encode(from_xml(\"<button id='send' type='submit'>Send</button>\"))"
            )
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. }
            if value == Value::string("{\"attrs\":{\"id\":\"send\",\"type\":\"submit\"},\"children\":[{\"text\":\"Send\"}],\"tag\":\"button\"}")
    ));
    assert!(matches!(
        runner
            .run_source(
                "return to_xml(from_xml(\"<form id='chat-composer'><input id='actor'/><button>Send</button></form>\"))"
            )
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. }
            if value == Value::string("<form id=\"chat-composer\"><input id=\"actor\"></input><button>Send</button></form>")
    ));
    assert!(matches!(
        runner
            .run_source(
                "return json_encode(dom_diff(dom_text(\"old\"), dom_text(\"new\")))"
            )
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. }
            if value == Value::string("[{\"op\":\"set_text\",\"path\":[],\"text\":\"new\"}]")
    ));
    assert!(matches!(
        runner
            .run_source(
                "return json_encode(dom_diff(dom_element(\"ul\", {}, []), dom_element(\"ul\", {:id -> \"messages\"}, [dom_text(\"hi\")])))"
            )
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. }
            if value == Value::string("[{\"name\":\"id\",\"op\":\"set_attr\",\"path\":[],\"value\":\"messages\"},{\"node\":{\"text\":\"hi\"},\"op\":\"append_child\",\"path\":[]}]")
    ));
}

#[test]
fn runner_string_filein_installs_primitive_prototype_verbs() {
    let mut runner = SourceRunner::new_empty();
    runner
        .run_filein(include_str!("../../../apps/shared/string.mica"))
        .unwrap();

    assert!(matches!(
        runner.run_source("return trim(\"  hello  \")").unwrap().outcome,
        TaskOutcome::Complete { value, .. } if value == Value::string("hello")
    ));
    assert!(matches!(
        runner.run_source("return split(\"a  b\")").unwrap().outcome,
        TaskOutcome::Complete { value, .. }
            if value == Value::list([Value::string("a"), Value::string("b")])
    ));
    assert!(matches!(
        runner
            .run_source("return join([\"a\", \"b\"], \"-\")")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. } if value == Value::string("a-b")
    ));
    assert!(matches!(
        runner
            .run_source("return strip_prefix(\"north\", \"no\")")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. } if value == Value::string("rth")
    ));
    assert!(matches!(
        runner
            .run_source(
                "try
                   return join([\"a\", 2], \"-\")
                 catch E_TYPE as err
                   return [err.message, err.value]
                 end"
            )
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. }
            if value == Value::list([
                Value::string("binding `part` requires string, got int"),
                Value::int(2).unwrap(),
            ])
    ));
}

#[test]
fn runner_chat_filein_enforces_value_kind_contracts() {
    let mut runner = SourceRunner::new_empty();
    runner
        .run_filein(include_str!("../../../apps/shared/sync-host.mica"))
        .unwrap();
    runner
        .run_filein(include_str!("../../../apps/chat/sync.mica"))
        .unwrap();
    runner.run_source("make_identity(:chat_endpoint)").unwrap();

    assert!(matches!(
        runner
            .run_source("return chat_room_revision(1)")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. } if value == Value::int(1).unwrap()
    ));
    assert!(matches!(
        runner
            .run_source("return sync_view_dependencies(11)")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. } if value.kind() == ValueKind::List
    ));
    assert!(matches!(
        runner
            .run_source("return sync_view_tree(11)")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. } if value.kind() == ValueKind::Map
    ));
    assert!(matches!(
        runner
            .run_source(
                "return sync_event(#chat_endpoint, 7, 11, \"submit\", \"chat-composer\", \"chat_post\", {:actor -> \"bob\", :text -> \"hello\"})"
            )
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. } if value == Value::bool(true)
    ));
    assert!(matches!(
        runner.run_source("return chat_room_revision(1)").unwrap().outcome,
        TaskOutcome::Complete { value, .. } if value == Value::int(2).unwrap()
    ));
    assert!(matches!(
        runner
            .run_source(
                "try
                   return sync_event(#chat_endpoint, 7, 11, \"submit\", \"chat-composer\", \"chat_post\", {:actor -> 7, :text -> \"hello\"})
                 catch E_TYPE as err
                   return [err.message, err.value]
                 end"
            )
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. }
            if value == Value::list([
                Value::string("parameter `actor` requires string, got int"),
                Value::int(7).unwrap(),
            ])
    ));
}

#[test]
fn runner_frob_builtins_construct_and_inspect_values() {
    let mut runner = SourceRunner::new_empty();
    runner.run_source("make_identity(:take_event)").unwrap();

    let delegate = runner.actor_identity(Symbol::intern("take_event")).unwrap();
    let report = runner
        .run_source(
            "let event = frob(#take_event, {:item -> \"coin\"})\n\
                 return [is_frob(event), frob_delegate(event), frob_value(event)[:item]]",
        )
        .unwrap();

    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. }
            if value == Value::list([
                Value::bool(true),
                Value::identity(delegate),
                Value::string("coin"),
            ])
    ));
}

#[test]
fn runner_frob_literals_compile_to_frob_values() {
    let mut runner = SourceRunner::new_empty();
    runner.run_source("make_identity(:take_event)").unwrap();

    let report = runner
        .run_source("return frob_value(#take_event<{:item -> \"coin\"}>)[:item]")
        .unwrap();

    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::string("coin")
    ));
}

#[test]
fn runner_empty_relation_results_are_falsey() {
    let mut runner = SourceRunner::new_empty();
    runner
        .run_source("make_relation(:Seen, 1)\nmake_identity(:missing)\nmake_identity(:present)")
        .unwrap();

    let report = runner
            .run_source(
                "let empty_list_branch = false\n\
                 if []\n\
                   empty_list_branch = true\n\
                 end\n\
                 let non_empty_list_branch = false\n\
                 if [nothing]\n\
                   non_empty_list_branch = true\n\
                 end\n\
                 let empty_relation_branch = false\n\
                 if Seen(#missing)\n\
                   empty_relation_branch = true\n\
                 end\n\
                 assert Seen(#present)\n\
                 let non_empty_relation_branch = false\n\
                 if Seen(#present)\n\
                   non_empty_relation_branch = true\n\
                 end\n\
                 return [empty_list_branch, non_empty_list_branch, empty_relation_branch, non_empty_relation_branch, not []]",
            )
            .unwrap();

    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. }
            if value == Value::list([
                Value::bool(false),
                Value::bool(true),
                Value::bool(false),
                Value::bool(true),
                Value::bool(true),
            ])
    ));
}

#[test]
fn runner_to_literal_renders_parseable_value_source() {
    let mut runner = SourceRunner::new_empty();
    runner.run_source("make_identity(:take_event)").unwrap();

    assert!(matches!(
        runner
            .run_source("return to_literal([nothing, true, 42, \"x\", :foo])")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. }
            if value == Value::string("[nothing, true, 42, \"x\", :foo]")
    ));
    assert!(matches!(
        runner
            .run_source("return to_literal(#take_event<[\"coin\"]>)")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. } if value == Value::string("#take_event<[\"coin\"]>")
    ));
    assert!(matches!(
        runner.run_source("return to_literal(b\"3q2-7w==\")").unwrap().outcome,
        TaskOutcome::Complete { value, .. } if value == Value::string("b\"3q2-7w==\"")
    ));
    assert!(matches!(
        runner
            .run_source("return to_literal([:thing] { [2], [1], [1] })")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. } if value == Value::string("[:thing] {[1], [2]}")
    ));
    assert!(matches!(
        runner
            .run_source("return to_literal([:world/thing] { [1] })")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. }
            if value == Value::string("[:world/thing] {[1]}")
    ));
}

#[test]
fn runner_from_literal_parses_to_literal_output() {
    let mut runner = SourceRunner::new_empty();
    runner.run_source("make_identity(:take_event)").unwrap();

    assert!(matches!(
        runner
            .run_source(
                "let value = [nothing, true, -42, \"x\", b\"3q2-7w==\", :foo, #take_event, {:a -> 1}, 2.._]
                 return from_literal(to_literal(value)) == value"
            )
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. } if value == Value::bool(true)
    ));
    assert!(matches!(
        runner
            .run_source(
                "let value = [:outer] { [[:inner] { [1] }] }
                 return from_literal(to_literal(value)) == value"
            )
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. } if value == Value::bool(true)
    ));
    assert!(matches!(
        runner
            .run_source("return from_literal(to_literal(#take_event<[\"coin\"]>))")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. }
            if value == Value::frob(
                runner.named_identity(Symbol::intern("take_event")).unwrap(),
                Value::list([Value::string("coin")])
            )
    ));
    assert!(matches!(
        runner
            .run_source(
                "let value = [:thing, :count] { [#take_event, 2], [#take_event, 1] }
                 return from_literal(to_literal(value)) == value"
            )
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. } if value == Value::bool(true)
    ));
    assert!(matches!(
        runner
            .run_source("try\n  return 1 / 0\ncatch E_DIV as err\n  return 42\nend")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. } if value == Value::int(42).unwrap()
    ));
    assert!(matches!(
        runner
            .run_source("try\n  return 1 / 0\ncatch any_err\n  return 7\nend")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. } if value == Value::int(7).unwrap()
    ));
}

#[test]
fn annotated_bindings_raise_catchable_type_errors_at_dynamic_writes() {
    let mut runner = SourceRunner::new_empty();
    let report = runner
        .run_source(
            "let count: int = 0
             let update = fn(value)
               count = value
               return count
             end
             try
               return update(\"wrong\")
             catch E_TYPE as err
               return err.value
             end",
        )
        .unwrap();

    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::string("wrong")
    ));
}

#[test]
fn annotated_parameters_raise_catchable_type_errors_at_each_call_boundary() {
    let mut runner = SourceRunner::new_empty();
    for source in [
        "fn accept(value: int) -> int => value
         try
           return accept(from_literal(\"\\\"wrong\\\"\"))
         catch E_TYPE as err
           return [err.message, err.value]
         end",
        "let offset: int = 0
         let typed = fn(value: int) -> int => value + offset
         let accept = typed
         try
           return accept(from_literal(\"\\\"wrong\\\"\"))
         catch E_TYPE as err
           return [err.message, err.value]
         end",
    ] {
        let report = runner.run_source(source).unwrap();
        assert!(matches!(
            report.outcome,
            TaskOutcome::Complete { value, .. }
                if value == Value::list([
                    Value::string("parameter `value` requires int, got string"),
                    Value::string("wrong"),
                ])
        ));
    }
}

#[test]
fn annotated_optional_and_rest_parameters_bind_function_value_arguments() {
    let mut runner = SourceRunner::new_empty();
    let report = runner
        .run_source(
            "let typed = fn(?value: int = 1, @rest: list) -> list => [value, @rest]
             let alias = typed
             return [alias(), alias(2, 3, 4)]",
        )
        .unwrap();

    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. }
            if value == Value::list([
                Value::list([Value::int(1).unwrap()]),
                Value::list([
                    Value::int(2).unwrap(),
                    Value::int(3).unwrap(),
                    Value::int(4).unwrap(),
                ]),
            ])
    ));
}

#[test]
fn annotated_loop_bindings_preserve_collection_iteration_shapes() {
    let mut runner = SourceRunner::new_empty();
    let report = runner
        .run_source(
            "let rows = [:item] { [7], [9] }
             let total: int = 0
             for row: map in rows
               total = total + row[:item]
             end
             for index: int, row: map in rows
               total = total + index + row[:item]
             end
             return total",
        )
        .unwrap();

    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::int(33).unwrap()
    ));
}

#[test]
fn annotated_collection_bindings_raise_catchable_type_errors() {
    let mut runner = SourceRunner::new_empty();
    for (source, subject) in [
        (
            "try
               for value: int in [1, \"wrong\"]
               end
             catch E_TYPE as err
               return [err.message, err.value]
             end",
            "value",
        ),
        (
            "try
               let [head: int] = [\"wrong\"]
             catch E_TYPE as err
               return [err.message, err.value]
             end",
            "head",
        ),
    ] {
        let report = runner.run_source(source).unwrap();
        assert!(matches!(
            report.outcome,
            TaskOutcome::Complete { value, .. }
                if value == Value::list([
                    Value::string(format!(
                        "binding `{subject}` requires int, got string"
                    )),
                    Value::string("wrong"),
                ])
        ));
    }
}

#[test]
fn annotated_scatter_bindings_handle_defaults_and_rest_values() {
    let mut runner = SourceRunner::new_empty();
    let report = runner
        .run_source(
            "let [head: int, ?middle: int = 2, @tail: list] = [1]
             return [head, middle, tail]",
        )
        .unwrap();

    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. }
            if value == Value::list([
                Value::int(1).unwrap(),
                Value::int(2).unwrap(),
                Value::list([]),
            ])
    ));
}

#[test]
fn installed_verb_annotations_check_after_dispatch_without_fallback() {
    let mut runner = SourceRunner::new_empty();
    runner
        .run_source(
            "verb typed(value)\n\
               return \"fallback\"\n\
             end\n\
             verb typed(value @ #string: int)\n\
               return \"specific\"\n\
             end",
        )
        .unwrap();

    let report = runner
        .run_source(
            "try\n\
               return :typed(value: \"wrong\")\n\
             catch E_TYPE as err\n\
               return [err.message, err.value]\n\
             end",
        )
        .unwrap();
    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. }
            if value == Value::list([
                Value::string("parameter `value` requires int, got string"),
                Value::string("wrong"),
            ])
    ));

    let report = runner.run_source("return :typed(value: 7)").unwrap();
    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::string("fallback")
    ));
}

#[test]
fn installed_verb_annotations_preserve_primitive_restriction_facts() {
    let mut runner = SourceRunner::new_empty();
    runner
        .run_source(
            "verb echo_text(value @ #string: string) -> string\n\
               return value\n\
             end\n\
             verb echo_identity(value @ #identity: identity) -> identity\n\
               return value\n\
             end",
        )
        .unwrap();

    let rows = runner
        .task_manager
        .kernel()
        .snapshot()
        .scan(
            param_relation(),
            &[
                None,
                Some(Value::symbol(Symbol::intern("value"))),
                None,
                None,
            ],
        )
        .unwrap();
    let string = Value::identity(runner.context.identity("string").unwrap());
    let identity = Value::identity(runner.context.identity("identity").unwrap());
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().any(|row| row.values()[2] == string));
    assert!(rows.iter().any(|row| row.values()[2] == identity));

    runner.run_source("make_identity(:typed_target)").unwrap();
    let text = runner
        .run_source("return :echo_text(value: \"hello\")")
        .unwrap();
    assert!(matches!(
        text.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::string("hello")
    ));
    let identity = runner
        .run_source("return :echo_identity(value: #typed_target)")
        .unwrap();
    assert!(matches!(
        identity.outcome,
        TaskOutcome::Complete { value, .. } if value.as_identity().is_some()
    ));
}

#[test]
fn annotated_relation_bindings_accept_dynamic_immediate_and_heap_values() {
    let mut runner = SourceRunner::new_empty();

    assert!(matches!(
        runner
            .run_source("let rows: relation = from_literal(\"nothing\")\nreturn rows")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. } if value == Value::nothing()
    ));
    assert!(matches!(
        runner
            .run_source(
                "let rows: relation = from_literal(\"[:item] {[1]}\")\nreturn rows"
            )
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. }
            if value == query_relation(["item"], [[Value::int(1).unwrap()]])
    ));
}

#[test]
fn annotated_integer_bindings_check_division_result_kinds() {
    let mut runner = SourceRunner::new_empty();

    assert!(matches!(
        runner
            .run_source("let quotient: int = 4 / 2\nreturn quotient")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. } if value == Value::int(2).unwrap()
    ));
    assert!(matches!(
        runner
            .run_source(
                "try
                   let quotient: int = 3 / 2
                   return quotient
                 catch E_TYPE as err
                   return err.value
                 end"
            )
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. } if value == Value::float(1.5).unwrap()
    ));
}

#[test]
fn annotated_function_results_are_proven_before_execution() {
    let mut runner = SourceRunner::new_empty();

    assert!(matches!(
        runner
            .run_source("fn answer() -> int => 42\nreturn answer()")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. } if value == Value::int(42).unwrap()
    ));
    assert!(matches!(
        runner
            .run_source("fn wrong() -> int => 1.0\nreturn wrong()")
            .unwrap_err(),
        SourceTaskError::Compile(CompileError::FunctionResultKindMismatch {
            expected: ValueKind::Int,
            inferred,
            ..
        }) if inferred == "float"
    ));
}

#[test]
fn relation_literals_construct_dynamic_empty_and_unit_relations() {
    let mut runner = SourceRunner::new_empty();
    let report = runner
        .run_source(
            "let count = 1
             return [[:thing, :count] { [7, count + 1], [7, count + 1] }, nothing == [] {}, [] {} == [] {[]}, not (not [] {}), not (not [] {[]})]",
        )
        .unwrap();

    let relation = query_relation(
        ["thing", "count"],
        [[Value::int(7).unwrap(), Value::int(2).unwrap()]],
    );
    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. }
            if value == Value::list([
                relation,
                Value::bool(true),
                Value::bool(false),
                Value::bool(false),
                Value::bool(true),
            ])
    ));
}

#[test]
fn missing_or_invalid_indexes_raise_catchable_errors() {
    let mut runner = SourceRunner::new_empty();
    for source in [
        "return [1][1]",
        "return {:present -> 1}[:missing]",
        "return [:value] { [1] }[1]",
        "return [1][-1]",
        "return 1[0]",
    ] {
        let report = runner
            .run_source(&format!(
                "try\n  {source}\ncatch E_INDEX as err\n  return err.value\nend"
            ))
            .unwrap();
        assert!(matches!(
            report.outcome,
            TaskOutcome::Complete { value, .. }
                if value.with_list(|values| values.len() == 2) == Some(true)
        ));
    }
}

#[test]
fn invalid_indexed_assignment_raises_a_catchable_error() {
    let mut runner = SourceRunner::new_empty();
    let report = runner
        .run_source(
            "let values = [1]
             try
               values[1] = 2
             catch E_INDEX as err
               return err.code
             end
             return false",
        )
        .unwrap();
    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. }
            if value == Value::error_code(Symbol::intern("E_INDEX"))
    ));
}

#[test]
fn index_or_makes_optional_lookup_explicit() {
    let mut runner = SourceRunner::new_empty();
    let report = runner
        .run_source(
            "return [index_or([1], 4, 9), index_or({:a -> 1}, :b, 9), index_or([:a] { [1] }, 4, 9)]",
        )
        .unwrap();
    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. }
            if value == Value::list([
                Value::int(9).unwrap(),
                Value::int(9).unwrap(),
                Value::int(9).unwrap(),
            ])
    ));
}

#[test]
fn runner_to_symbol_converts_strings_and_keeps_symbols() {
    let mut runner = SourceRunner::new_empty();

    assert!(matches!(
        runner
            .run_source("return [to_symbol(\"AgentProposal\"), to_symbol(:SourceFile)]")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. }
            if value == Value::list([
                Value::symbol(Symbol::intern("AgentProposal")),
                Value::symbol(Symbol::intern("SourceFile")),
            ])
    ));
}

#[test]
fn runner_dispatches_frobs_by_delegate_restriction() {
    let mut runner = SourceRunner::new_empty();
    runner
        .run_filein(
            "make_identity(:event)\n\
                 make_identity(:take_event)\n\
                 make_relation(:Delegates, 3)\n\
                 assert Delegates(#take_event, #event, 0)\n\
                 verb render(event @ #event<_>)\n\
                   return frob_value(event)[:item]\n\
                 end\n",
        )
        .unwrap();

    let report = runner
        .run_source("return :render(event: #take_event<{:item -> \"coin\"}>)")
        .unwrap();

    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::string("coin")
    ));
}

#[test]
fn runner_dispatch_selects_most_specific_method() {
    let mut runner = SourceRunner::new_empty();
    runner
        .run_filein(
            "make_identity(:event)\n\
                 make_identity(:take_event)\n\
                 make_relation(:Delegates, 3)\n\
                 assert Delegates(#take_event, #event, 0)\n\
                 verb render(event)\n\
                   return \"fallback\"\n\
                 end\n\
                 verb render(event @ #event<_>)\n\
                   return \"event\"\n\
                 end\n\
                 verb render(event @ #take_event<_>)\n\
                   return \"take\"\n\
                 end\n",
        )
        .unwrap();

    let report = runner
        .run_source("return :render(event: #take_event<\"payload\">)")
        .unwrap();

    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::string("take")
    ));
}

#[test]
fn runner_openai_filein_installs_chat_helpers() {
    let mut runner = SourceRunner::new_empty();
    runner
        .run_filein(include_str!("../../../apps/shared/openai.mica"))
        .unwrap();

    assert!(matches!(
        runner
            .run_source("return openai/user_message(\"ping\")")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. }
            if value
                .map_get(&Value::symbol(Symbol::intern("role")))
                == Some(Value::string("user"))
                && value
                    .map_get(&Value::symbol(Symbol::intern("content")))
                    == Some(Value::string("ping"))
    ));
    assert!(matches!(
        runner
            .run_source(
                "return openai/assistant_text({:choices -> [{:message -> {:content -> \"pong\"}}]})"
            )
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. } if value == Value::string("pong")
    ));
}

#[test]
fn runner_agent_core_resolves_default_model_with_env_override() {
    let _env_guard = env_lock().lock().unwrap();
    let mut runner = SourceRunner::new_empty();
    runner
        .run_filein(include_str!("../../../apps/shared/llm.mica"))
        .unwrap();
    runner
        .run_filein(include_str!("../../../apps/agent/core.mica"))
        .unwrap();

    assert!(matches!(
        runner
            .run_source("return agent/resolve_model(#agent/default)")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. } if value == Value::string("deepseek/deepseek-v4-pro")
    ));

    assert!(matches!(
        runner
            .run_source(
                "let a = frob(#agent, [\"override-agent\"])\n\
                 a.agentModel = \"anthropic/claude-test\"\n\
                 return agent/resolve_model(a)"
            )
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. } if value == Value::string("anthropic/claude-test")
    ));

    let prior = std::env::var_os("MICA_AGENT_MODEL");
    unsafe {
        std::env::set_var("MICA_AGENT_MODEL", "google/gemini-env-override");
    }
    let result = runner
        .run_source(
            "let a = frob(#agent, [\"no-override\"])\n\
             return agent/resolve_model(a)",
        )
        .unwrap();
    match prior {
        Some(value) => unsafe { std::env::set_var("MICA_AGENT_MODEL", value) },
        None => unsafe { std::env::remove_var("MICA_AGENT_MODEL") },
    }
    assert!(matches!(
        result.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::string("google/gemini-env-override")
    ));
}

#[test]
fn runner_agent_llm_messages_with_tools_maps_transcript_with_tool_calls() {
    let _env_guard = env_lock().lock().unwrap();
    let mut runner = SourceRunner::new_empty();
    runner
        .run_filein(include_str!("../../../apps/shared/llm.mica"))
        .unwrap();
    runner
        .run_filein(include_str!("../../../apps/agent/core.mica"))
        .unwrap();
    let prior_root = std::env::var_os("MICA_SOURCE_ROOT");
    unsafe {
        std::env::set_var("MICA_SOURCE_ROOT", "/tmp/agent-llm-messages-test");
    }
    runner
        .run_filein(include_str!("../../../apps/agent/workspaces.mica"))
        .unwrap();
    match prior_root {
        Some(value) => unsafe { std::env::set_var("MICA_SOURCE_ROOT", value) },
        None => unsafe { std::env::remove_var("MICA_SOURCE_ROOT") },
    }
    runner
        .run_filein(include_str!("../../../apps/agent/tools.mica"))
        .unwrap();

    let result = runner
        .run_source(
            "let ws = frob(#workspace, [\"test-workspace\"])\n\
             let t = agent/ensure_transcript(#agent/default, ws)\n\
             let m1 = frob(#event/user_message, [t, 0])\n\
             assert Message(m1)\n\
             assert MessageTranscript(m1, t)\n\
             assert MessageSeq(m1, 0)\n\
             assert MessageRole(m1, \"user\")\n\
             assert MessageContent(m1, \"hello there\")\n\
             let m2 = frob(#event/assistant_message, [t, 1])\n\
             assert Message(m2)\n\
             assert MessageTranscript(m2, t)\n\
             assert MessageSeq(m2, 1)\n\
             assert MessageRole(m2, \"assistant\")\n\
             assert MessageContent(m2, \"\")\n\
             let call = frob(#tool_call, [m2, \"call_1\"])\n\
             assert ToolCall(call)\n\
             assert ToolCallMessage(call, m2)\n\
             assert ToolCallId(call, \"call_1\")\n\
             assert ToolCallName(call, \"read\")\n\
             assert ToolCallArguments(call, \"{\\\"path\\\":\\\"foo\\\"}\")\n\
             assert ToolCallStatus(call, \"complete\")\n\
             let result = frob(#tool_result, [call])\n\
             assert ToolResult(result)\n\
             assert ToolResultCall(result, call)\n\
             assert ToolResultContent(result, \"file contents\")\n\
             assert ToolResultIsError(result, false)\n\
             assert ToolResultDetails(result, \"{:status -> \\\"complete\\\"}\")\n\
             let m3 = frob(#event/tool_message, [t, 2])\n\
             assert Message(m3)\n\
             assert MessageTranscript(m3, t)\n\
             assert MessageSeq(m3, 2)\n\
             assert MessageRole(m3, \"tool\")\n\
             assert MessageContent(m3, \"{:tool_call_id -> \\\"call_1\\\", :content -> \\\"file contents\\\"}\")\n\
             let m4 = frob(#event/assistant_message, [t, 3])\n\
             assert Message(m4)\n\
             assert MessageTranscript(m4, t)\n\
             assert MessageSeq(m4, 3)\n\
             assert MessageRole(m4, \"assistant\")\n\
             assert MessageContent(m4, \"done\")\n\
             return agent/llm_messages_with_tools(t)",
        )
        .unwrap();
    let value = match result.outcome {
        TaskOutcome::Complete { value, .. } => value,
        other => panic!("expected complete, got {other:?}"),
    };
    let items = value
        .with_list(|items| items.to_vec())
        .expect("llm_messages_with_tools should return a list");
    assert_eq!(
        items.len(),
        5,
        "system + user + assistant+tool_calls + tool + assistant"
    );
    let system = items[0]
        .map_get(&Value::symbol(Symbol::intern("role")))
        .and_then(|v| v.with_str(str::to_owned))
        .unwrap_or_default();
    assert_eq!(system, "system");
    let assistant_with_calls = &items[2];
    let calls = assistant_with_calls
        .map_get(&Value::symbol(Symbol::intern("tool_calls")))
        .and_then(|v| v.with_list(<[Value]>::to_vec))
        .expect("assistant with tool calls should have :tool_calls");
    assert_eq!(calls.len(), 1);
    let tool_message = &items[3];
    assert_eq!(
        tool_message
            .map_get(&Value::symbol(Symbol::intern("role")))
            .and_then(|v| v.with_str(str::to_owned))
            .unwrap_or_default(),
        "tool"
    );
    assert_eq!(
        tool_message
            .map_get(&Value::symbol(Symbol::intern("tool_call_id")))
            .and_then(|v| v.with_str(str::to_owned))
            .unwrap_or_default(),
        "call_1"
    );
    assert_eq!(
        tool_message
            .map_get(&Value::symbol(Symbol::intern("content")))
            .and_then(|v| v.with_str(str::to_owned))
            .unwrap_or_default(),
        "file contents"
    );

    assert!(matches!(
        runner
            .run_source(
                "let entries = 0\n\
                 let calls = 0\n\
                 let results = 0\n\
                 for found in TranscriptEntry(#agent/default, ?transcript, ?seq, ?message, ?role, ?content)\n\
                   entries = entries + 1\n\
                 end\n\
                 for found in TranscriptToolCall(#agent/default, ?message, ?call, ?call_id, ?name, ?arguments, ?status)\n\
                   calls = calls + 1\n\
                 end\n\
                 for found in TranscriptToolResult(#agent/default, ?call, ?result, ?content, ?is_error, ?details)\n\
                   results = results + 1\n\
                 end\n\
                 return [entries, calls, results]"
            )
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. }
            if value == Value::list([
                Value::int(4).unwrap(),
                Value::int(1).unwrap(),
                Value::int(1).unwrap(),
            ])
    ));
}

#[test]
fn runner_agent_max_tool_rounds_reads_agent_relation() {
    let _env_guard = env_lock().lock().unwrap();
    let mut runner = SourceRunner::new_empty();
    runner
        .run_filein(include_str!("../../../apps/agent/core.mica"))
        .unwrap();
    let prior_root = std::env::var_os("MICA_SOURCE_ROOT");
    unsafe {
        std::env::set_var("MICA_SOURCE_ROOT", "/tmp/agent-max-rounds-test");
    }
    runner
        .run_filein(include_str!("../../../apps/agent/workspaces.mica"))
        .unwrap();
    match prior_root {
        Some(value) => unsafe { std::env::set_var("MICA_SOURCE_ROOT", value) },
        None => unsafe { std::env::remove_var("MICA_SOURCE_ROOT") },
    }
    runner
        .run_filein(include_str!("../../../apps/agent/tools.mica"))
        .unwrap();

    let result = runner
        .run_source("return agent/max_tool_rounds(#agent/default)")
        .unwrap();
    let value = match result.outcome {
        TaskOutcome::Complete { value, .. } => value,
        other => panic!("expected complete, got {other:?}"),
    };
    let rounds = value
        .as_int()
        .expect("max_tool_rounds should return an int");
    assert_eq!(rounds, 4);
}

#[test]
fn runner_agent_workspaces_binds_default_agent_to_source_root() {
    let _env_guard = env_lock().lock().unwrap();
    let mut runner = SourceRunner::new_empty();
    runner
        .run_filein(include_str!("../../../apps/agent/core.mica"))
        .unwrap();

    let prior = std::env::var_os("MICA_SOURCE_ROOT");
    unsafe {
        std::env::set_var("MICA_SOURCE_ROOT", "/tmp/agent-test-root");
    }
    runner
        .run_filein(include_str!("../../../apps/agent/workspaces.mica"))
        .unwrap();
    match prior {
        Some(value) => unsafe { std::env::set_var("MICA_SOURCE_ROOT", value) },
        None => unsafe { std::env::remove_var("MICA_SOURCE_ROOT") },
    }

    let workspace = runner
        .named_identity(Symbol::intern("workspace/default"))
        .expect("workspace/default identity should exist");
    assert!(matches!(
        runner
            .run_source("return workspace/active(#agent/default)")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. } if value == Value::identity(workspace)
    ));
    assert!(matches!(
        runner
            .run_source("return one WorkspaceRoot(workspace/active(#agent/default), ?root)")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. } if value == Value::string("/tmp/agent-test-root")
    ));
    assert!(matches!(
        runner
            .run_source("return agent/transcript(#agent/default)")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. } if value == Value::nothing()
    ));
}

#[test]
fn runner_llm_filein_installs_chat_helpers() {
    let mut runner = SourceRunner::new_empty();
    runner
        .run_filein(include_str!("../../../apps/shared/llm.mica"))
        .unwrap();

    assert!(matches!(
        runner
            .run_source("return llm/user_message(\"hello\")")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. }
        if value
                .map_get(&Value::symbol(Symbol::intern("role")))
                == Some(Value::string("user"))
                && value
                    .map_get(&Value::symbol(Symbol::intern("content")))
                    == Some(Value::string("hello"))
    ));
    assert!(matches!(
        runner
            .run_source("return llm/system_message(\"be helpful\")")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. }
        if value
                .map_get(&Value::symbol(Symbol::intern("role")))
                == Some(Value::string("system"))
                && value
                    .map_get(&Value::symbol(Symbol::intern("content")))
                    == Some(Value::string("be helpful"))
    ));
    assert!(matches!(
        runner
            .run_source("return llm/tool_message(\"call_1\", \"result\")")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. }
        if value
                .map_get(&Value::symbol(Symbol::intern("role")))
                == Some(Value::string("tool"))
                && value
                    .map_get(&Value::symbol(Symbol::intern("tool_call_id")))
                    == Some(Value::string("call_1"))
    ));
    assert!(matches!(
        runner
            .run_source("return map_pairs({:a -> 1, :b -> 2})")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. }
        if value
                .with_list(|items| {
                    items.len() == 2
                        && items.iter().all(|pair| pair.with_list(|pair| {
                            pair.len() == 2
                                && (pair[0] == Value::symbol(Symbol::intern("a"))
                                    && pair[1] == Value::int(1).unwrap()
                                    || pair[0] == Value::symbol(Symbol::intern("b"))
                                        && pair[1] == Value::int(2).unwrap())
                        }).unwrap_or(false))
                })
                .unwrap_or(false)
    ));
}

#[test]
fn runner_event_substitution_filein_renders_per_viewer() {
    let mut runner = SourceRunner::new_empty();
    runner
        .run_filein(include_str!("../../../apps/shared/string.mica"))
        .unwrap();
    runner
        .run_filein(include_str!("../../../apps/shared/events.mica"))
        .unwrap();
    runner
        .run_filein(include_str!("../../../apps/mud/core.mica"))
        .unwrap();
    runner
        .run_filein(include_str!("../../../apps/mud/event-substitutions.mica"))
        .unwrap();
    runner
        .run_filein(include_str!("../../../apps/mud/tests/event-scenarios.mica"))
        .unwrap();

    let report = runner
        .run_source("return test/event_substitutions_render_per_viewer()")
        .unwrap();
    assert!(
        matches!(report.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::bool(true)),
        "{}",
        report.render()
    );

    let report = runner
        .run_source("return test/event_delivery_replaces_group_for_listener()")
        .unwrap();
    assert!(
        matches!(report.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::bool(true)),
        "{}",
        report.render()
    );
}

#[test]
fn runner_submit_source_as_exposes_context_and_drains_emissions() {
    let mut runner = SourceRunner::new_empty();
    runner.run_source("make_relation(:GrantEffect, 1)").unwrap();
    runner.run_source("make_identity(:alice)").unwrap();
    runner.run_source("assert GrantEffect(#alice)").unwrap();
    let actor = runner.actor_identity(Symbol::intern("alice")).unwrap();
    let endpoint = Identity::new(0x00ee_0000_0000_0001).unwrap();

    let submitted = runner
        .submit_source_as(actor, endpoint, "emit(#endpoint, \"hello\")")
        .unwrap();

    assert!(matches!(
        submitted.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::string("hello")
    ));
    let emissions = runner.drain_emissions();
    assert_eq!(emissions.len(), 1);
    assert_eq!(emissions[0].task_id, submitted.task_id);
    assert_eq!(emissions[0].target, endpoint);
    assert_eq!(emissions[0].value, Value::string("hello"));
    assert!(runner.drain_emissions().is_empty());
}

#[test]
fn runner_submit_invocation_as_adds_actor_and_endpoint_roles() {
    let mut runner = SourceRunner::new_empty();
    runner
        .run_filein(include_str!("../../../apps/shared/capabilities.mica"))
        .unwrap();
    let actor = runner.actor_identity(Symbol::intern("alice")).unwrap();
    let endpoint = Identity::new(0x00ee_0000_0000_0002).unwrap();
    let lamp = runner.actor_identity(Symbol::intern("lamp")).unwrap();

    let submitted = runner
        .submit_invocation_as(
            actor,
            endpoint,
            Symbol::intern("polish"),
            vec![(Symbol::intern("item"), Value::identity(lamp))],
        )
        .unwrap();

    assert!(matches!(
        submitted.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::string("polished brass lamp")
    ));
    let emissions = runner.drain_emissions();
    assert_eq!(emissions.len(), 1);
    assert_eq!(emissions[0].task_id, submitted.task_id);
    assert_eq!(emissions[0].target, actor);
}

#[test]
fn runner_persisted_method_can_spawn_child_invocation() {
    let mut runner = SourceRunner::new_empty();
    runner
        .run_filein(
            "verb parent(endpoint)\n\
                   let child = spawn :child(endpoint: endpoint) after 0\n\
                   return child\n\
                 end\n\
                 verb child(endpoint)\n\
                   return endpoint\n\
                 end\n",
        )
        .unwrap();

    let report = runner
        .run_source("return :parent(endpoint: endpoint())")
        .unwrap();

    assert!(matches!(
        report.outcome,
        TaskOutcome::Suspended {
            kind: SuspendKind::Spawn(SpawnRequest {
                selector,
                delay_millis: Some(0),
                ..
            }),
            ..
        } if selector == Symbol::intern("child")
    ));
}

#[test]
fn runner_can_spawn_receiver_positional_invocation() {
    let mut runner = SourceRunner::new_empty();
    let coin = runner.run_source("return make_identity(:coin)").unwrap();
    let TaskOutcome::Complete { value: coin, .. } = coin.outcome else {
        panic!("expected coin identity creation to complete");
    };
    let alice = runner.run_source("return make_identity(:alice)").unwrap();
    let TaskOutcome::Complete { value: alice, .. } = alice.outcome else {
        panic!("expected alice identity creation to complete");
    };
    runner
        .run_filein(
            "verb parent()\n\
                   let child = spawn #coin:inspect(#alice) after 0\n\
                   return child\n\
                 end\n\
                 verb inspect(receiver, actor)\n\
                   return [receiver, actor]\n\
                 end\n",
        )
        .unwrap();

    let report = runner.run_source("return :parent()").unwrap();

    assert!(matches!(
        report.outcome,
        TaskOutcome::Suspended {
            kind: SuspendKind::Spawn(SpawnRequest {
                selector,
                target: SpawnTarget::PositionalArgs(args),
                delay_millis: Some(0),
            }),
            ..
        } if selector == Symbol::intern("inspect") && args == vec![coin, alice]
    ));
}

#[test]
fn runner_can_spawn_positional_invocation_with_argument_splices() {
    let mut runner = SourceRunner::new_empty();
    let coin = runner.run_source("return make_identity(:coin)").unwrap();
    let TaskOutcome::Complete { value: coin, .. } = coin.outcome else {
        panic!("expected coin identity creation to complete");
    };
    let alice = runner.run_source("return make_identity(:alice)").unwrap();
    let TaskOutcome::Complete { value: alice, .. } = alice.outcome else {
        panic!("expected alice identity creation to complete");
    };
    runner
        .run_filein(
            "verb parent()\n\
                   let args = [#coin]\n\
                   let child = spawn :inspect(#alice, @args) after 0.5\n\
                   return child\n\
                 end\n\
                 verb inspect(actor, item)\n\
                   return [actor, item]\n\
                 end\n",
        )
        .unwrap();

    let report = runner.run_source("return :parent()").unwrap();

    assert!(matches!(
        report.outcome,
        TaskOutcome::Suspended {
            kind: SuspendKind::Spawn(SpawnRequest {
                selector,
                target: SpawnTarget::PositionalArgs(args),
                delay_millis: Some(500),
            }),
            ..
        } if selector == Symbol::intern("inspect") && args == vec![alice, coin]
    ));
}

#[test]
fn runner_can_spawn_named_invocation_with_argument_splices() {
    let mut runner = SourceRunner::new_empty();
    let coin = runner.run_source("return make_identity(:coin)").unwrap();
    let TaskOutcome::Complete { value: coin, .. } = coin.outcome else {
        panic!("expected coin identity creation to complete");
    };
    let alice = runner.run_source("return make_identity(:alice)").unwrap();
    let TaskOutcome::Complete { value: alice, .. } = alice.outcome else {
        panic!("expected alice identity creation to complete");
    };
    runner
        .run_filein(
            "verb parent()\n\
                   let roles = {:item -> #coin}\n\
                   let child = spawn :inspect(actor: #alice, @roles) after 0.25\n\
                   return child\n\
                 end\n\
                 verb inspect(actor, item)\n\
                   return [actor, item]\n\
                 end\n",
        )
        .unwrap();

    let report = runner.run_source("return :parent()").unwrap();

    assert!(matches!(
        report.outcome,
        TaskOutcome::Suspended {
            kind: SuspendKind::Spawn(SpawnRequest {
                selector,
                target: SpawnTarget::NamedRoles(roles),
                delay_millis: Some(250),
            }),
            ..
        } if selector == Symbol::intern("inspect")
            && roles == vec![
                (Symbol::intern("actor"), alice),
                (Symbol::intern("item"), coin),
            ]
    ));
}

#[test]
fn runner_can_spawn_receiver_positional_invocation_with_argument_splices() {
    let mut runner = SourceRunner::new_empty();
    let coin = runner.run_source("return make_identity(:coin)").unwrap();
    let TaskOutcome::Complete { value: coin, .. } = coin.outcome else {
        panic!("expected coin identity creation to complete");
    };
    let alice = runner.run_source("return make_identity(:alice)").unwrap();
    let TaskOutcome::Complete { value: alice, .. } = alice.outcome else {
        panic!("expected alice identity creation to complete");
    };
    runner
        .run_filein(
            "verb parent()\n\
                   let args = [#alice]\n\
                   let child = spawn #coin:inspect(@args) after 0\n\
                   return child\n\
                 end\n\
                 verb inspect(receiver, actor)\n\
                   return [receiver, actor]\n\
                 end\n",
        )
        .unwrap();

    let report = runner.run_source("return :parent()").unwrap();

    assert!(matches!(
        report.outcome,
        TaskOutcome::Suspended {
            kind: SuspendKind::Spawn(SpawnRequest {
                selector,
                target: SpawnTarget::PositionalArgs(args),
                delay_millis: Some(0),
            }),
            ..
        } if selector == Symbol::intern("inspect") && args == vec![coin, alice]
    ));
}

#[test]
fn shared_runner_executes_invocations_from_multiple_threads() {
    let mut runner = SourceRunner::new_empty();
    runner
        .run_filein(
            "make_identity(:player)\n\
                 make_identity(:alice)\n\
                 make_relation(:Delegates, 3)\n\
                 assert Delegates(#alice, #player, 0)\n\
                 verb count_up(actor @ #player, count)\n\
                   let i = 0\n\
                   while i < count\n\
                     i = i + 1\n\
                   end\n\
                   return i\n\
                 end\n",
        )
        .unwrap();
    let actor = runner.actor_identity(Symbol::intern("alice")).unwrap();
    let completed_before = runner.task_manager.completed_len();
    let runner = Arc::new(runner.into_shared());

    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for worker in 0..4 {
            let runner = Arc::clone(&runner);
            handles.push(scope.spawn(move || {
                for _ in 0..10 {
                    let submitted = runner
                        .submit_invocation(TaskRequest {
                            principal: None,
                            actor: None,
                            endpoint: Identity::new(0x00ee_2000_0000_0000 + worker).unwrap(),
                            authority: AuthorityContext::root(),
                            input: TaskInput::Invocation {
                                selector: Symbol::intern("count_up"),
                                roles: vec![
                                    (Symbol::intern("actor"), Value::identity(actor)),
                                    (Symbol::intern("count"), Value::int(100).unwrap()),
                                ],
                            },
                        })
                        .unwrap();
                    assert!(matches!(
                        submitted.outcome,
                        TaskOutcome::Complete { value, .. } if value == Value::int(100).unwrap()
                    ));
                }
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }
    });

    assert_eq!(runner.completed_len(), completed_before + 40);
}

#[test]
fn shared_runner_reads_endpoint_state_from_multiple_threads() {
    let mut runner = SourceRunner::new_empty();
    runner.run_source("make_identity(:alice)").unwrap();
    let alice = runner.actor_identity(Symbol::intern("alice")).unwrap();
    for worker in 0..4 {
        runner
            .open_endpoint(
                Identity::new(0x00ee_2100_0000_0000 + worker).unwrap(),
                Some(alice),
                Symbol::intern("telnet"),
            )
            .unwrap();
    }
    let runner = Arc::new(runner.into_shared());

    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for worker in 0..4 {
            let runner = Arc::clone(&runner);
            handles.push(scope.spawn(move || {
                let endpoint = Identity::new(0x00ee_2100_0000_0000 + worker).unwrap();
                for _ in 0..10 {
                    let request = runner
                        .source_request_for_endpoint(
                            endpoint,
                            "return EndpointActor(endpoint(), #alice)",
                        )
                        .unwrap();
                    let request = TaskRequest {
                        authority: AuthorityContext::root(),
                        ..request
                    };
                    let submitted = runner.submit_source(request).unwrap();
                    assert!(matches!(
                        submitted.outcome,
                        TaskOutcome::Complete { value, .. } if value == Value::bool(true)
                    ));
                }
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }
    });
}

#[test]
fn runner_dispatch_binds_unrestricted_method_params() {
    let mut runner = SourceRunner::new_empty();
    runner
        .run_filein(include_str!("../../../apps/shared/string.mica"))
        .unwrap();
    runner
        .run_filein(include_str!("../../../apps/shared/events.mica"))
        .unwrap();
    runner
        .run_filein(include_str!("../../../apps/mud/core.mica"))
        .unwrap();
    runner
        .run_filein(include_str!("../../../apps/mud/event-substitutions.mica"))
        .unwrap();
    let alice = runner.actor_identity(Symbol::intern("alice")).unwrap();
    let bob = runner.actor_identity(Symbol::intern("bob")).unwrap();

    let report = runner
        .run_source("return :say(actor: #alice, message: \"hello\")")
        .unwrap();

    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::bool(true)
    ));
    let emissions = runner.drain_emissions();
    assert_eq!(emissions.len(), 2);
    assert_eq!(emissions[0].target, alice);
    assert_eq!(emissions[0].value, Value::string("You say, \"hello\""));
    assert_eq!(emissions[1].target, bob);
    assert_eq!(emissions[1].value, Value::string("Alice says, \"hello\""));
}

#[test]
fn runner_mud_command_parser_runs_in_mica() {
    let mut runner = SourceRunner::new_empty();
    runner
        .run_filein(include_str!("../../../apps/shared/string.mica"))
        .unwrap();
    runner
        .run_filein(include_str!("../../../apps/shared/events.mica"))
        .unwrap();
    runner
        .run_filein(include_str!("../../../apps/mud/core.mica"))
        .unwrap();
    runner
        .run_filein(include_str!("../../../apps/mud/event-substitutions.mica"))
        .unwrap();
    runner
        .run_filein(include_str!("../../../apps/mud/command-parser.mica"))
        .unwrap();
    runner
        .run_filein(include_str!("../../../apps/mud/tests/event-scenarios.mica"))
        .unwrap();
    let alice = runner.actor_identity(Symbol::intern("alice")).unwrap();
    let bob = runner.actor_identity(Symbol::intern("bob")).unwrap();
    let endpoint = SYSTEM_ENDPOINT;

    runner.run_source("make_identity(:polluted_coin)").unwrap();
    runner
        .run_source("assert Delegates(#polluted_coin, #thing, 0)")
        .unwrap();
    runner
        .run_source("assert command/Noun(#polluted_coin, \"coin\")")
        .unwrap();
    runner
        .run_source("assert LocatedIn(#polluted_coin, #first_room)")
        .unwrap();

    let report = runner
        .run_source("return :command(actor: #alice, endpoint: endpoint(), line: \"say hello\")")
        .unwrap();

    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::bool(true)
    ));
    let emissions = runner.drain_emissions();
    assert_eq!(emissions.len(), 2);
    assert_eq!(emissions[0].target, alice);
    assert_eq!(emissions[0].value, Value::string("You say, \"hello\""));
    assert_eq!(emissions[1].target, bob);
    assert_eq!(emissions[1].value, Value::string("Alice says, \"hello\""));

    let report = runner
        .run_source("return :command(actor: #alice, endpoint: endpoint(), line: \"up\")")
        .unwrap();
    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::bool(false)
    ));
    let emissions = runner.drain_emissions();
    assert_eq!(emissions.len(), 1);
    assert_eq!(emissions[0].target, alice);
    assert_eq!(emissions[0].value, Value::string("You cannot go that way."));

    let report = runner
        .run_source("return :command(actor: #alice, endpoint: endpoint(), line: \"get coin\")")
        .unwrap();
    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::bool(true)
    ));
    let emissions = runner.drain_emissions();
    assert_eq!(emissions.len(), 2);
    assert_eq!(emissions[0].target, alice);
    assert_eq!(emissions[0].value, Value::string("You take the coin."));
    assert_eq!(emissions[1].target, bob);
    assert_eq!(emissions[1].value, Value::string("Alice takes the coin."));

    let report = runner
        .run_source("return :command(actor: #alice, endpoint: endpoint(), line: \"look\")")
        .unwrap();
    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::bool(true)
    ));
    let emissions = runner.drain_emissions();
    assert!(
        emissions.iter().all(
            |effect| effect.value != Value::string("A tarnished brass coin catches the light.")
        )
    );
    assert!(
        emissions.iter().any(|effect| effect.value
            == Value::string("A small wooden box rests here, open and empty."))
    );

    let report = runner
        .run_source("return :command(actor: #alice, endpoint: endpoint(), line: \"look box\")")
        .unwrap();
    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::bool(true)
    ));
    let emissions = runner.drain_emissions();
    assert_eq!(emissions.len(), 1);
    assert_eq!(
        emissions[0].value,
        Value::string("A small wooden box rests here, open and empty.")
    );

    let report = runner
        .run_source("return :command(actor: #alice, endpoint: endpoint(), line: \"look at box\")")
        .unwrap();
    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::bool(true)
    ));
    let emissions = runner.drain_emissions();
    assert_eq!(emissions.len(), 1);
    assert_eq!(
        emissions[0].value,
        Value::string("A small wooden box rests here, open and empty.")
    );

    let report = runner
        .run_source("return :command(actor: #alice, endpoint: endpoint(), line: \"look in box\")")
        .unwrap();
    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::bool(true)
    ));
    let emissions = runner.drain_emissions();
    assert_eq!(emissions.len(), 1);
    assert_eq!(emissions[0].value, Value::string("It is empty."));

    let report = runner
        .run_source(
            "return :command(actor: #alice, endpoint: endpoint(), line: \"put coin in box\")",
        )
        .unwrap();
    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::bool(true)
    ));
    let emissions = runner.drain_emissions();
    assert_eq!(emissions.len(), 2);
    assert_eq!(emissions[0].target, alice);
    assert_eq!(
        emissions[0].value,
        Value::string("You put the coin in the box.")
    );
    assert_eq!(emissions[1].target, bob);
    assert_eq!(
        emissions[1].value,
        Value::string("Alice puts the coin in the box.")
    );

    let report = runner
        .run_source("return :command(actor: #alice, endpoint: endpoint(), line: \"look in box\")")
        .unwrap();
    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::bool(true)
    ));
    let emissions = runner.drain_emissions();
    assert_eq!(emissions.len(), 1);
    assert_eq!(
        emissions[0].value,
        Value::string("A tarnished brass coin catches the light.")
    );

    let report = runner
        .run_source(
            "return :command(actor: #alice, endpoint: endpoint(), line: \"take coin from box\")",
        )
        .unwrap();
    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::bool(true)
    ));
    let emissions = runner.drain_emissions();
    assert_eq!(emissions.len(), 2);
    assert_eq!(emissions[0].target, alice);
    assert_eq!(
        emissions[0].value,
        Value::string("You take the coin from the box.")
    );
    assert_eq!(emissions[1].target, bob);
    assert_eq!(
        emissions[1].value,
        Value::string("Alice takes the coin from the box.")
    );

    let report = runner
        .run_source("return :command(actor: #alice, endpoint: endpoint(), line: \"get coin\")")
        .unwrap();
    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::bool(false)
    ));
    let emissions = runner.drain_emissions();
    assert_eq!(emissions.len(), 1);
    assert_eq!(emissions[0].target, alice);
    assert_eq!(emissions[0].value, Value::string("You already have that."));

    let report = runner
        .run_source("return :command(actor: #alice, endpoint: endpoint(), line: \"drop coin\")")
        .unwrap();
    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::bool(true)
    ));
    let emissions = runner.drain_emissions();
    assert_eq!(emissions.len(), 2);
    assert_eq!(emissions[0].target, alice);
    assert_eq!(emissions[0].value, Value::string("You drop the coin."));
    assert_eq!(emissions[1].target, bob);
    assert_eq!(emissions[1].value, Value::string("Alice drops the coin."));

    let report = runner
        .run_source("return :command(actor: #alice, endpoint: endpoint(), line: \"flailwildly\")")
        .unwrap();

    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::bool(false)
    ));
    let emissions = runner.drain_emissions();
    assert_eq!(emissions.len(), 1);
    assert_eq!(emissions[0].target, endpoint);
    assert_eq!(
        emissions[0].value,
        Value::string("I do not understand that.")
    );

    let report = runner
        .run_source("return test/command_parser_records_structured_utility_events()")
        .unwrap();
    assert!(
        matches!(report.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::bool(true)),
        "{}",
        report.render()
    );
    runner.drain_emissions();

    let report = runner
        .run_source("return test/social_commands_emit_perspective_events()")
        .unwrap();
    assert!(
        matches!(report.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::bool(true)),
        "{}",
        report.render()
    );
    runner.drain_emissions();
}

#[test]
fn runner_mud_core_derives_exits_and_recursive_location() {
    let mut runner = SourceRunner::new_empty();
    runner
        .run_filein(include_str!("../../../apps/shared/string.mica"))
        .unwrap();
    runner
        .run_filein(include_str!("../../../apps/shared/events.mica"))
        .unwrap();
    runner
        .run_filein(include_str!("../../../apps/mud/core.mica"))
        .unwrap();
    runner
        .run_filein(include_str!("../../../apps/mud/event-substitutions.mica"))
        .unwrap();
    runner
        .run_filein(include_str!("../../../apps/mud/command-parser.mica"))
        .unwrap();
    let first_room = runner.named_identity(Symbol::intern("first_room")).unwrap();
    let north_room = runner.named_identity(Symbol::intern("north_room")).unwrap();
    let attic = runner.named_identity(Symbol::intern("attic")).unwrap();
    let coin = runner.named_identity(Symbol::intern("coin")).unwrap();

    let report = runner
        .run_source("return one Exit(#north_room, \"south\", ?destination)")
        .unwrap();
    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::identity(first_room)
    ));

    let report = runner.run_source("return CanSee(#alice, #coin)").unwrap();
    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::bool(true)
    ));

    runner
        .run_source("return :get(actor: #alice, item: #coin)")
        .unwrap();
    let report = runner
        .run_source(
            "let event = one event/Delivery(#alice, ?event)\n\
                 let source = one event/Source(event, ?source)\n\
                 return [frob_delegate(source), event/bindings(source)[:item]]",
        )
        .unwrap();
    let take_event = runner.named_identity(Symbol::intern("event/take")).unwrap();
    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. }
            if value == Value::list([Value::identity(take_event), Value::identity(coin)])
    ));

    let report = runner.run_source("return Carrying(#alice, #coin)").unwrap();
    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::bool(true)
    ));

    let north = runner
        .run_source("return :go(actor: #alice, direction: \"north\")")
        .unwrap();
    assert!(matches!(
        north.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::bool(true)
    ));
    let report = runner
        .run_source("return Within(#coin, #north_room)")
        .unwrap();
    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::bool(true)
    ));
    let report = runner
        .run_source("return one LocatedIn(#alice, ?room)")
        .unwrap();
    assert!(
        matches!(
            report.outcome,
            TaskOutcome::Complete { ref value, .. } if *value == Value::identity(north_room)
        ),
        "{}",
        report.render()
    );

    let north = runner
        .run_source("return :go(actor: #alice, direction: \"north\")")
        .unwrap();
    assert!(matches!(
        north.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::bool(false)
    ));
    let report = runner
        .run_source("return one LocatedIn(#alice, ?room)")
        .unwrap();
    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::identity(north_room)
    ));

    let drop = runner
        .run_source("return :drop(actor: #alice, item: #coin)")
        .unwrap();
    assert!(matches!(
        drop.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::bool(true)
    ));
    let report = runner
        .run_source("return one LocatedIn(#coin, ?room)")
        .unwrap();
    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::identity(north_room)
    ));

    let up = runner
        .run_source("return :go(actor: #alice, direction: \"up\")")
        .unwrap();
    assert!(matches!(
        up.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::bool(true)
    ));
    let report = runner
        .run_source("return one LocatedIn(#alice, ?room)")
        .unwrap();
    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::identity(attic)
    ));

    let down = runner
        .run_source("return :go(actor: #alice, direction: \"down\")")
        .unwrap();
    assert!(matches!(
        down.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::bool(true)
    ));
    let report = runner
        .run_source("return one LocatedIn(#alice, ?room)")
        .unwrap();
    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::identity(north_room)
    ));

    let south = runner
        .run_source("return :go(actor: #alice, direction: \"south\")")
        .unwrap();
    assert!(matches!(
        south.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::bool(true)
    ));
    let report = runner
        .run_source("return one LocatedIn(#alice, ?room)")
        .unwrap();
    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::identity(first_room)
    ));

    let denied = runner
        .run_source("return :build_room(actor: #bob, name: \"Library\")")
        .unwrap();
    assert!(
        matches!(denied.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::bool(false)),
        "{}",
        denied.render()
    );
    let report = runner
        .run_source("return builder/room_named_in_area(#hotel_area, \"Library\")")
        .unwrap();
    assert!(
        matches!(report.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::nothing()),
        "{}",
        report.render()
    );

    let built = runner
        .run_source("return :build_room(actor: #alice, name: \"Library\")")
        .unwrap();
    let library = match built.outcome {
        TaskOutcome::Complete { ref value, .. } if value.frob_delegate().is_some() => value.clone(),
        _ => panic!("unexpected build outcome: {}", built.render()),
    };
    let report = runner
        .run_source("return builder/room_named_in_area(#hotel_area, \"Library\")")
        .unwrap();
    assert!(
        matches!(report.outcome, TaskOutcome::Complete { ref value, .. } if *value == library),
        "{}",
        report.render()
    );

    let built = runner
        .run_source("return :build_room(actor: #alice, name: \"Balcony\")")
        .unwrap();
    let balcony = match built.outcome {
        TaskOutcome::Complete { ref value, .. } if value.frob_delegate().is_some() => value.clone(),
        _ => panic!("unexpected build outcome: {}", built.render()),
    };
    let dug = runner
        .run_source(
            "return :create_passage(actor: #alice, from: #first_room, to: builder/room_named_in_area(#hotel_area, \"Balcony\"), label: \"out\", return_label: nothing)",
        )
        .unwrap();
    assert!(
        matches!(dug.outcome, TaskOutcome::Complete { ref value, .. } if value.frob_delegate().is_some()),
        "{}",
        dug.render()
    );
    let report = runner
        .run_source("return one Exit(#first_room, \"out\", ?destination)")
        .unwrap();
    assert!(
        matches!(report.outcome, TaskOutcome::Complete { ref value, .. } if *value == balcony),
        "{}",
        report.render()
    );
    let report = runner
        .run_source("return one Exit(builder/room_named_in_area(#hotel_area, \"Balcony\"), \"in\", ?destination)")
        .unwrap();
    assert!(
        matches!(report.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::nothing()),
        "{}",
        report.render()
    );

    let dug = runner
        .run_source(
            "return :create_passage(actor: #alice, from: #first_room, to: builder/room_named_in_area(#hotel_area, \"Library\"), label: \"east\", return_label: \"west\")",
        )
        .unwrap();
    assert!(
        matches!(dug.outcome, TaskOutcome::Complete { ref value, .. } if value.frob_delegate().is_some()),
        "{}",
        dug.render()
    );
    let report = runner
        .run_source("return one Exit(#first_room, \"east\", ?destination)")
        .unwrap();
    assert!(
        matches!(report.outcome, TaskOutcome::Complete { ref value, .. } if *value == library),
        "{}",
        report.render()
    );

    let plain_build = runner
        .run_source("return :command(actor: #alice, endpoint: endpoint(), line: \"build Shed\")")
        .unwrap();
    assert!(
        matches!(plain_build.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::bool(false)),
        "{}",
        plain_build.render()
    );
    let report = runner
        .run_source("return builder/room_named_in_area(#hotel_area, \"Shed\")")
        .unwrap();
    assert!(
        matches!(report.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::nothing()),
        "{}",
        report.render()
    );

    let built = runner
        .run_source("return :command(actor: #alice, endpoint: endpoint(), line: \"@build Studio\")")
        .unwrap();
    let studio = match built.outcome {
        TaskOutcome::Complete { ref value, .. } if value.frob_delegate().is_some() => value.clone(),
        _ => panic!("unexpected @build outcome: {}", built.render()),
    };
    let dug = runner
        .run_source(
            "return :command(actor: #alice, endpoint: endpoint(), line: \"@dig portal to Studio\")",
        )
        .unwrap();
    let _passage = match dug.outcome {
        TaskOutcome::Complete { ref value, .. } if value.frob_delegate().is_some() => value.clone(),
        _ => panic!("unexpected @dig outcome: {}", dug.render()),
    };
    let report = runner
        .run_source("return one Exit(#first_room, \"portal\", ?destination)")
        .unwrap();
    assert!(
        matches!(report.outcome, TaskOutcome::Complete { ref value, .. } if *value == studio),
        "{}",
        report.render()
    );
    let described = runner
        .run_source(
            "return :command(actor: #alice, endpoint: endpoint(), line: \"@describe portal as A shimmering arch hums here.\")",
        )
        .unwrap();
    assert!(
        matches!(described.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::bool(true)),
        "{}",
        described.render()
    );
    let report = runner
        .run_source("return one PassageDescription(builder/passage_from(#first_room, \"portal\"), #first_room, ?description)")
        .unwrap();
    assert!(
        matches!(
            report.outcome,
            TaskOutcome::Complete { ref value, .. }
                if value.with_str(|text| text == "A shimmering arch hums here.").unwrap_or(false)
        ),
        "{}",
        report.render()
    );
    let described = runner
        .run_source(
            "return :command(actor: #alice, endpoint: endpoint(), line: \"@describe here as The lobby has been rewritten.\")",
        )
        .unwrap();
    assert!(
        matches!(described.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::bool(true)),
        "{}",
        described.render()
    );
    let report = runner.run_source("return #first_room.description").unwrap();
    assert!(
        matches!(
            report.outcome,
            TaskOutcome::Complete { ref value, .. }
                if value.with_str(|text| text == "The lobby has been rewritten.").unwrap_or(false)
        ),
        "{}",
        report.render()
    );
    let described = runner
        .run_source(
            "return :command(actor: #alice, endpoint: endpoint(), line: \"@describe coin as The coin now gleams.\")",
        )
        .unwrap();
    assert!(
        matches!(described.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::bool(true)),
        "{}",
        described.render()
    );
    let report = runner.run_source("return #coin.description").unwrap();
    assert!(
        matches!(
            report.outcome,
            TaskOutcome::Complete { ref value, .. }
                if value.with_str(|text| text == "The coin now gleams.").unwrap_or(false)
        ),
        "{}",
        report.render()
    );
    let renamed = runner
        .run_source(
            "return :command(actor: #alice, endpoint: endpoint(), line: \"@name coin as doubloon\")",
        )
        .unwrap();
    assert!(
        matches!(renamed.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::bool(true)),
        "{}",
        renamed.render()
    );
    let report = runner.run_source("return #coin.name").unwrap();
    assert!(
        matches!(
            report.outcome,
            TaskOutcome::Complete { ref value, .. }
                if value.with_str(|text| text == "doubloon").unwrap_or(false)
        ),
        "{}",
        report.render()
    );
    let aliased = runner
        .run_source(
            "return :command(actor: #alice, endpoint: endpoint(), line: \"@alias doubloon as coin, shiny coin\")",
        )
        .unwrap();
    assert!(
        matches!(aliased.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::bool(true)),
        "{}",
        aliased.render()
    );
    let report = runner
        .run_source("return builder/resolve_edit_target(#alice, \"shiny coin\") == #coin")
        .unwrap();
    assert!(
        matches!(report.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::bool(true)),
        "{}",
        report.render()
    );
    let fixed = runner
        .run_source(
            "return :command(actor: #alice, endpoint: endpoint(), line: \"@fixed shiny coin\")",
        )
        .unwrap();
    assert!(
        matches!(fixed.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::bool(true)),
        "{}",
        fixed.render()
    );
    let report = runner.run_source("return Portable(#coin)").unwrap();
    assert!(
        matches!(report.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::bool(false)),
        "{}",
        report.render()
    );
    let portable = runner
        .run_source(
            "return :command(actor: #alice, endpoint: endpoint(), line: \"@portable shiny coin\")",
        )
        .unwrap();
    assert!(
        matches!(portable.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::bool(true)),
        "{}",
        portable.render()
    );
    let report = runner.run_source("return Portable(#coin)").unwrap();
    assert!(
        matches!(report.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::bool(true)),
        "{}",
        report.render()
    );
    let shown = runner
        .run_source(
            "return :command(actor: #alice, endpoint: endpoint(), line: \"@show shiny coin\")",
        )
        .unwrap();
    assert!(
        matches!(shown.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::bool(true)),
        "{}",
        shown.render()
    );
    let report = runner
        .run_source("return #coin.locatedIn == #first_room")
        .unwrap();
    assert!(
        matches!(report.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::bool(true)),
        "{}",
        report.render()
    );
    let hidden = runner
        .run_source(
            "return :command(actor: #alice, endpoint: endpoint(), line: \"@hide shiny coin\")",
        )
        .unwrap();
    assert!(
        matches!(hidden.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::bool(true)),
        "{}",
        hidden.render()
    );
    let report = runner
        .run_source("return #coin.locatedIn == nothing")
        .unwrap();
    assert!(
        matches!(report.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::bool(true)),
        "{}",
        report.render()
    );
    let shown = runner
        .run_source(
            "return :command(actor: #alice, endpoint: endpoint(), line: \"@show shiny coin\")",
        )
        .unwrap();
    assert!(
        matches!(shown.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::bool(true)),
        "{}",
        shown.render()
    );
    let report = runner
        .run_source("return #coin.locatedIn == #first_room")
        .unwrap();
    assert!(
        matches!(report.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::bool(true)),
        "{}",
        report.render()
    );
    let removed = runner
        .run_source("return :command(actor: #alice, endpoint: endpoint(), line: \"@undig portal\")")
        .unwrap();
    assert!(
        matches!(removed.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::bool(true)),
        "{}",
        removed.render()
    );
    let report = runner
        .run_source("return one Exit(#first_room, \"portal\", ?destination)")
        .unwrap();
    assert!(
        matches!(report.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::nothing()),
        "{}",
        report.render()
    );
}

#[test]
fn runner_mud_narrative_renders_recent_event_window() {
    let mut runner = SourceRunner::new_empty();
    runner
        .run_filein(include_str!("../../../apps/shared/string.mica"))
        .unwrap();
    runner
        .run_filein(include_str!("../../../apps/shared/events.mica"))
        .unwrap();
    runner
        .run_filein(include_str!("../../../apps/shared/retrieval.mica"))
        .unwrap();
    runner
        .run_filein(include_str!("../../../apps/mud/core.mica"))
        .unwrap();
    runner
        .run_filein(include_str!("../../../apps/mud/event-substitutions.mica"))
        .unwrap();
    runner.run_source("make_identity(:web)").unwrap();
    runner
        .run_filein(include_str!("../../../apps/mud/ui-session.mica"))
        .unwrap();
    runner
        .run_filein(include_str!("../../../apps/mud/ui-mica-inspect.mica"))
        .unwrap();
    runner
        .run_filein(include_str!("../../../apps/mud/ui-compose.mica"))
        .unwrap();
    runner
        .run_filein(include_str!("../../../apps/mud/ui-retrieval.mica"))
        .unwrap();
    runner
        .run_filein(include_str!("../../../apps/mud/ui-narrative.mica"))
        .unwrap();
    runner
        .run_filein(include_str!("../../../apps/mud/tests/event-scenarios.mica"))
        .unwrap();
    runner
        .run_filein(include_str!(
            "../../../apps/mud/tests/ui-narrative-scenarios.mica"
        ))
        .unwrap();

    let report = runner
        .run_source("return test/ui_narrative_renders_recent_event_window()")
        .unwrap();
    assert!(
        matches!(report.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::bool(true)),
        "{}",
        report.render()
    );

    let report = runner
        .run_source("return test/ui_narrative_renders_structured_events()")
        .unwrap();
    assert!(
        matches!(report.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::bool(true)),
        "{}",
        report.render()
    );

    let report = runner
        .run_source("return test/narrative_entries_follow_delivery_and_replacement()")
        .unwrap();
    assert!(
        matches!(report.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::bool(true)),
        "{}",
        report.render()
    );
}

#[test]
fn runner_mud_auth_sync_view_tree_renders() {
    let mut runner = SourceRunner::new_empty();
    runner
        .run_filein(include_str!("../../../apps/shared/sync-host.mica"))
        .unwrap();
    runner
        .run_filein(include_str!("../../../apps/shared/string.mica"))
        .unwrap();
    runner
        .run_filein(include_str!("../../../apps/shared/events.mica"))
        .unwrap();
    runner
        .run_filein(include_str!("../../../apps/shared/retrieval.mica"))
        .unwrap();
    runner
        .run_filein(include_str!("../../../apps/mud/core.mica"))
        .unwrap();
    runner
        .run_filein(include_str!("../../../apps/mud/auth.mica"))
        .unwrap();
    runner
        .run_filein(include_str!("../../../apps/mud/event-substitutions.mica"))
        .unwrap();
    runner
        .run_filein(include_str!("../../../apps/mud/command-parser.mica"))
        .unwrap();
    runner
        .run_filein(include_str!("../../../apps/shared/sync-dom.mica"))
        .unwrap();
    runner
        .run_filein(include_str!("../../../apps/mud/ui-session.mica"))
        .unwrap();
    runner
        .run_filein(include_str!("../../../apps/mud/ui-mica-inspect.mica"))
        .unwrap();
    runner
        .run_filein(include_str!("../../../apps/mud/ui-compose.mica"))
        .unwrap();
    runner
        .run_filein(include_str!("../../../apps/mud/ui-retrieval.mica"))
        .unwrap();
    runner
        .run_filein(include_str!("../../../apps/mud/ui-narrative.mica"))
        .unwrap();
    runner
        .run_filein(include_str!("../../../apps/mud/ui-actions.mica"))
        .unwrap();
    runner
        .run_filein(include_str!("../../../apps/mud/http.mica"))
        .unwrap();

    let report = runner
        .run_source_as(
            Symbol::intern("web"),
            "return http_login_document(\"/auth/login?return=%2Fmud\")",
        )
        .unwrap();
    assert!(
        matches!(
            report.outcome,
            TaskOutcome::Complete { ref value, .. }
                if value
                    .with_str(|text| {
                        text.contains("login-icon-label")
                            && text.contains("Sign In")
                            && text.contains("Create Player")
                    })
                    .unwrap_or(false)
        ),
        "{:?}",
        report.outcome
    );

    let report = runner
        .run_source_as(
            Symbol::intern("web"),
            "return to_literal(sync_view_tree(21))",
        )
        .unwrap();
    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. }
            if value
                .with_str(|text| text.contains("mud-login") && text.contains("mud-icon-label"))
                .unwrap_or(false)
    ));

    let web = runner.actor_identity(Symbol::intern("web")).unwrap();
    let alice = runner.actor_identity(Symbol::intern("alice")).unwrap();
    let endpoint = Identity::new(0x00ee_0000_0000_0021).unwrap();
    runner
        .open_endpoint_with_context(endpoint, Some(web), Some(alice), Symbol::intern("http"))
        .unwrap();
    let request = runner
        .source_request_for_endpoint(endpoint, "return to_literal(sync_view_tree(21))")
        .unwrap();
    let report = runner.submit_source(request).unwrap();
    assert!(
        matches!(
            report.outcome,
            TaskOutcome::Complete { ref value, .. }
                if value
                    .with_str(|text| {
                        text.contains("world-tools")
                            && text.contains("inspect-current-room")
                            && text.contains("mud_create_passage")
                            && text.contains("mud_object_browser_search")
                            && text.contains("mud_mica_browser_search")
                    })
                    .unwrap_or(false)
        ),
        "{:?}",
        report.outcome
    );

    let request = runner
        .source_request_for_endpoint(
            endpoint,
            "return sync_event(endpoint(), nothing, 21, \"submit\", \"\", \"mud_command\", {})",
        )
        .unwrap();
    let report = runner.submit_source(request).unwrap();
    assert!(
        matches!(report.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::bool(false)),
        "{:?}",
        report.outcome
    );

    let request = runner
        .source_request_for_endpoint(
            endpoint,
            "let ok = sync_event(endpoint(), nothing, 21, \"input\", \"\", \"mud_command\", {:text -> \"get \", :suggest -> \"true\", :suggest_index -> \"0\"})\n\
             return ok && endpoint().session/commandDraft == \"get \"",
        )
        .unwrap();
    let report = runner.submit_source(request).unwrap();
    assert!(
        matches!(report.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::bool(true)),
        "{:?}",
        report.outcome
    );

    let request = runner
        .source_request_for_endpoint(endpoint, "return to_literal(ui/command_bar_node(#alice))")
        .unwrap();
    let report = runner.submit_source(request).unwrap();
    assert!(
        matches!(
            report.outcome,
            TaskOutcome::Complete { ref value, .. }
                if value
                    .with_str(|text| text.contains("get coin"))
                    .unwrap_or(false)
        ),
        "{:?}",
        report.outcome
    );

    let request = runner
        .source_request_for_endpoint(
            endpoint,
            "let ok = sync_event(endpoint(), nothing, 21, \"submit\", \"\", \"mud_edit_entity\", {:entity -> to_literal(#coin), :name -> \"doubloon\", :aliases -> \"coin, shiny coin\", :description -> \"A direct-manipulation edit.\", :portable -> \"true\", :visible_here -> \"true\"})\n\
             return ok && #coin.name == \"doubloon\" && #coin.description == \"A direct-manipulation edit.\" && Portable(#coin) && #coin.locatedIn == #first_room && command/match_object(#alice, \"shiny coin\") == #coin",
        )
        .unwrap();
    let report = runner.submit_source(request).unwrap();
    assert!(
        matches!(report.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::bool(true)),
        "{:?}",
        report.outcome
    );

    let request = runner
        .source_request_for_endpoint(
            endpoint,
            "return sync_event(endpoint(), nothing, 21, \"input\", \"\", \"mud_object_browser_search\", {:query -> \"wooden\"})",
        )
        .unwrap();
    let report = runner.submit_source(request).unwrap();
    assert!(
        matches!(report.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::bool(true)),
        "{:?}",
        report.outcome
    );

    let request = runner
        .source_request_for_endpoint(
            endpoint,
            "return to_literal(ui/object_browser_panel_node(#alice))",
        )
        .unwrap();
    let report = runner.submit_source(request).unwrap();
    assert!(
        matches!(
            report.outcome,
            TaskOutcome::Complete { ref value, .. }
                if value
                    .with_str(|text| {
                        text.contains("object-browser-row")
                            && text.contains("wooden box")
                            && text.contains("mud_edit_entity")
                    })
                    .unwrap_or(false)
        ),
        "{:?}",
        report.outcome
    );

    let request = runner
        .source_request_for_endpoint(
            endpoint,
            "ui/mica_inspect_set_selected(endpoint(), #alice)\n\
             return to_literal(ui/mica_inspect_panel_node(#alice))",
        )
        .unwrap();
    let report = runner.submit_source(request).unwrap();
    assert!(
        matches!(
            report.outcome,
            TaskOutcome::Complete { ref value, .. }
                if value
                    .with_str(|text| {
                        text.contains("source-popout")
                            && text.contains("mica-source-full")
                            && text.contains("Open source")
                    })
                    .unwrap_or(false)
        ),
        "{:?}",
        report.outcome
    );

    let request = runner
        .source_request_for_endpoint(
            endpoint,
            "ui/mica_inspect_set_selected(endpoint(), #room)\n\
             return to_literal(ui/mica_method_catalog_node(#room))",
        )
        .unwrap();
    let report = runner.submit_source(request).unwrap();
    assert!(
        matches!(
            report.outcome,
            TaskOutcome::Complete { ref value, .. }
                if value
                    .with_str(|text| text.contains("Showing 8 of") && text.contains("Show all"))
                    .unwrap_or(false)
        ),
        "{:?}",
        report.outcome
    );

    let request = runner
        .source_request_for_endpoint(
            endpoint,
            "return sync_event(endpoint(), nothing, 21, \"submit\", \"\", \"mud_mica_method_limit\", {:mode -> \"all\"})",
        )
        .unwrap();
    let report = runner.submit_source(request).unwrap();
    assert!(
        matches!(report.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::bool(true)),
        "{:?}",
        report.outcome
    );

    let request = runner
        .source_request_for_endpoint(
            endpoint,
            "return to_literal(ui/mica_method_catalog_node(#room))",
        )
        .unwrap();
    let report = runner.submit_source(request).unwrap();
    assert!(
        matches!(
            report.outcome,
            TaskOutcome::Complete { ref value, .. }
                if value.with_str(|text| text.contains("Show fewer")).unwrap_or(false)
        ),
        "{:?}",
        report.outcome
    );

    let request = runner
        .source_request_for_endpoint(
            endpoint,
            "return sync_event(endpoint(), nothing, 21, \"input\", \"\", \"mud_mica_browser_search\", {:query -> \"room\"})",
        )
        .unwrap();
    let report = runner.submit_source(request).unwrap();
    assert!(
        matches!(report.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::bool(true)),
        "{:?}",
        report.outcome
    );

    let request = runner
        .source_request_for_endpoint(
            endpoint,
            "return to_literal(ui/mica_browser_panel_node(#alice))",
        )
        .unwrap();
    let report = runner.submit_source(request).unwrap();
    assert!(
        matches!(
            report.outcome,
            TaskOutcome::Complete { ref value, .. }
                if value
                    .with_str(|text| {
                        text.contains("mica-browser-results")
                            && text.contains("Relations")
                            && text.contains("Methods")
                            && text.contains("Rules")
                            && text.contains("source-popout")
                    })
                    .unwrap_or(false)
        ),
        "{:?}",
        report.outcome
    );
}

#[test]
fn runner_resume_task_uses_continuation_request_authority() {
    let mut runner = SourceRunner::new_empty();
    let program = Arc::new(
        Program::new(
            0,
            [
                Instruction::Suspend {
                    kind: SuspendKind::TimedMillis(1),
                },
                Instruction::Return {
                    value: Operand::Value(Value::bool(true)),
                },
            ],
        )
        .unwrap(),
    );
    let (task_id, first) = runner.task_manager.submit(program).unwrap();
    assert!(matches!(first, TaskOutcome::Suspended { .. }));

    let outcome = runner
        .resume_task(TaskRequest {
            principal: None,
            actor: None,
            endpoint: SYSTEM_ENDPOINT,
            authority: AuthorityContext::root(),
            input: TaskInput::Continuation {
                task_id,
                value: Value::nothing(),
            },
        })
        .unwrap();

    assert!(matches!(
        outcome,
        TaskOutcome::Complete { value, .. } if value == Value::bool(true)
    ));
}

#[test]
fn runner_suspend_returns_continuation_value() {
    let mut runner = SourceRunner::new_empty();
    let submitted = runner
        .submit_source(TaskRequest {
            principal: None,
            actor: None,
            endpoint: SYSTEM_ENDPOINT,
            authority: AuthorityContext::root(),
            input: TaskInput::Source("return suspend()".to_owned()),
        })
        .unwrap();
    assert!(matches!(
        submitted.outcome,
        TaskOutcome::Suspended {
            kind: SuspendKind::Never,
            ..
        }
    ));

    let outcome = runner
        .resume_task(TaskRequest {
            principal: None,
            actor: None,
            endpoint: SYSTEM_ENDPOINT,
            authority: AuthorityContext::root(),
            input: TaskInput::Continuation {
                task_id: submitted.task_id,
                value: Value::string("awake"),
            },
        })
        .unwrap();

    assert!(matches!(
        outcome,
        TaskOutcome::Complete { value, .. } if value == Value::string("awake")
    ));
}

#[test]
fn runner_commit_yields_and_resumes_with_nothing() {
    let mut runner = SourceRunner::new_empty();
    let submitted = runner
        .submit_source(TaskRequest {
            principal: None,
            actor: None,
            endpoint: SYSTEM_ENDPOINT,
            authority: AuthorityContext::root(),
            input: TaskInput::Source("return commit()".to_owned()),
        })
        .unwrap();
    assert!(matches!(
        submitted.outcome,
        TaskOutcome::Suspended {
            kind: SuspendKind::Commit,
            ..
        }
    ));

    let outcome = runner
        .resume_task(TaskRequest {
            principal: None,
            actor: None,
            endpoint: SYSTEM_ENDPOINT,
            authority: AuthorityContext::root(),
            input: TaskInput::Continuation {
                task_id: submitted.task_id,
                value: Value::nothing(),
            },
        })
        .unwrap();

    assert!(matches!(
        outcome,
        TaskOutcome::Complete { value, .. } if value == Value::nothing()
    ));
}

#[test]
fn runner_tasks_builtin_lists_running_and_suspended_tasks() {
    let mut runner = SourceRunner::new_empty();
    runner.run_source("suspend(10)").unwrap();
    let report = runner.run_source("return tasks()").unwrap();
    let TaskOutcome::Complete { value, .. } = report.outcome else {
        panic!("tasks() did not complete");
    };
    let tasks = value.with_list(<[Value]>::to_vec).unwrap();

    assert!(
        tasks
            .iter()
            .any(|task| task_status(task) == Some((1, Symbol::intern("suspended"))))
    );
    assert!(
        tasks
            .iter()
            .any(|task| task_status(task) == Some((2, Symbol::intern("running"))))
    );
}

#[test]
fn runner_context_builtins_return_runtime_identities() {
    let mut runner = SourceRunner::new_empty();
    runner.run_source("make_identity(:alice)").unwrap();
    let alice = runner.actor_identity(Symbol::intern("alice")).unwrap();
    let endpoint = Identity::new(0x00ee_0000_0000_0003).unwrap();

    let submitted = runner
        .submit_source(TaskRequest {
            principal: Some(alice),
            actor: Some(alice),
            endpoint,
            authority: AuthorityContext::root(),
            input: TaskInput::Source("return [principal(), actor(), endpoint()]".to_owned()),
        })
        .unwrap();

    assert!(matches!(
        submitted.outcome,
        TaskOutcome::Complete { value, .. }
            if value.with_list(<[Value]>::to_vec).unwrap()
                == vec![
                    Value::identity(alice),
                    Value::identity(alice),
                    Value::identity(endpoint),
                ]
    ));
}

#[test]
fn runner_context_builtins_return_system_endpoint_without_actor_context() {
    let mut runner = SourceRunner::new_empty();
    let report = runner
        .run_source("return [principal(), actor(), endpoint()]")
        .unwrap();

    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. }
            if value.with_list(<[Value]>::to_vec).unwrap()
                == vec![
                    Value::nothing(),
                    Value::nothing(),
                    Value::identity(SYSTEM_ENDPOINT),
                ]
    ));
}

#[test]
fn runner_endpoint_facts_are_volatile_and_relation_wide() {
    let mut runner = SourceRunner::new_empty();
    runner.run_source("make_identity(:alice)").unwrap();
    let alice = runner.actor_identity(Symbol::intern("alice")).unwrap();
    let endpoint = Identity::new(0x00ee_0000_0000_0010).unwrap();
    let version_before_open = runner.task_manager.kernel().snapshot().version();
    runner
        .open_endpoint(endpoint, Some(alice), Symbol::intern("telnet"))
        .unwrap();
    let snapshot = runner.task_manager.kernel().snapshot();
    for name in [
        "Endpoint",
        "EndpointPrincipal",
        "EndpointActor",
        "EndpointProtocol",
        "EndpointOpen",
    ] {
        let metadata = snapshot
            .relation_metadata()
            .find(|metadata| metadata.name().name() == Some(name))
            .unwrap();
        assert_eq!(metadata.durability(), RelationDurability::Volatile);
    }
    assert_eq!(
        runner.task_manager.kernel().snapshot().version(),
        version_before_open + 1
    );
    let duplicate = runner
        .open_endpoint(endpoint, Some(alice), Symbol::intern("telnet"))
        .unwrap_err();
    assert!(format!("{duplicate:?}").contains("endpoint is already open"));
    assert_eq!(
        runner.task_manager.kernel().snapshot().version(),
        version_before_open + 1
    );

    let visible = runner
        .submit_source(TaskRequest {
            principal: None,
            actor: Some(alice),
            endpoint,
            authority: AuthorityContext::root(),
            input: TaskInput::Source("return EndpointActor(?endpoint, #alice)".to_owned()),
        })
        .unwrap();
    assert!(matches!(
        visible.outcome,
        TaskOutcome::Complete { value, .. }
            if value == query_relation(["endpoint"], [[Value::identity(endpoint)]])
    ));

    let root = runner
        .run_source("return EndpointActor(?endpoint, #alice)")
        .unwrap();
    assert!(matches!(
        root.outcome,
        TaskOutcome::Complete { value, .. }
            if value == query_relation(["endpoint"], [[Value::identity(endpoint)]])
    ));

    assert_eq!(runner.close_endpoint(endpoint), 4);
    assert_eq!(
        runner.task_manager.kernel().snapshot().version(),
        version_before_open + 2
    );
    let closed = runner
        .submit_source(TaskRequest {
            principal: None,
            actor: Some(alice),
            endpoint,
            authority: AuthorityContext::root(),
            input: TaskInput::Source("return EndpointActor(?endpoint, #alice)".to_owned()),
        })
        .unwrap();
    assert!(matches!(
        closed.outcome,
        TaskOutcome::Complete { value, .. } if value == query_relation(["endpoint"], [])
    ));
}

#[test]
fn runner_endpoint_invocation_uses_principal_authority_without_actor() {
    let mut runner = SourceRunner::new_empty();
    runner
        .run_filein(
            "make_identity(:web)\n\
                 make_relation(:RequestPath, 2, :volatile)\n\
                 make_relation(:CanRead, 2)\n\
                 make_relation(:CanInvoke, 2)\n\
                 assert CanRead(#web, :RequestPath)\n\
                 assert CanInvoke(#web, :http_request)\n\
                 verb http_request(request)\n\
                   return one RequestPath(request, ?path)\n\
                 end\n",
        )
        .unwrap();
    let web = runner.actor_identity(Symbol::intern("web")).unwrap();
    let endpoint = Identity::new(0x00ee_0000_0000_0011).unwrap();
    let request = Identity::new(0x00eb_0000_0000_0011).unwrap();
    let request_fact = (
        Symbol::intern("RequestPath"),
        Tuple::from([Value::identity(request), Value::string("/hello")]),
    );
    let version = runner.task_manager.kernel().snapshot().version();
    assert_eq!(
        runner
            .open_endpoint_with_context_and_volatile_tuples_named(
                endpoint,
                Some(web),
                None,
                Symbol::intern("http-request"),
                vec![request_fact.clone()],
            )
            .unwrap(),
        5
    );
    assert_eq!(
        runner.task_manager.kernel().snapshot().version(),
        version + 1
    );

    let submitted = runner
        .submit_invocation_for_endpoint(
            endpoint,
            Symbol::intern("http_request"),
            vec![(Symbol::intern("request"), Value::identity(request))],
        )
        .unwrap();

    assert!(matches!(
        submitted.outcome,
        TaskOutcome::Complete { value, .. }
            if value.with_str(|text| text == "/hello").unwrap_or(false)
    ));
    assert_eq!(
        runner
            .close_endpoint_and_retract_volatile_tuples_named(endpoint, vec![request_fact])
            .unwrap(),
        5
    );
    assert_eq!(
        runner.task_manager.kernel().snapshot().version(),
        version + 2
    );
}

#[test]
fn runner_volatile_host_fact_batches_are_atomic() {
    let mut runner = SourceRunner::new_empty();
    runner
        .run_source(
            "make_relation(:RequestPath, 2, :volatile)\n\
             make_relation(:RequestHeader, 3, :volatile)\n\
             make_relation(:DurableFact, 1)",
        )
        .unwrap();
    let request = Identity::new(0x00eb_0000_0000_0012).unwrap();
    let facts = vec![
        (
            Symbol::intern("RequestPath"),
            Tuple::from([Value::identity(request), Value::string("/hello")]),
        ),
        (
            Symbol::intern("RequestHeader"),
            Tuple::from([
                Value::identity(request),
                Value::string("accept"),
                Value::bytes(b"text/plain"),
            ]),
        ),
    ];
    let version = runner.task_manager.kernel().snapshot().version();

    assert_eq!(
        runner.assert_volatile_tuples_named(facts.clone()).unwrap(),
        2
    );
    assert_eq!(
        runner.task_manager.kernel().snapshot().version(),
        version + 1
    );
    assert!(
        runner
            .run_source("return RequestPath(?request, ?path)")
            .unwrap()
            .render()
            .contains("/hello")
    );

    assert_eq!(
        runner.retract_volatile_tuples_named(facts.clone()).unwrap(),
        2
    );
    assert_eq!(
        runner.task_manager.kernel().snapshot().version(),
        version + 2
    );
    let rows = runner
        .run_source("return RequestPath(?request, ?path)")
        .unwrap();
    assert!(matches!(
        rows.outcome,
        TaskOutcome::Complete { value, .. }
            if value == query_relation(["request", "path"], [])
    ));

    let rejected = runner
        .assert_volatile_tuples_named(vec![(
            Symbol::intern("DurableFact"),
            Tuple::from([Value::identity(request)]),
        )])
        .unwrap_err();
    assert!(format!("{rejected:?}").contains("relation DurableFact is not volatile"));
    assert_eq!(
        runner.task_manager.kernel().snapshot().version(),
        version + 2
    );
    assert!(
        runner
            .run_source("return DurableFact(?request)")
            .unwrap()
            .render()
            .contains("{}")
    );

    let endpoint = Identity::new(0x00ee_0000_0000_0012).unwrap();
    let rejected = runner
        .open_endpoint_with_context_and_volatile_tuples_named(
            endpoint,
            None,
            None,
            Symbol::intern("test"),
            vec![(
                Symbol::intern("DurableFact"),
                Tuple::from([Value::identity(request)]),
            )],
        )
        .unwrap_err();
    assert!(format!("{rejected:?}").contains("relation DurableFact is not volatile"));
    assert_eq!(
        runner.task_manager.kernel().snapshot().version(),
        version + 2
    );
    assert!(
        runner
            .task_manager
            .kernel()
            .snapshot()
            .scan(endpoint_open_relation(), &[Some(Value::identity(endpoint))])
            .unwrap()
            .is_empty()
    );
}

#[test]
fn runner_replaces_volatile_host_facts_in_one_commit() {
    let mut runner = SourceRunner::new_empty();
    runner
        .run_source("make_relation(:CurrentValue, 2, :volatile)")
        .unwrap();
    let subject = Identity::new(0x00eb_0000_0000_0042).unwrap();
    let relation = Symbol::intern("CurrentValue");
    let old = (
        relation,
        Tuple::from([Value::identity(subject), Value::int(1).unwrap()]),
    );
    let new = (
        relation,
        Tuple::from([Value::identity(subject), Value::int(2).unwrap()]),
    );
    runner
        .assert_volatile_tuples_named(vec![old.clone()])
        .unwrap();
    let version = runner.task_manager.kernel().snapshot().version();

    assert_eq!(
        runner
            .replace_volatile_tuples_named(vec![old], vec![new])
            .unwrap(),
        2
    );
    assert_eq!(
        runner.task_manager.kernel().snapshot().version(),
        version + 1
    );
    let report = runner
        .run_source("return CurrentValue(?subject, ?value)")
        .unwrap();
    assert_completed_value(
        &report,
        query_relation(
            ["subject", "value"],
            [[Value::identity(subject), Value::int(2).unwrap()]],
        ),
    );
}

#[test]
fn runner_assume_actor_requires_principal_specific_policy() {
    let mut runner = SourceRunner::new_empty();
    runner.run_source("make_identity(:account)").unwrap();
    runner.run_source("make_identity(:alice)").unwrap();
    runner.run_source("make_identity(:bob)").unwrap();
    runner
        .run_source("make_relation(:session/CanAssumeActor, 2)")
        .unwrap();
    let account = runner.actor_identity(Symbol::intern("account")).unwrap();
    let alice = runner.actor_identity(Symbol::intern("alice")).unwrap();
    let bob = runner.actor_identity(Symbol::intern("bob")).unwrap();
    let endpoint = Identity::new(0x00ee_0000_0000_0012).unwrap();
    runner
        .open_endpoint_with_context(
            endpoint,
            Some(account),
            Some(alice),
            Symbol::intern("telnet"),
        )
        .unwrap();

    let denied_request = runner
        .source_request_for_endpoint(endpoint, "return assume_actor(#bob)")
        .unwrap();
    let denied = runner.submit_source(denied_request).unwrap_err();
    assert!(format!("{denied:?}").contains("PermissionDenied"));

    runner
        .run_source("assert session/CanAssumeActor(#account, #bob)")
        .unwrap();
    let allowed_request = runner
        .source_request_for_endpoint(endpoint, "return assume_actor(#bob)")
        .unwrap();
    let switched = runner.submit_source(allowed_request).unwrap();
    assert!(matches!(
        switched.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::identity(bob)
    ));

    let actor_request = runner
        .source_request_for_endpoint(endpoint, "return actor()")
        .unwrap();
    let actor = runner.submit_source(actor_request).unwrap();
    assert!(matches!(
        actor.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::identity(bob)
    ));
}

#[test]
fn runner_assume_actor_rolls_back_with_aborted_task() {
    let mut runner = SourceRunner::new_empty();
    runner.run_source("make_identity(:account)").unwrap();
    runner.run_source("make_identity(:alice)").unwrap();
    runner.run_source("make_identity(:bob)").unwrap();
    runner
        .run_source(
            "make_relation(:session/CanAssumeActor, 2)\n\
             assert session/CanAssumeActor(#account, #bob)",
        )
        .unwrap();
    let account = runner.actor_identity(Symbol::intern("account")).unwrap();
    let alice = runner.actor_identity(Symbol::intern("alice")).unwrap();
    let endpoint = Identity::new(0x00ee_0000_0000_0014).unwrap();
    runner
        .open_endpoint_with_context(
            endpoint,
            Some(account),
            Some(alice),
            Symbol::intern("telnet"),
        )
        .unwrap();

    let request = runner
        .source_request_for_endpoint(
            endpoint,
            "assume_actor(#bob)\nraise E_INVARG, \"roll back actor\"",
        )
        .unwrap();
    let aborted = runner.submit_source(request).unwrap();
    assert!(matches!(
        aborted.outcome,
        TaskOutcome::Aborted { error, .. }
            if error.error_code_symbol() == Some(Symbol::intern("E_INVARG"))
    ));

    let actor_request = runner
        .source_request_for_endpoint(endpoint, "return actor()")
        .unwrap();
    let actor = runner.submit_source(actor_request).unwrap();
    assert!(matches!(
        actor.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::identity(alice)
    ));
}

#[test]
fn runner_routes_actor_effect_targets_to_open_endpoints() {
    let mut runner = SourceRunner::new_empty();
    runner.run_source("make_identity(:alice)").unwrap();
    let alice = runner.actor_identity(Symbol::intern("alice")).unwrap();
    let endpoint = Identity::new(0x00ee_0000_0000_0011).unwrap();

    assert_eq!(runner.route_effect_targets(alice), vec![alice]);
    runner
        .open_endpoint(endpoint, Some(alice), Symbol::intern("telnet"))
        .unwrap();
    assert_eq!(runner.route_effect_targets(alice), vec![endpoint]);
    assert_eq!(runner.route_effect_targets(endpoint), vec![endpoint]);

    runner.close_endpoint(endpoint);
    assert_eq!(runner.route_effect_targets(alice), vec![alice]);
}

#[test]
fn runner_destroy_identity_retracts_subject_facts_and_name_binding() {
    let mut runner = SourceRunner::new_empty();
    let thing = runner.run_source("return make_identity(:thing)").unwrap();
    let TaskOutcome::Complete {
        value: thing_value, ..
    } = thing.outcome
    else {
        panic!("make_identity did not complete");
    };
    let thing = thing_value.as_identity().unwrap();
    runner.run_source("make_identity(:room)").unwrap();
    runner.run_source("make_relation(:Object, 1)").unwrap();
    runner.run_source("make_relation(:LocatedIn, 2)").unwrap();
    runner.run_source("assert Object(#thing)").unwrap();
    runner.run_source("assert Object(#room)").unwrap();
    runner
        .run_source("assert LocatedIn(#thing, #room)")
        .unwrap();
    runner
        .run_source("assert LocatedIn(#room, #thing)")
        .unwrap();

    let destroyed = runner
        .run_source("return destroy_identity(#thing)")
        .unwrap();

    assert!(matches!(
        destroyed.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::int(3).unwrap()
    ));
    let snapshot = runner.task_manager.kernel().snapshot();
    assert!(
        snapshot
            .subject_facts(&Value::identity(thing))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        snapshot
            .mentioned_facts(&Value::identity(thing))
            .unwrap()
            .len(),
        1
    );
    assert!(
        format!("{:?}", runner.run_source("return #thing").unwrap_err())
            .contains("UnknownIdentity")
    );
    assert!(runner.run_source("return #room").is_ok());
}

#[test]
fn runner_read_waits_for_input_and_returns_continuation_value() {
    let mut runner = SourceRunner::new_empty();
    let submitted = runner
        .submit_source(TaskRequest {
            principal: None,
            actor: None,
            endpoint: SYSTEM_ENDPOINT,
            authority: AuthorityContext::root(),
            input: TaskInput::Source("return read(:line)".to_owned()),
        })
        .unwrap();
    assert!(matches!(
        submitted.outcome,
        TaskOutcome::Suspended {
            kind: SuspendKind::WaitingForInput(value),
            ..
        } if value == Value::symbol(Symbol::intern("line"))
    ));

    let outcome = runner
        .resume_task(TaskRequest {
            principal: None,
            actor: None,
            endpoint: SYSTEM_ENDPOINT,
            authority: AuthorityContext::root(),
            input: TaskInput::Continuation {
                task_id: submitted.task_id,
                value: Value::string("look"),
            },
        })
        .unwrap();

    assert!(matches!(
        outcome,
        TaskOutcome::Complete { value, .. } if value == Value::string("look")
    ));
}

#[test]
fn runner_suspend_seconds_becomes_timed_suspend() {
    let mut runner = SourceRunner::new_empty();
    let submitted = runner
        .submit_source(TaskRequest {
            principal: None,
            actor: None,
            endpoint: SYSTEM_ENDPOINT,
            authority: AuthorityContext::root(),
            input: TaskInput::Source("return suspend(0.5)".to_owned()),
        })
        .unwrap();

    assert!(matches!(
        submitted.outcome,
        TaskOutcome::Suspended {
            kind: SuspendKind::TimedMillis(500),
            ..
        }
    ));
}

#[test]
fn runner_aborts_on_divide_by_zero_before_builtin_effect() {
    let mut runner = SourceRunner::new_empty();
    runner.run_source("make_identity(:target)").unwrap();
    let report = runner.run_source("return emit(#target, 1 / 0)").unwrap();

    assert!(matches!(
        report.outcome,
        TaskOutcome::Aborted { error, effects, .. }
            if error.error_code_symbol() == Some(Symbol::intern("E_DIV"))
                && effects.is_empty()
    ));
}

fn task_status(value: &Value) -> Option<(i64, Symbol)> {
    let id = value
        .map_get(&Value::symbol(Symbol::intern("id")))?
        .as_int()?;
    let state = value
        .map_get(&Value::symbol(Symbol::intern("state")))?
        .as_symbol()?;
    Some((id, state))
}

#[test]
fn runner_make_relation_refreshes_compile_context() {
    let mut runner = SourceRunner::new_empty();
    let made = runner.run_source("return make_relation(:Hog, 1)").unwrap();
    assert_eq!(
        made.render(),
        "task 1 complete: relation(:Hog) (retries: 0)"
    );
    let relation = match made.outcome {
        TaskOutcome::Complete { value, .. } => value.as_identity().unwrap(),
        other => panic!("unexpected make_relation outcome: {other:?}"),
    };

    let asserted = runner.run_source("assert Hog(1)\nreturn true").unwrap();

    assert!(matches!(
        asserted.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::bool(true)
    ));
    assert_eq!(
        runner
            .task_manager
            .kernel()
            .snapshot()
            .scan(relation, &[Some(Value::int(1).unwrap())])
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn runner_same_source_body_can_use_declared_relation() {
    let mut runner = SourceRunner::new_empty();
    let report = runner
        .run_source(
            "make_relation(:Hog, 1)\n\
                 assert Hog(1)\n\
                 return Hog(?value)",
        )
        .unwrap();

    assert_eq!(
        report.render(),
        "task 1 complete: [:value] {[1]} (retries: 0)"
    );
}

#[test]
fn runner_same_source_body_can_use_declared_functional_relation_and_identity() {
    let mut runner = SourceRunner::new_empty();
    let report = runner
        .run_source(
            "make_identity(:thing)\n\
                 make_functional_relation(:Name, 2, [0])\n\
                 #thing.name = \"brass lamp\"\n\
                 return #thing.name",
        )
        .unwrap();

    assert_eq!(
        report.render(),
        "task 1 complete: \"brass lamp\" (retries: 0)"
    );
}

#[test]
fn runner_same_source_body_can_use_declared_identity_without_reusing_id() {
    let mut runner = SourceRunner::new_empty();
    let first = runner
        .run_source(
            "make_identity(:thing)\n\
                 return #thing",
        )
        .unwrap();
    let second = runner.run_source("return make_identity(:room)").unwrap();

    let TaskOutcome::Complete { value: first, .. } = first.outcome else {
        panic!("expected first identity");
    };
    let TaskOutcome::Complete { value: second, .. } = second.outcome else {
        panic!("expected second identity");
    };
    assert_ne!(first, second);
}

#[test]
fn runner_make_relation_is_idempotent_for_matching_arity() {
    let mut runner = SourceRunner::new_empty();
    let first = runner.run_source("return make_relation(:Hog, 1)").unwrap();
    let second = runner.run_source("return make_relation(:Hog, 1)").unwrap();

    assert!(matches!(
        (first.outcome, second.outcome),
        (
            TaskOutcome::Complete { value: first, .. },
            TaskOutcome::Complete { value: second, .. }
        ) if first == second
    ));
}

#[test]
fn runner_relation_durability_is_explicit_metadata() {
    let mut runner = SourceRunner::new_empty();
    runner
        .run_source(
            "make_relation(:Scratch, 1, :volatile)\n\
             make_functional_relation(:Cache, 2, [0], :volatile)\n\
             return true",
        )
        .unwrap();

    let snapshot = runner.task_manager.kernel().snapshot();
    for name in ["Scratch", "Cache"] {
        let metadata = snapshot
            .relation_metadata()
            .find(|metadata| metadata.name().name() == Some(name))
            .unwrap();
        assert_eq!(metadata.durability(), RelationDurability::Volatile);
    }
    let relation_durability = snapshot
        .relation_metadata()
        .find(|metadata| metadata.name().name() == Some("RelationDurability"))
        .unwrap();
    let scratch = snapshot
        .relation_metadata()
        .find(|metadata| metadata.name().name() == Some("Scratch"))
        .unwrap();
    assert_eq!(
        snapshot
            .scan(
                relation_durability.id(),
                &[Some(Value::identity(scratch.id())), None],
            )
            .unwrap(),
        vec![Tuple::from([
            Value::identity(scratch.id()),
            Value::symbol(Symbol::intern("volatile")),
        ])]
    );
}

#[test]
fn runner_volatile_relation_authority_is_relation_wide() {
    let mut runner = SourceRunner::new_empty();
    runner
        .run_source(
            "make_identity(:alice)\n\
             make_identity(:bob)\n\
             make_identity(:lamp)\n\
             make_identity(:book)\n\
             make_relation(:LiveSelection, 2, :volatile)\n\
             make_relation(:CanRead, 2)\n\
             assert CanRead(#alice, :LiveSelection)\n\
             assert CanRead(#bob, :LiveSelection)\n\
             assert LiveSelection(#alice, #lamp)\n\
             assert LiveSelection(#bob, #book)\n\
             return true",
        )
        .unwrap();
    let alice = runner.actor_identity(Symbol::intern("alice")).unwrap();
    let bob = runner.actor_identity(Symbol::intern("bob")).unwrap();
    let lamp = runner.actor_identity(Symbol::intern("lamp")).unwrap();
    let book = runner.actor_identity(Symbol::intern("book")).unwrap();
    let alice_endpoint = Identity::new(0x00ee_0000_0000_0020).unwrap();
    let bob_endpoint = Identity::new(0x00ee_0000_0000_0021).unwrap();

    for (actor, endpoint) in [(alice, alice_endpoint), (bob, bob_endpoint)] {
        let report = runner
            .submit_source_as(actor, endpoint, "return LiveSelection(?owner, ?item)")
            .unwrap();
        assert!(matches!(
            report.outcome,
            TaskOutcome::Complete { value, .. }
                if value == query_relation(
                    ["owner", "item"],
                    [
                        [Value::identity(alice), Value::identity(lamp)],
                        [Value::identity(bob), Value::identity(book)],
                    ],
                )
        ));
    }

    let own = runner
        .submit_source_as(alice, alice_endpoint, "return LiveSelection(#alice, ?item)")
        .unwrap();
    assert!(matches!(
        own.outcome,
        TaskOutcome::Complete { value, .. }
            if value == query_relation(["item"], [[Value::identity(lamp)]])
    ));
}

#[test]
fn runner_rejects_relation_durability_mismatch() {
    let mut runner = SourceRunner::new_empty();
    runner
        .run_source("return make_relation(:Scratch, 1, :volatile)")
        .unwrap();
    let error = runner
        .run_source("return make_relation(:Scratch, 1, :durable)")
        .unwrap_err();

    assert!(format!("{error:?}").contains("relation name already exists with different metadata"));
}

#[test]
fn runner_make_identity_refreshes_compile_context() {
    let mut runner = SourceRunner::new_empty();
    runner.run_source("make_relation(:Object, 1)").unwrap();
    let made = runner.run_source("return make_identity(:root)").unwrap();
    let root = match made.outcome {
        TaskOutcome::Complete { value, .. } => value.as_identity().unwrap(),
        other => panic!("unexpected make_identity outcome: {other:?}"),
    };

    let asserted = runner
        .run_source("assert Object(#root)\nreturn true")
        .unwrap();

    assert!(matches!(
        asserted.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::bool(true)
    ));
    assert_eq!(
        runner
            .task_manager
            .kernel()
            .snapshot()
            .scan(
                runner.context.relation("Object").unwrap(),
                &[Some(Value::identity(root))]
            )
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn report_renders_named_identities_in_values_and_effects() {
    let mut runner = SourceRunner::new_empty();
    let made = runner.run_source("return make_identity(:thing)").unwrap();
    let report = runner
        .run_source("return emit(#thing, [#thing, {:owner -> #thing}])")
        .unwrap();

    assert_eq!(made.render(), "task 1 complete: #thing (retries: 0)");
    assert_eq!(
        report.render(),
        "task 2 complete: [#thing, [:owner: #thing]] (retries: 0)\neffect #thing: [#thing, [:owner: #thing]]"
    );
}

#[test]
fn source_task_error_renders_named_identity_values() {
    let mut runner = SourceRunner::new_empty();
    let report = runner
        .run_source("return make_identity(:not_callable)")
        .unwrap();
    let identity = match report.outcome {
        TaskOutcome::Complete { value, .. } => value.as_identity().unwrap(),
        _ => panic!("identity creation did not complete"),
    };
    let error = SourceTaskError::TaskManager(TaskManagerError::Task(TaskError::Runtime(
        RuntimeError::InvalidCallable(Value::identity(identity)),
    )));

    assert_eq!(
        runner.render_source_task_error(&error),
        "task manager error: invalid callable #not_callable"
    );
}

#[test]
fn runner_relation_calls_with_query_vars_return_relation_values() {
    let mut runner = SourceRunner::new_empty();
    runner.run_source("make_identity(:thing)").unwrap();
    runner.run_source("make_identity(:room)").unwrap();
    runner.run_source("make_relation(:Location, 2)").unwrap();
    runner.run_source("assert Location(#thing, #room)").unwrap();

    let report = runner.run_source("return Location(#thing, ?room)").unwrap();

    assert_eq!(
        report.render(),
        "task 5 complete: [:room] {[#room]} (retries: 0)"
    );
}

#[test]
fn runner_relation_queries_allow_all_positions_free() {
    let mut runner = SourceRunner::new_empty();
    runner.run_source("make_identity(:thing)").unwrap();
    runner.run_source("make_identity(:room)").unwrap();
    runner.run_source("make_relation(:Location, 2)").unwrap();
    runner.run_source("assert Location(#thing, #room)").unwrap();

    let report = runner.run_source("return Location(?what, ?where)").unwrap();

    assert_eq!(
        report.render(),
        "task 5 complete: [:what, :where] {[#thing, #room]} (retries: 0)"
    );
}

#[test]
fn runner_relation_queries_unify_repeated_variables() {
    let mut runner = SourceRunner::new_empty();
    runner.run_source("make_relation(:Pair, 2)").unwrap();
    runner.run_source("assert Pair(1, 1)").unwrap();
    runner.run_source("assert Pair(1, 2)").unwrap();

    let report = runner.run_source("return Pair(?same, ?same)").unwrap();

    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. }
            if value == query_relation(["same"], [[Value::int(1).unwrap()]])
    ));
}

#[test]
fn runner_iterates_relation_rows_as_binding_maps() {
    let mut runner = SourceRunner::new_empty();
    runner.run_source("make_identity(:thing)").unwrap();
    runner.run_source("make_identity(:room)").unwrap();
    runner.run_source("make_relation(:Location, 2)").unwrap();
    runner.run_source("assert Location(#thing, #room)").unwrap();

    let report = runner
        .run_source(
            "let found = []\n\
             for row in Location(?thing, ?room)\n\
               found = [@found, row[:thing], row[:room]]\n\
             end\n\
             return found",
        )
        .unwrap();

    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. }
            if value == Value::list([
                Value::identity(runner.actor_identity(Symbol::intern("thing")).unwrap()),
                Value::identity(runner.actor_identity(Symbol::intern("room")).unwrap()),
            ])
    ));
}

#[test]
fn runner_one_returns_a_map_for_multi_column_relation_rows() {
    let mut runner = SourceRunner::new_empty();
    runner.run_source("make_identity(:thing)").unwrap();
    runner.run_source("make_identity(:room)").unwrap();
    runner.run_source("make_relation(:Location, 2)").unwrap();
    runner.run_source("assert Location(#thing, #room)").unwrap();
    let thing = runner.actor_identity(Symbol::intern("thing")).unwrap();
    let room = runner.actor_identity(Symbol::intern("room")).unwrap();

    let report = runner
        .run_source("return one Location(?thing, ?room)")
        .unwrap();

    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. }
            if value == Value::map([
                (Value::symbol(Symbol::intern("thing")), Value::identity(thing)),
                (Value::symbol(Symbol::intern("room")), Value::identity(room)),
            ])
    ));
}

#[test]
fn runner_one_rejects_ambiguous_relation_values() {
    let mut runner = SourceRunner::new_empty();
    runner.run_source("make_relation(:Number, 1)").unwrap();
    runner.run_source("assert Number(1)").unwrap();
    runner.run_source("assert Number(2)").unwrap();

    let report = runner.run_source("return one Number(?number)").unwrap();

    assert!(matches!(
        report.outcome,
        TaskOutcome::Aborted { error, .. }
            if error.error_code_symbol() == Some(Symbol::intern("E_AMBIGUOUS"))
                && error.with_error(|error| {
                    error.value() == Some(&query_relation(
                        ["number"],
                        [[Value::int(1).unwrap()], [Value::int(2).unwrap()]],
                    ))
                }) == Some(true)
    ));
}

#[test]
fn runner_applies_relation_algebra_to_query_values() {
    let mut runner = SourceRunner::new_empty();
    let report = runner
        .run_source(
            "make_relation(:Left, 2)\n\
             make_relation(:Right, 2)\n\
             make_relation(:More, 1)\n\
             assert Left(1, \"one\")\n\
             assert Left(2, \"two\")\n\
             assert Right(1, true)\n\
             assert Right(3, false)\n\
             assert More(2)\n\
             assert More(3)\n\
             let left = Left(?id, ?name)\n\
             let right = Right(?id, ?active)\n\
             let more = More(?id)\n\
             let joined = natural_join(left, right)\n\
             return [joined, project(joined, :name), union(project(left, :id), more), difference(project(left, :id), more)]",
        )
        .unwrap();

    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. }
            if value == Value::list([
                query_relation(
                    ["id", "name", "active"],
                    [[
                        Value::int(1).unwrap(),
                        Value::string("one"),
                        Value::bool(true),
                    ]],
                ),
                query_relation(["name"], [[Value::string("one")]]),
                query_relation(
                    ["id"],
                    [
                        [Value::int(1).unwrap()],
                        [Value::int(2).unwrap()],
                        [Value::int(3).unwrap()],
                    ],
                ),
                query_relation(["id"], [[Value::int(1).unwrap()]]),
            ])
    ));
}

#[test]
fn runner_relation_project_supports_zero_columns_and_reports_unknown_columns() {
    let mut runner = SourceRunner::new_empty();
    runner.run_source("make_relation(:Number, 1)").unwrap();
    runner.run_source("assert Number(1)").unwrap();

    let unit = runner
        .run_source("return project(Number(?number))")
        .unwrap();
    assert!(matches!(
        unit.outcome,
        TaskOutcome::Complete { value, .. } if value == query_relation([], [[]])
    ));

    let error = runner
        .run_source("return project(Number(?number), :missing)")
        .unwrap_err();
    assert!(format!("{error:?}").contains("relation has no column :missing"));
}

#[test]
fn runner_one_and_dot_read_project_functional_relations() {
    let mut runner = SourceRunner::new_empty();
    runner.run_source("make_identity(:thing)").unwrap();
    runner.run_source("make_identity(:room)").unwrap();
    runner
        .run_source("make_functional_relation(:Location, 2, [0])")
        .unwrap();
    runner.run_source("assert Location(#thing, #room)").unwrap();

    let one = runner
        .run_source("return one Location(#thing, ?room)")
        .unwrap();
    let dot = runner.run_source("return #thing.location").unwrap();

    assert_eq!(one.render(), "task 5 complete: #room (retries: 0)");
    assert_eq!(dot.render(), "task 6 complete: #room (retries: 0)");
}

#[test]
fn runner_namespaced_dot_read_and_assignment_project_functional_relations() {
    let mut runner = SourceRunner::new_empty();
    runner.run_source("make_identity(:endpoint)").unwrap();
    runner.run_source("make_identity(:alice)").unwrap();
    runner.run_source("make_identity(:bob)").unwrap();
    runner
        .run_source("make_functional_relation(:session/Actor, 2, [0])")
        .unwrap();

    let write = runner
        .run_source("#endpoint.session/actor = #alice")
        .unwrap();
    let read = runner.run_source("return #endpoint.session/actor").unwrap();
    runner.run_source("#endpoint.session/actor = #bob").unwrap();
    let replaced = runner.run_source("return #endpoint.session/actor").unwrap();

    assert_eq!(write.render(), "task 5 complete: #alice (retries: 0)");
    assert_eq!(read.render(), "task 6 complete: #alice (retries: 0)");
    assert_eq!(replaced.render(), "task 8 complete: #bob (retries: 0)");
}

#[test]
fn runner_rejects_dot_read_on_nonfunctional_relation() {
    let mut runner = SourceRunner::new_empty();
    runner.run_source("make_identity(:thing)").unwrap();
    runner.run_source("make_relation(:Location, 2)").unwrap();

    let error = runner.run_source("return #thing.location").unwrap_err();

    assert!(matches!(
        error,
        SourceTaskError::Compile(CompileError::Unsupported { message, .. })
            if message == "dot name `location` requires `Location` to be functional on position 0"
    ));
}

#[test]
fn runner_root_source_can_mix_method_definition_and_task_code() {
    let mut runner = SourceRunner::new_empty();
    let report = runner
        .run_source(
            "verb greet()\n\
                   return \"hello\"\n\
                 end\n\
                 return :greet()",
        )
        .unwrap();

    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::string("hello")
    ));
}

#[test]
fn runner_root_source_can_mix_rule_definition_and_task_code() {
    let mut runner = SourceRunner::new_empty();
    let report = runner
        .run_source(
            "make_identity(:alice)\n\
                 make_identity(:lamp)\n\
                 make_identity(:room)\n\
                 make_relation(:LocatedIn, 2)\n\
                 make_relation(:VisibleTo, 2)\n\
                 assert LocatedIn(#alice, #room)\n\
                 assert LocatedIn(#lamp, #room)\n\
                 VisibleTo(actor, obj) :-\n\
                   LocatedIn(actor, room),\n\
                   LocatedIn(obj, room)\n\
                 return VisibleTo(#alice, ?obj)",
        )
        .unwrap();

    assert_eq!(
        report.render(),
        "task 9 complete: [:obj] {[#alice], [#lamp]} (retries: 0)"
    );
}

#[test]
fn runner_installs_relation_rules_and_queries_derived_tuples() {
    let mut runner = SourceRunner::new_empty();
    runner.run_source("make_relation(:LocatedIn, 2)").unwrap();
    runner.run_source("make_relation(:VisibleTo, 2)").unwrap();
    let rule = runner
        .run_source("VisibleTo(actor, obj) :-\n  LocatedIn(actor, room),\n  LocatedIn(obj, room)")
        .unwrap();
    runner.run_source("make_identity(:alice)").unwrap();
    runner.run_source("make_identity(:lamp)").unwrap();
    runner.run_source("make_identity(:room)").unwrap();
    runner
        .run_source("assert LocatedIn(#alice, #room)")
        .unwrap();
    runner.run_source("assert LocatedIn(#lamp, #room)").unwrap();

    let query = runner.run_source("return VisibleTo(#alice, ?obj)").unwrap();

    assert_eq!(rule.render(), "task 3 complete: #rule1 (retries: 0)");
    assert_eq!(
        query.render(),
        "task 9 complete: [:obj] {[#alice], [#lamp]} (retries: 0)"
    );
}

#[test]
fn runner_installs_relation_rules_with_comparison_guards() {
    let mut runner = SourceRunner::new_empty();
    let report = runner
        .run_source(
            "make_identity(:current)\n\
                 make_identity(:rev1)\n\
                 make_identity(:rev2)\n\
                 make_identity(:file_a)\n\
                 make_identity(:file_b)\n\
                 make_relation(:IndexRevision, 2)\n\
                 make_relation(:FileRevision, 2)\n\
                 make_relation(:StaleFile, 2)\n\
                 assert IndexRevision(#current, #rev1)\n\
                 assert FileRevision(#file_a, #rev1)\n\
                 assert FileRevision(#file_b, #rev2)\n\
                 StaleFile(index, file) :-\n\
                   IndexRevision(index, index_revision),\n\
                   FileRevision(file, file_revision),\n\
                   index_revision != file_revision\n\
                 return StaleFile(#current, ?file)",
        )
        .unwrap();

    assert_eq!(
        report.render(),
        "task 13 complete: [:file] {[#file_b]} (retries: 0)"
    );
}

#[test]
fn runner_inspects_and_disables_rules() {
    let mut runner = SourceRunner::new_empty();
    runner.run_source("make_relation(:LocatedIn, 2)").unwrap();
    runner.run_source("make_relation(:VisibleTo, 2)").unwrap();
    runner
        .run_source("VisibleTo(actor, obj) :-\n  LocatedIn(actor, room),\n  LocatedIn(obj, room)")
        .unwrap();
    runner.run_source("make_identity(:alice)").unwrap();
    runner.run_source("make_identity(:lamp)").unwrap();
    runner.run_source("make_identity(:room)").unwrap();
    runner
        .run_source("assert LocatedIn(#alice, #room)")
        .unwrap();
    runner.run_source("assert LocatedIn(#lamp, #room)").unwrap();

    let rules = runner.run_source("return rules(:VisibleTo)").unwrap();
    let source = runner
        .run_source("return describe_rule(one rules(:VisibleTo))")
        .unwrap();
    let disabled = runner
        .run_source("disable_rule(one rules(:VisibleTo))")
        .unwrap();
    let query = runner.run_source("return VisibleTo(#alice, ?obj)").unwrap();

    assert_eq!(rules.render(), "task 9 complete: [#rule1] (retries: 0)");
    assert_eq!(
        source.render(),
        "task 10 complete: \"VisibleTo(actor, obj) :-\\n  LocatedIn(actor, room),\\n  LocatedIn(obj, room)\" (retries: 0)"
    );
    assert_eq!(disabled.render(), "task 11 complete: nothing (retries: 0)");
    assert_eq!(query.render(), "task 12 complete: [:obj] {} (retries: 0)");

    let unknown_relation = runner
        .run_source(
            "try
               return rules(:Missing)
             catch E_INVARG as err
               return [err.message, err.value]
             end",
        )
        .unwrap();
    assert!(matches!(
        unknown_relation.outcome,
        TaskOutcome::Complete { value, .. }
            if value == Value::list([
                Value::string("unknown relation :Missing"),
                Value::symbol(Symbol::intern("Missing")),
            ])
    ));
    runner.run_source("make_identity(:not_a_rule)").unwrap();
    let unknown_rule = runner
        .run_source(
            "try
               return describe_rule(#not_a_rule)
             catch E_INVARG as err
               return [err.message, err.value]
             end",
        )
        .unwrap();
    let not_a_rule = runner.named_identity(Symbol::intern("not_a_rule")).unwrap();
    assert!(matches!(
        unknown_rule.outcome,
        TaskOutcome::Complete { value, .. }
            if value == Value::list([
                Value::string("rule does not exist"),
                Value::identity(not_a_rule),
            ])
    ));
}

#[test]
fn runner_fileouts_active_rules() {
    let mut runner = SourceRunner::new_empty();
    runner.run_source("make_relation(:LocatedIn, 2)").unwrap();
    runner.run_source("make_relation(:VisibleTo, 2)").unwrap();
    runner
        .run_source("VisibleTo(actor, obj) :- LocatedIn(actor, obj)")
        .unwrap();

    let fileout = runner
        .run_source("return fileout_rules(:VisibleTo)")
        .unwrap();

    assert_eq!(
        fileout.render(),
        "task 4 complete: \"VisibleTo(actor, obj) :- LocatedIn(actor, obj)\" (retries: 0)"
    );

    let TaskOutcome::Complete { value, .. } = fileout.outcome else {
        panic!("expected fileout to complete");
    };
    let source = value.with_str(str::to_owned).unwrap();
    let mut imported = SourceRunner::new_empty();
    imported.run_source("make_relation(:LocatedIn, 2)").unwrap();
    imported.run_source("make_relation(:VisibleTo, 2)").unwrap();
    let installed = imported.run_source(&source).unwrap();
    assert_eq!(installed.render(), "task 3 complete: #rule1 (retries: 0)");

    let unknown_relation = runner
        .run_source(
            "try
               return fileout_rules(:Missing)
             catch E_INVARG as err
               return [err.message, err.value]
             end",
        )
        .unwrap();
    assert!(matches!(
        unknown_relation.outcome,
        TaskOutcome::Complete { value, .. }
            if value == Value::list([
                Value::string("unknown relation :Missing"),
                Value::symbol(Symbol::intern("Missing")),
            ])
    ));
}

#[test]
fn runner_queries_system_catalog_relations() {
    let mut runner = SourceRunner::new_empty();

    let report = runner
        .run_source("return one RelationName(?relation, :RelationName)")
        .unwrap();

    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::identity(relation_name_relation())
    ));
}

#[test]
fn runner_queries_identity_neighbourhood_relations() {
    let mut runner = SourceRunner::new_empty();
    runner
        .run_source(
            "make_identity(:coin)\n\
                 make_identity(:room)\n\
                 make_relation(:LocatedIn, 2)\n\
                 assert LocatedIn(#coin, #room)",
        )
        .unwrap();

    let report = runner
        .run_source(
            "let relation = one RelationName(?relation, :LocatedIn)\n\
                 return one MentionedFact(#coin, relation, 0, ?tuple)",
        )
        .unwrap();

    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. }
            if value == Value::list([Value::identity(runner.named_identity(Symbol::intern("coin")).unwrap()), Value::identity(runner.named_identity(Symbol::intern("room")).unwrap())])
    ));
}

#[test]
fn runner_filters_reflection_rows_by_underlying_relation_authority() {
    let mut runner = SourceRunner::new_empty();
    runner
        .run_source(
            "make_identity(:programmer)\n\
                 make_identity(:coin)\n\
                 make_identity(:room)\n\
                 make_relation(:CanRead, 2)\n\
                 make_relation(:LocatedIn, 2)\n\
                 make_relation(:Secret, 2)\n\
                 assert LocatedIn(#coin, #room)\n\
                 assert Secret(#coin, \"hidden\")\n\
                 assert CanRead(#programmer, :MentionedFact)\n\
                 assert CanRead(#programmer, :RelationName)\n\
                 assert CanRead(#programmer, :LocatedIn)",
        )
        .unwrap();

    let report = runner
        .run_source_as(
            Symbol::intern("programmer"),
            "let located = one RelationName(?located, :LocatedIn)\n\
                 let secret = one RelationName(?secret, :Secret)\n\
                 let visible = MentionedFact(#coin, located, ?position, ?tuple)\n\
                 let hidden = MentionedFact(#coin, secret, ?hidden_position, ?hidden_tuple)\n\
                 return [visible, hidden]",
        )
        .unwrap();

    let TaskOutcome::Complete { value, .. } = report.outcome else {
        panic!("expected reflection query to complete");
    };
    value
        .with_list(|results| {
            assert_eq!(results.len(), 2);
            assert_eq!(results[0].with_relation(|relation| relation.len()), Some(1));
            assert_eq!(results[1].with_relation(|relation| relation.len()), Some(0));
        })
        .expect("expected result list");
}

#[test]
fn runner_queries_method_source_as_relation() {
    let mut runner = SourceRunner::new_empty();
    runner.run_source("make_identity(:thing)").unwrap();
    runner
        .run_source(
            "verb inspect(target @ #thing)\n\
                   return target\n\
                 end",
        )
        .unwrap();
    let report = runner
        .run_source(
            "let rows = MethodSource(?m, ?s)\n\
                 return rows",
        )
        .unwrap();

    assert!(report.render().contains("verb inspect"));
}

#[test]
fn runner_rejects_writes_to_system_relations() {
    let mut runner = SourceRunner::new_empty();
    runner.run_source("make_identity(:thing)").unwrap();

    let error = runner
        .run_source("assert SubjectFact(#thing, :bogus, [])")
        .unwrap_err();

    assert!(
        format!("{error:?}").contains(&format!("ReadOnlyRelation({:?})", subject_fact_relation()))
    );
}

#[test]
fn runner_filein_unit_fileout_round_trips_readable_source() {
    let mut runner = SourceRunner::new_empty();
    let unit = Symbol::intern("mud_core");
    runner
        .run_filein_with_unit(
            unit,
            "make_identity(:lamp)\n\
                 make_identity(:room)\n\
                 make_relation(:Name, 2)\n\
                 make_relation(:LocatedIn, 2)\n\
                 make_relation(:VisibleTo, 2)\n\
                 assert Name(#lamp, \"brass lamp\")\n\
                 assert LocatedIn(#lamp, #room)\n\
                 VisibleTo(actor, obj) :- LocatedIn(obj, actor)\n\
                 verb look(actor @ #room)\n\
                   return \"ok\"\n\
                 end\n",
            FileinMode::Add,
        )
        .unwrap();

    let source = runner.fileout_unit(unit).unwrap();

    assert!(source.contains("make_identity(:lamp)"));
    assert!(source.contains("make_relation(:Name, 2)"));
    assert!(source.contains("assert Name(#lamp, \"brass lamp\")"));
    assert!(source.contains("VisibleTo(actor, obj) :- LocatedIn(obj, actor)"));
    assert!(source.contains("verb look(actor @ #room)"));

    let mut imported = SourceRunner::new_empty();
    imported
        .run_filein_with_unit(unit, &source, FileinMode::Add)
        .unwrap();
    let query = imported.run_source("return Name(#lamp, ?name)").unwrap();
    let dispatch = imported.run_source("return :look(actor: #room)").unwrap();
    assert!(query.render().contains("[:name] {[\"brass lamp\"]}"));
    assert!(dispatch.render().contains("\"ok\""));
}

#[test]
fn runner_fileout_round_trips_value_kind_annotations() {
    let mut runner = SourceRunner::new_empty();
    let unit = Symbol::intern("typed_verbs");
    runner
        .run_filein_with_unit(
            unit,
            "verb typed_echo(value @ #string: string) -> string\n\
               let copy: string = value\n\
               return copy\n\
             end\n",
            FileinMode::Add,
        )
        .unwrap();

    let source = runner.fileout_unit(unit).unwrap();
    assert!(source.contains("verb typed_echo(value @ #string: string) -> string"));
    assert!(source.contains("let copy: string = value"));

    let mut imported = SourceRunner::new_empty();
    imported
        .run_filein_with_unit(unit, &source, FileinMode::Add)
        .unwrap();
    let report = imported
        .run_source("return :typed_echo(value: \"round trip\")")
        .unwrap();
    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::string("round trip")
    ));
}

#[test]
fn runner_fileout_preserves_frob_fact_literals() {
    let mut runner = SourceRunner::new_empty();
    let unit = Symbol::intern("events");
    runner
        .run_filein_with_unit(
            unit,
            "make_identity(:take_event)\n\
                 make_relation(:CompiledEvent, 1)\n\
                 assert CompiledEvent(#take_event<{:item -> \"coin\"}>)\n",
            FileinMode::Add,
        )
        .unwrap();

    let source = runner.fileout_unit(unit).unwrap();

    assert!(source.contains("assert CompiledEvent(#take_event<{:item -> \"coin\"}>)"));
    let mut imported = SourceRunner::new_empty();
    imported
        .run_filein_with_unit(unit, &source, FileinMode::Add)
        .unwrap();
    let query = imported
        .run_source("return frob_value(one CompiledEvent(?event))[:item]")
        .unwrap();
    assert!(matches!(
        query.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::string("coin")
    ));
}

#[test]
fn runner_fileout_preserves_slash_qualified_names() {
    let mut runner = SourceRunner::new_empty();
    let unit = Symbol::intern("ui");
    runner
        .run_filein_with_unit(
            unit,
            "make_identity(:ui/alice)\n\
                 make_identity(:ui/lamp)\n\
                 make_functional_relation(:ui/Name, 2, [0])\n\
                 make_relation(:ui/Visible, 2)\n\
                 make_relation(:ui/CanSee, 2)\n\
                 assert ui/Name(#ui/lamp, \"brass lamp\")\n\
                 assert ui/Visible(#ui/alice, #ui/lamp)\n\
                 ui/CanSee(actor, obj) :- ui/Visible(actor, obj)\n\
                 verb ui/look(actor, item)\n\
                   return one ui/Name(item, ?name)\n\
                 end\n",
            FileinMode::Add,
        )
        .unwrap();

    let source = runner.fileout_unit(unit).unwrap();

    assert!(source.contains("make_identity(:ui/lamp)"));
    assert!(source.contains("make_functional_relation(:ui/Name, 2, [0])"));
    assert!(source.contains("assert ui/Name(#ui/lamp, \"brass lamp\")"));
    assert!(source.contains("ui/CanSee(actor, obj) :- ui/Visible(actor, obj)"));
    assert!(source.contains("verb ui/look(actor, item)"));

    let mut imported = SourceRunner::new_empty();
    imported
        .run_filein_with_unit(unit, &source, FileinMode::Add)
        .unwrap();
    let query = imported
        .run_source("return ui/Name(#ui/lamp, ?name)")
        .unwrap();
    let dispatch = imported
        .run_source("return :ui/look(actor: #ui/alice, item: #ui/lamp)")
        .unwrap();
    assert!(query.render().contains("[:name] {[\"brass lamp\"]}"));
    assert!(dispatch.render().contains("\"brass lamp\""));
}

#[test]
fn runner_filein_grant_blocks_fileout_as_grant_blocks() {
    let mut runner = SourceRunner::new_empty();
    let unit = Symbol::intern("policy");
    runner
        .run_filein_with_unit(
            unit,
            "make_identity(:web)\n\
                 make_identity(:player)\n\
                 make_relation(:CanRead, 2)\n\
                 make_relation(:CanWrite, 2)\n\
                 make_relation(:CanInvoke, 2)\n\
                 make_relation(:CanEffect, 1)\n\
                 make_relation(:RoleCanRead, 2)\n\
                 make_relation(:RoleCanInvoke, 2)\n\
                 make_relation(:RoleCanEffect, 1)\n\
\n\
                 grant #web\n\
                   read:\n\
                     :HttpRequest\n\
                     :source/RuntimeConfig\n\
                   write :source/RuntimeConfig\n\
                   invoke:\n\
                     :http_request\n\
                     :source/http_document\n\
                   effect\n\
                 end\n\
\n\
                 grant role #player\n\
                   read :Name, :Description\n\
                   invoke:\n\
                     :look\n\
                   effect\n\
                 end\n",
            FileinMode::Add,
        )
        .unwrap();

    let read = runner
        .run_source("return CanRead(#web, :source/RuntimeConfig)")
        .unwrap();
    let invoke = runner
        .run_source("return RoleCanInvoke(#player, :look)")
        .unwrap();
    assert_relation_query_is_true(&read);
    assert_relation_query_is_true(&invoke);

    let source = runner.fileout_unit(unit).unwrap();
    assert!(source.contains("grant #web"));
    assert!(source.contains("  read:\n    :HttpRequest\n    :source/RuntimeConfig"));
    assert!(source.contains("  write:\n    :source/RuntimeConfig"));
    assert!(source.contains("  invoke:\n    :http_request\n    :source/http_document"));
    assert!(source.contains("  effect"));
    assert!(source.contains("grant role #player"));
    assert!(!source.contains("assert CanRead(#web"));
    assert!(!source.contains("assert RoleCanInvoke(#player"));

    let mut imported = SourceRunner::new_empty();
    imported
        .run_filein_with_unit(unit, &source, FileinMode::Add)
        .unwrap();
    let read = imported
        .run_source("return CanRead(#web, :HttpRequest)")
        .unwrap();
    let effect = imported.run_source("return CanEffect(#web)").unwrap();
    assert_relation_query_is_true(&read);
    assert_relation_query_is_true(&effect);
}

#[test]
fn runner_filein_include_text_compiles_and_fileout_preserves_reference() {
    let mut runner = SourceRunner::new_empty();
    let unit = Symbol::intern("web_assets");
    let source = "verb page_style()\n\
                      return include_text(\"style.css\")\n\
                    end\n";
    let css = "body { color: #f5f0e8; }\nbutton::before { content: \"go\"; }\n";
    runner
        .run_filein_with_unit_and_include_loader(unit, source, FileinMode::Add, |path| {
            if path == "style.css" {
                Ok(css.to_owned())
            } else {
                Err(format!("unexpected include {path}"))
            }
        })
        .unwrap();

    let report = runner.run_source("return :page_style()").unwrap();
    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::string(css)
    ));

    let filed_out = runner.fileout_unit(unit).unwrap();
    assert!(filed_out.contains("include_text(\"style.css\")"));
    assert!(!filed_out.contains("button::before"));

    let mut imported = SourceRunner::new_empty();
    imported
        .run_filein_with_unit_and_include_loader(unit, &filed_out, FileinMode::Add, |_| {
            Ok(css.to_owned())
        })
        .unwrap();
    let report = imported.run_source("return :page_style()").unwrap();
    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::string(css)
    ));
}

#[test]
fn runner_filein_replace_removes_facts_no_longer_in_source_unit() {
    let mut runner = SourceRunner::new_empty();
    let unit = Symbol::intern("mud_core");
    runner
        .run_filein_with_unit(
            unit,
            "make_identity(:lamp)\n\
                 make_relation(:Name, 2)\n\
                 assert Name(#lamp, \"brass lamp\")\n",
            FileinMode::Add,
        )
        .unwrap();
    let version_before_replace = runner.task_manager.kernel().snapshot().version();
    runner
        .run_filein_with_unit(
            unit,
            "make_identity(:lamp)\n\
                 make_relation(:Name, 2)\n\
                 assert Name(#lamp, \"golden lamp\")\n",
            FileinMode::Replace,
        )
        .unwrap();
    assert_eq!(
        runner.task_manager.kernel().snapshot().version(),
        version_before_replace + 1
    );

    let query = runner.run_source("return Name(#lamp, ?name)").unwrap();
    let source = runner.fileout_unit(unit).unwrap();

    assert!(query.render().contains("[:name] {[\"golden lamp\"]}"));
    assert!(source.contains("assert Name(#lamp, \"golden lamp\")"));
    assert!(!source.contains("brass lamp"));
}

#[test]
fn runner_failed_filein_replacement_preserves_source_unit() {
    let mut runner = SourceRunner::new_empty();
    let unit = Symbol::intern("equipment");
    runner
        .run_filein_with_unit(
            unit,
            "make_identity(:sensor)\n\
                 make_relation(:Label, 2)\n\
                 make_relation(:Operational, 1)\n\
                 assert Label(#sensor, \"original\")\n\
                 Operational(item) :- Label(item, ?label)\n\
                 verb equipment_label(item)\n\
                   return one Label(item, ?label)\n\
                 end\n",
            FileinMode::Add,
        )
        .unwrap();
    let source_before = runner.fileout_unit(unit).unwrap();
    let version_before = runner.task_manager.kernel().snapshot().version();

    let error = runner
        .run_filein_with_unit(
            unit,
            "make_identity(:sensor)\n\
                 make_relation(:Label, 2)\n\
                 assert Label(#sensor, \"replacement\")\n\
                 raise E_INVARG, \"replacement failed\"\n",
            FileinMode::Replace,
        )
        .unwrap_err();

    assert!(matches!(
        error,
        SourceTaskError::TaskManager(TaskManagerError::Task(TaskError::Runtime(
            RuntimeError::Raised(_)
        )))
    ));
    assert_eq!(
        runner.task_manager.kernel().snapshot().version(),
        version_before
    );
    assert_eq!(runner.fileout_unit(unit).unwrap(), source_before);
    let query = runner.run_source("return Label(#sensor, ?label)").unwrap();
    let derived = runner.run_source("return Operational(#sensor)").unwrap();
    let invocation = runner
        .run_source("return :equipment_label(item: #sensor)")
        .unwrap();
    assert!(query.render().contains("original"));
    assert_relation_query_is_true(&derived);
    assert_completed_value(&invocation, Value::string("original"));

    let parse_error = runner
        .run_filein_with_unit(unit, "make_identity(:sensor\n", FileinMode::Replace)
        .unwrap_err();
    assert!(matches!(
        parse_error,
        SourceTaskError::Compile(CompileError::ParseErrors { .. })
    ));
    assert_eq!(
        runner.task_manager.kernel().snapshot().version(),
        version_before
    );
    assert_eq!(runner.fileout_unit(unit).unwrap(), source_before);
}

#[test]
fn runner_failed_filein_include_preserves_source_unit() {
    let mut runner = SourceRunner::new_empty();
    let unit = Symbol::intern("web_assets");
    runner
        .run_filein_with_unit(
            unit,
            "verb page_style()\n\
               return \"original css\"\n\
             end\n",
            FileinMode::Add,
        )
        .unwrap();
    let source_before = runner.fileout_unit(unit).unwrap();
    let version_before = runner.task_manager.kernel().snapshot().version();

    let error = runner
        .run_filein_with_unit_and_include_loader(
            unit,
            "verb page_style()\n\
               return include_text(\"missing.css\")\n\
             end\n",
            FileinMode::Replace,
            |_| Err("asset unavailable".to_owned()),
        )
        .unwrap_err();

    assert!(format!("{error:?}").contains("asset unavailable"));
    assert_eq!(
        runner.task_manager.kernel().snapshot().version(),
        version_before
    );
    assert_eq!(runner.fileout_unit(unit).unwrap(), source_before);
    let invocation = runner.run_source("return :page_style()").unwrap();
    assert_completed_value(&invocation, Value::string("original css"));
}

#[test]
#[cfg(feature = "fjall")]
fn runner_fjall_filein_replacement_reopens_atomic_state() {
    let path = std::env::temp_dir().join(format!(
        "mica-runtime-fjall-{}-{}",
        std::process::id(),
        Symbol::intern("runner_fjall_filein_replacement_reopens_atomic_state").id()
    ));
    let _ = std::fs::remove_dir_all(&path);
    let unit = Symbol::intern("equipment");

    {
        let mut runner =
            SourceRunner::open_fjall(&path, mica_relation_kernel::FjallDurabilityMode::Strict)
                .unwrap();
        runner
            .run_filein_with_unit(
                unit,
                "make_identity(:sensor)\n\
                 make_relation(:Label, 2)\n\
                 make_relation(:Ready, 1)\n\
                 assert Label(#sensor, \"original\")\n\
                 Ready(item) :- Label(item, \"original\")\n\
                 verb equipment_label(item)\n\
                   return one Label(item, ?label)\n\
                 end\n",
                FileinMode::Add,
            )
            .unwrap();
        let version_before = runner.task_manager.kernel().snapshot().version();
        runner
            .run_filein_with_unit(
                unit,
                "make_identity(:sensor)\n\
                 make_relation(:Label, 2)\n\
                 make_relation(:Ready, 1)\n\
                 make_relation(:CalibrationState, 2)\n\
                 assert Label(#sensor, \"replacement\")\n\
                 assert CalibrationState(#sensor, :current)\n\
                 Ready(item) :- Label(item, \"replacement\")\n\
                 verb equipment_label(item)\n\
                   return one Label(item, ?label)\n\
                 end\n",
                FileinMode::Replace,
            )
            .unwrap();
        assert_eq!(
            runner.task_manager.kernel().snapshot().version(),
            version_before + 1
        );
    }

    {
        let mut runner =
            SourceRunner::open_fjall(&path, mica_relation_kernel::FjallDurabilityMode::Strict)
                .unwrap();
        let label = runner.run_source("return Label(#sensor, ?label)").unwrap();
        let calibration = runner
            .run_source("return CalibrationState(#sensor, ?state)")
            .unwrap();
        let ready = runner.run_source("return Ready(#sensor)").unwrap();
        let label_from_verb = runner
            .run_source("return :equipment_label(item: #sensor)")
            .unwrap();
        let source = runner.fileout_unit(unit).unwrap();
        assert!(label.render().contains("replacement"));
        assert!(calibration.render().contains(":current"));
        assert_relation_query_is_true(&ready);
        assert_completed_value(&label_from_verb, Value::string("replacement"));
        assert!(source.contains("make_relation(:CalibrationState, 2)"));
        assert!(!source.contains("original"));
    }

    let _ = std::fs::remove_dir_all(&path);
}

#[test]
fn shared_runner_installs_replaces_checks_and_files_out_units() {
    let shared = SourceRunner::new_empty().into_shared();
    let unit = Symbol::intern("equipment");
    shared
        .run_filein_with_unit_and_include_loader(
            unit,
            "make_identity(:sensor)\n\
             make_relation(:Label, 2)\n\
             assert Label(#sensor, include_text(\"label.txt\"))\n\
             verb equipment_label(item)\n\
               return one Label(item, ?label)\n\
             end\n",
            FileinMode::Add,
            |path| match path {
                "label.txt" => Ok("original".to_owned()),
                _ => Err(format!("unknown include {path}")),
            },
        )
        .unwrap();
    let initial = shared
        .submit_root_source("return :equipment_label(item: #sensor)")
        .unwrap();
    assert!(matches!(
        initial.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::string("original")
    ));

    let checked = shared.check_filein("make_identity(:checked_only)").unwrap();
    assert_eq!(checked.len(), 1);
    assert!(
        shared
            .named_identity(Symbol::intern("checked_only"))
            .is_err()
    );

    shared
        .run_filein_with_unit(
            unit,
            "make_identity(:sensor)\n\
             make_relation(:Label, 2)\n\
             assert Label(#sensor, \"replacement\")\n\
             verb equipment_label(item)\n\
               return one Label(item, ?label)\n\
             end\n",
            FileinMode::Replace,
        )
        .unwrap();
    let replacement = shared
        .submit_root_source("return :equipment_label(item: #sensor)")
        .unwrap();
    assert!(matches!(
        replacement.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::string("replacement")
    ));
    let filed_out = shared.fileout_unit(unit).unwrap();
    assert!(filed_out.contains("replacement"));
    assert!(!filed_out.contains("original"));
}

#[test]
#[cfg(feature = "fjall")]
fn runner_fjall_failed_filein_replacement_reopens_original_state() {
    let path = std::env::temp_dir().join(format!(
        "mica-runtime-fjall-{}-{}",
        std::process::id(),
        Symbol::intern("runner_fjall_failed_filein_replacement_reopens_original_state").id()
    ));
    let _ = std::fs::remove_dir_all(&path);
    let unit = Symbol::intern("equipment");

    {
        let mut runner =
            SourceRunner::open_fjall(&path, mica_relation_kernel::FjallDurabilityMode::Strict)
                .unwrap();
        runner
            .run_filein_with_unit(
                unit,
                "make_identity(:sensor)\n\
                 make_relation(:Label, 2)\n\
                 assert Label(#sensor, \"original\")\n",
                FileinMode::Add,
            )
            .unwrap();
        let version_before = runner.task_manager.kernel().snapshot().version();
        runner
            .run_filein_with_unit(
                unit,
                "make_identity(:sensor)\n\
                 make_relation(:Label, 2)\n\
                 assert Label(#sensor, \"replacement\")\n\
                 raise E_INVARG, \"replacement failed\"\n",
                FileinMode::Replace,
            )
            .unwrap_err();
        assert_eq!(
            runner.task_manager.kernel().snapshot().version(),
            version_before
        );
    }

    {
        let mut runner =
            SourceRunner::open_fjall(&path, mica_relation_kernel::FjallDurabilityMode::Strict)
                .unwrap();
        let label = runner.run_source("return Label(#sensor, ?label)").unwrap();
        let source = runner.fileout_unit(unit).unwrap();
        assert!(label.render().contains("original"));
        assert!(source.contains("original"));
        assert!(!source.contains("replacement"));
    }

    let _ = std::fs::remove_dir_all(&path);
}

#[test]
#[cfg(feature = "fjall")]
fn runner_fjall_store_reopens_state() {
    let path = std::env::temp_dir().join(format!(
        "mica-runtime-fjall-{}-{}",
        std::process::id(),
        Symbol::intern("runner_fjall_store_reopens_state").id()
    ));
    let _ = std::fs::remove_dir_all(&path);

    {
        let mut runner =
            SourceRunner::open_fjall(&path, mica_relation_kernel::FjallDurabilityMode::Strict)
                .unwrap();
        runner.run_source("make_identity(:lamp)").unwrap();
        runner.run_source("make_relation(:Name, 2)").unwrap();
        runner
            .run_source("assert Name(#lamp, \"brass lamp\")")
            .unwrap();
        let lamp = runner.actor_identity(Symbol::intern("lamp")).unwrap();
        let endpoint = Identity::new(0x00ee_0000_0000_0030).unwrap();
        runner
            .open_endpoint(endpoint, Some(lamp), Symbol::intern("test"))
            .unwrap();
        let endpoints = runner.run_source("return EndpointOpen(?endpoint)").unwrap();
        assert!(matches!(
            endpoints.outcome,
            TaskOutcome::Complete { value, .. }
                if value == query_relation(["endpoint"], [[Value::identity(endpoint)]])
        ));
    }

    {
        let mut runner =
            SourceRunner::open_fjall(&path, mica_relation_kernel::FjallDurabilityMode::Strict)
                .unwrap();
        let query = runner.run_source("return Name(#lamp, ?name)").unwrap();
        assert!(query.render().contains("[:name] {[\"brass lamp\"]}"));
        let endpoints = runner.run_source("return EndpointOpen(?endpoint)").unwrap();
        assert!(matches!(
            endpoints.outcome,
            TaskOutcome::Complete { value, .. }
                if value == query_relation(["endpoint"], [])
        ));
    }

    let _ = std::fs::remove_dir_all(&path);
}

#[test]
fn runner_installs_rules_with_surface_negation() {
    let mut runner = SourceRunner::new_empty();
    runner.run_source("make_relation(:LocatedIn, 2)").unwrap();
    runner.run_source("make_relation(:HiddenFrom, 2)").unwrap();
    runner.run_source("make_relation(:VisibleTo, 2)").unwrap();
    runner
            .run_source(
                "VisibleTo(actor, obj) :-\n  LocatedIn(actor, room),\n  LocatedIn(obj, room),\n  not HiddenFrom(obj, actor)",
            )
            .unwrap();
    runner.run_source("make_identity(:alice)").unwrap();
    runner.run_source("make_identity(:lamp)").unwrap();
    runner.run_source("make_identity(:room)").unwrap();
    runner
        .run_source("assert LocatedIn(#alice, #room)")
        .unwrap();
    runner.run_source("assert LocatedIn(#lamp, #room)").unwrap();
    runner
        .run_source("assert HiddenFrom(#lamp, #alice)")
        .unwrap();

    let query = runner.run_source("return VisibleTo(#alice, ?obj)").unwrap();

    assert_eq!(
        query.render(),
        "task 11 complete: [:obj] {[#alice]} (retries: 0)"
    );
}

#[test]
fn runner_equipment_guide_example_stays_executable() {
    let mut runner = SourceRunner::new_empty();
    runner
        .run_filein(include_str!(
            "../../../apps/examples/equipment-service.mica"
        ))
        .unwrap();

    assert_completed_value(
        &runner
            .run_source("return CanCollect(#bob, #spectrometer_2)")
            .unwrap(),
        Value::bool(true),
    );
    assert_completed_value(
        &runner
            .run_source("return CanCollect(#alice, #sensor_17)")
            .unwrap(),
        Value::bool(false),
    );
    assert_completed_value(
        &runner
            .run_source("return BlockedProject(#air_quality_project, #sensor_17)")
            .unwrap(),
        Value::bool(true),
    );

    assert_completed_value(
        &runner
            .run_source("return :record_calibration(actor: #alice, instrument: #sensor_17)")
            .unwrap(),
        Value::symbol(Symbol::intern("calibrated")),
    );
    assert_completed_value(
        &runner.run_source("return ReadyForUse(#sensor_17)").unwrap(),
        Value::bool(true),
    );
    assert_completed_value(
        &runner
            .run_source(
                "return :transfer(actor: #alice, instrument: #sensor_17, destination: #north_office)",
            )
            .unwrap(),
        Value::symbol(Symbol::intern("transferred")),
    );
    assert_completed_value(
        &runner
            .run_source("return CanCollect(#bob, #sensor_17)")
            .unwrap(),
        Value::bool(true),
    );
}

#[test]
fn runner_approval_guide_example_stays_executable() {
    let mut runner = SourceRunner::new_empty();
    runner
        .run_filein(include_str!(
            "../../../apps/examples/approval-workflow.mica"
        ))
        .unwrap();

    assert_completed_value(
        &runner
            .run_source("return CanApprove(#alice, #office_supplies_request)")
            .unwrap(),
        Value::bool(true),
    );
    assert_completed_value(
        &runner
            .run_source("return CanApprove(#alice, #lab_upgrade_request)")
            .unwrap(),
        Value::bool(false),
    );
    assert_completed_value(
        &runner
            .run_source(
                "return :approve(actor: #alice, request: #lab_upgrade_request, note: \"approved\")",
            )
            .unwrap(),
        Value::symbol(Symbol::intern("not_eligible")),
    );
    assert_completed_value(
        &runner
            .run_source("return Pending(#lab_upgrade_request)")
            .unwrap(),
        Value::bool(true),
    );

    assert_completed_value(
        &runner
            .run_source(
                "return :approve(actor: #sam, request: #lab_upgrade_request, note: \"budget confirmed\")",
            )
            .unwrap(),
        Value::symbol(Symbol::intern("approved")),
    );
    assert_completed_value(
        &runner
            .run_source("return Approved(#lab_upgrade_request)")
            .unwrap(),
        Value::bool(true),
    );
    assert_completed_value(
        &runner
            .run_source("return #lab_upgrade_request.approvedBy")
            .unwrap(),
        runner.named_identity(Symbol::intern("sam")).unwrap().into(),
    );
}

#[test]
fn runner_dependency_guide_example_stays_executable() {
    let mut runner = SourceRunner::new_empty();
    runner
        .run_filein(include_str!(
            "../../../apps/examples/dependency-planner.mica"
        ))
        .unwrap();

    assert_completed_value(
        &runner
            .run_source("return DependsOn(#web_frontend, #database)")
            .unwrap(),
        Value::bool(true),
    );
    assert_completed_value(
        &runner
            .run_source("return :mark_unavailable(actor: #olivia, component: #database)")
            .unwrap(),
        Value::symbol(Symbol::intern("marked_unavailable")),
    );
    for component in ["web_frontend", "api_service", "job_worker", "database"] {
        assert_completed_value(
            &runner
                .run_source(&format!("return Affected(#{component})"))
                .unwrap(),
            Value::bool(true),
        );
    }

    assert_completed_value(
        &runner
            .run_source("return :restore(actor: #olivia, component: #database)")
            .unwrap(),
        Value::symbol(Symbol::intern("restored")),
    );
    assert_completed_value(
        &runner.run_source("return Affected(#web_frontend)").unwrap(),
        Value::bool(false),
    );
}

#[test]
fn runner_filein_installs_mud_verbs_and_invokes_dispatch() {
    let mut runner = SourceRunner::new_empty();
    let reports = runner
        .run_filein(
            "make_identity(:player)\n\
                 make_identity(:thing)\n\
                 make_identity(:portable)\n\
                 make_identity(:container)\n\
                 make_identity(:alice)\n\
                 make_identity(:coin)\n\
                 make_identity(:box)\n\
                 make_relation(:Delegates, 3)\n\
                 make_relation(:HeldBy, 2)\n\
                 make_relation(:In, 2)\n\
                 make_relation(:Portable, 1)\n\
                 make_relation(:CanSee, 2)\n\
                 assert Delegates(#portable, #thing, 0)\n\
                 assert Delegates(#coin, #portable, 0)\n\
                 assert Delegates(#alice, #player, 0)\n\
                 assert Delegates(#box, #container, 0)\n\
                 assert Portable(#coin)\n\
                 CanSee(actor, item) :-\n\
                   HeldBy(actor, item)\n\
                 CanSee(actor, item) :-\n\
                   HeldBy(actor, container),\n\
                   In(item, container)\n\
                 verb get(actor @ #player, item @ #thing)\n\
                   if Portable(item)\n\
                     assert HeldBy(actor, item)\n\
                     return true\n\
                   else\n\
                     return false\n\
                   end\n\
                 end\n\
                 verb put(actor @ #player, item @ #thing, container @ #container)\n\
                   if HeldBy(actor, item)\n\
                     assert In(item, container)\n\
                     return true\n\
                   else\n\
                     return false\n\
                   end\n\
                 end\n\
                 :get(item: #coin, actor: #alice)\n\
                 :put(container: #box, item: #coin, actor: #alice)\n\
                 return In(#coin, ?container)\n\
                 return CanSee(#alice, ?item)\n",
        )
        .unwrap();

    assert_eq!(
        reports[17].render(),
        "task 18 complete: #rule1 (retries: 0)"
    );
    assert_eq!(
        reports[18].render(),
        "task 19 complete: #rule2 (retries: 0)"
    );
    assert_eq!(
        reports[19].render(),
        "task 20 complete: #verb_get_1 (retries: 0)"
    );
    assert_eq!(
        reports[20].render(),
        "task 21 complete: #verb_put_2 (retries: 0)"
    );
    assert_eq!(reports[21].render(), "task 22 complete: true (retries: 0)");
    assert_eq!(reports[22].render(), "task 23 complete: true (retries: 0)");
    assert_eq!(
        reports[23].render(),
        "task 24 complete: [:container] {[#box]} (retries: 0)"
    );
    assert_eq!(
        reports[24].render(),
        "task 25 complete: [:item] {[#coin]} (retries: 0)"
    );
}

#[test]
fn runner_make_identity_is_idempotent_for_matching_name() {
    let mut runner = SourceRunner::new_empty();
    let first = runner.run_source("return make_identity(:root)").unwrap();
    let second = runner.run_source("return make_identity(:root)").unwrap();

    assert!(matches!(
        (first.outcome, second.outcome),
        (
            TaskOutcome::Complete { value: first, .. },
            TaskOutcome::Complete { value: second, .. }
        ) if first == second
    ));
}

#[test]
fn runner_mailbox_allocates_fresh_directional_caps() {
    let mut runner = SourceRunner::new_empty();
    let report = runner
        .run_source(
            "let first = mailbox()\n\
                 let second = mailbox()\n\
                 return first[0] != first[1] && first[0] != second[0] && first[1] != second[1]",
        )
        .unwrap();

    assert!(matches!(
        report.outcome,
        TaskOutcome::Complete { value, .. } if value == Value::bool(true)
    ));
}

#[test]
fn runner_mailbox_close_revokes_both_capabilities() {
    let mut runner = SourceRunner::new_empty();
    let result = runner.run_source(
        "let caps = mailbox()\n\
             mailbox_close(caps[0])\n\
             return mailbox_send(caps[1], \"late\")",
    );

    assert!(result.is_err());
}

#[test]
fn runner_mailbox_recv_expands_argument_splices() {
    let mut runner = SourceRunner::new_empty();
    let report = runner
        .run_source(
            "let caps = mailbox()\n\
                 let args = [[caps[0]], 0.5]\n\
                 return mailbox_recv(@args)",
        )
        .unwrap();

    let TaskOutcome::Suspended {
        kind: SuspendKind::MailboxRecv(request),
        ..
    } = report.outcome
    else {
        panic!("mailbox_recv(@args) did not suspend on mailbox receive");
    };

    assert_eq!(request.timeout_millis, Some(500));
    assert_eq!(request.receivers.len(), 1);
    runner
        .mailbox_for_receiver(&request.receivers[0])
        .expect("spliced receiver should be a valid receive cap");
}

#[test]
fn runner_relation_subscription_delivers_snapshot_then_ordered_settled_changes() {
    let mut runner = SourceRunner::new_empty();
    runner.run_source("make_relation(:Base, 1)").unwrap();
    runner.run_source("make_relation(:Derived, 1)").unwrap();
    runner
        .run_source("Derived(value) :-\n  Base(value)")
        .unwrap();
    runner.run_source("assert Base(1)").unwrap();

    let submitted = runner
        .submit_source(TaskRequest {
            principal: None,
            actor: None,
            endpoint: SYSTEM_ENDPOINT,
            authority: AuthorityContext::root(),
            input: TaskInput::Source(
                "let caps = mailbox()\n\
                 let subscription = subscribe_changes(caps[1], :relation, :Derived, [nothing], :snapshot)\n\
                 commit()\n\
                 assert Base(2)\n\
                 commit()\n\
                 return caps[0]"
                    .to_owned(),
            ),
        })
        .unwrap();
    assert!(matches!(
        submitted.outcome,
        TaskOutcome::Suspended {
            kind: SuspendKind::Commit,
            ..
        }
    ));
    let second = runner
        .resume_task(TaskRequest {
            principal: None,
            actor: None,
            endpoint: SYSTEM_ENDPOINT,
            authority: AuthorityContext::root(),
            input: TaskInput::Continuation {
                task_id: submitted.task_id,
                value: Value::nothing(),
            },
        })
        .unwrap();
    assert!(matches!(
        second,
        TaskOutcome::Suspended {
            kind: SuspendKind::Commit,
            ..
        }
    ));
    let completed = runner
        .resume_task(TaskRequest {
            principal: None,
            actor: None,
            endpoint: SYSTEM_ENDPOINT,
            authority: AuthorityContext::root(),
            input: TaskInput::Continuation {
                task_id: submitted.task_id,
                value: Value::nothing(),
            },
        })
        .unwrap();
    let TaskOutcome::Complete {
        value: receiver, ..
    } = completed
    else {
        panic!("subscription task did not complete");
    };

    let messages = runner.drain_mailbox(receiver).unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(
        subscription_message_symbol(&messages[0], "subject"),
        "snapshot"
    );
    assert_eq!(
        subscription_message_symbol(&messages[1], "subject"),
        "relation"
    );
    let first_cursor = subscription_message_int(&messages[0], "cursor");
    let second_cursor = subscription_message_int(&messages[1], "cursor");
    assert!(first_cursor < second_cursor);
    assert_eq!(
        subscription_message_rows(&messages[0], "assertions"),
        vec![vec![1]]
    );
    assert_eq!(
        subscription_message_rows(&messages[1], "assertions"),
        vec![vec![2]]
    );
    assert!(subscription_message_rows(&messages[1], "retractions").is_empty());
}

#[test]
fn runner_subscription_overflow_replaces_incremental_batches_with_resynchronization() {
    let mut runner = SourceRunner::new_empty();
    runner.run_source("make_relation(:Observed, 1)").unwrap();
    let submitted = runner
        .submit_source(TaskRequest {
            principal: None,
            actor: None,
            endpoint: SYSTEM_ENDPOINT,
            authority: AuthorityContext::root(),
            input: TaskInput::Source(
                "let caps = mailbox()\n\
                 let subscription = subscribe_changes(caps[1], :facts, :Observed, [nothing], :changes, nothing, 1)\n\
                 commit()\n\
                 assert Observed(1)\n\
                 commit()\n\
                 assert Observed(2)\n\
                 commit()\n\
                 return caps[0]"
                    .to_owned(),
            ),
        })
        .unwrap();
    let mut outcome = submitted.outcome;
    for _ in 0..3 {
        assert!(matches!(
            outcome,
            TaskOutcome::Suspended {
                kind: SuspendKind::Commit,
                ..
            }
        ));
        outcome = runner
            .resume_task(TaskRequest {
                principal: None,
                actor: None,
                endpoint: SYSTEM_ENDPOINT,
                authority: AuthorityContext::root(),
                input: TaskInput::Continuation {
                    task_id: submitted.task_id,
                    value: Value::nothing(),
                },
            })
            .unwrap();
    }
    let TaskOutcome::Complete {
        value: receiver, ..
    } = outcome
    else {
        panic!("overflow subscription task did not complete");
    };
    let messages = runner.drain_mailbox(receiver).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(
        subscription_message_symbol(&messages[0], "kind"),
        "resynchronize"
    );
}

#[test]
fn runner_subscription_refreshes_authority_before_filtering_values() {
    let mut runner = SourceRunner::new_empty();
    runner
        .run_source(
            "make_identity(:alice)\n\
             make_relation(:Observed, 1)\n\
             make_relation(:CanRead, 2)\n\
             assert CanRead(#alice, :Observed)",
        )
        .unwrap();
    let alice = runner.actor_identity(Symbol::intern("alice")).unwrap();
    let submitted = runner
        .submit_source_as(
            alice,
            SYSTEM_ENDPOINT,
            "let caps = mailbox()\n\
             let subscription = subscribe_changes(caps[1], :relation, :Observed, [nothing], :changes)\n\
             commit()\n\
             return caps[0]",
        )
        .unwrap();
    assert!(matches!(
        submitted.outcome,
        TaskOutcome::Suspended {
            kind: SuspendKind::Commit,
            ..
        }
    ));
    let completed = runner
        .resume_as(Symbol::intern("alice"), submitted.task_id)
        .unwrap();
    let TaskOutcome::Complete {
        value: receiver, ..
    } = completed.outcome
    else {
        panic!("authorized subscription task did not complete");
    };

    runner
        .run_source("retract CanRead(#alice, :Observed)")
        .unwrap();
    let messages = runner.drain_mailbox(receiver).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(subscription_message_symbol(&messages[0], "kind"), "revoked");
    assert!(
        messages[0]
            .map_get(&Value::symbol(Symbol::intern("assertions")))
            .is_none()
    );
}

#[test]
fn runner_subscription_cancellation_takes_effect_at_its_commit_boundary() {
    let mut runner = SourceRunner::new_empty();
    runner.run_source("make_relation(:Observed, 1)").unwrap();
    let submitted = runner
        .submit_source(TaskRequest {
            principal: None,
            actor: None,
            endpoint: SYSTEM_ENDPOINT,
            authority: AuthorityContext::root(),
            input: TaskInput::Source(
                "let caps = mailbox()\n\
                 let subscription = subscribe_changes(caps[1], :facts, :Observed, [nothing], :changes)\n\
                 commit()\n\
                 cancel_subscription(subscription)\n\
                 commit()\n\
                 assert Observed(1)\n\
                 commit()\n\
                 return caps[0]"
                    .to_owned(),
            ),
        })
        .unwrap();
    let mut outcome = submitted.outcome;
    for _ in 0..3 {
        outcome = runner
            .resume_task(TaskRequest {
                principal: None,
                actor: None,
                endpoint: SYSTEM_ENDPOINT,
                authority: AuthorityContext::root(),
                input: TaskInput::Continuation {
                    task_id: submitted.task_id,
                    value: Value::nothing(),
                },
            })
            .unwrap();
    }
    let TaskOutcome::Complete {
        value: receiver, ..
    } = outcome
    else {
        panic!("cancelled subscription task did not complete");
    };
    assert!(runner.drain_mailbox(receiver).unwrap().is_empty());
}

#[test]
fn runner_catalogue_subscription_receives_non_task_catalogue_commits() {
    let mut runner = SourceRunner::new_empty();
    let submitted = runner
        .submit_source(TaskRequest {
            principal: None,
            actor: None,
            endpoint: SYSTEM_ENDPOINT,
            authority: AuthorityContext::root(),
            input: TaskInput::Source(
                "let caps = mailbox()\n\
                 let subscription = subscribe_changes(caps[1], :catalogue, nothing, [], :changes)\n\
                 commit()\n\
                 return caps[0]"
                    .to_owned(),
            ),
        })
        .unwrap();
    let completed = runner
        .resume_task(TaskRequest {
            principal: None,
            actor: None,
            endpoint: SYSTEM_ENDPOINT,
            authority: AuthorityContext::root(),
            input: TaskInput::Continuation {
                task_id: submitted.task_id,
                value: Value::nothing(),
            },
        })
        .unwrap();
    let TaskOutcome::Complete {
        value: receiver, ..
    } = completed
    else {
        panic!("catalogue subscription task did not complete");
    };
    runner.run_source("make_relation(:Later, 1)").unwrap();
    let messages = runner.drain_mailbox(receiver).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(
        subscription_message_symbol(&messages[0], "subject"),
        "catalogue"
    );
}

#[test]
fn runner_relation_subscription_rebuilds_after_rule_catalogue_change() {
    let mut runner = SourceRunner::new_empty();
    runner.run_source("make_relation(:Base, 1)").unwrap();
    runner.run_source("make_relation(:Derived, 1)").unwrap();
    runner.run_source("assert Base(7)").unwrap();
    let submitted = runner
        .submit_source(TaskRequest {
            principal: None,
            actor: None,
            endpoint: SYSTEM_ENDPOINT,
            authority: AuthorityContext::root(),
            input: TaskInput::Source(
                "let caps = mailbox()\n\
                 let subscription = subscribe_changes(caps[1], :relation, :Derived, [nothing], :changes)\n\
                 commit()\n\
                 return caps[0]"
                    .to_owned(),
            ),
        })
        .unwrap();
    let completed = runner
        .resume_task(TaskRequest {
            principal: None,
            actor: None,
            endpoint: SYSTEM_ENDPOINT,
            authority: AuthorityContext::root(),
            input: TaskInput::Continuation {
                task_id: submitted.task_id,
                value: Value::nothing(),
            },
        })
        .unwrap();
    let TaskOutcome::Complete {
        value: receiver, ..
    } = completed
    else {
        panic!("relation subscription task did not complete");
    };

    runner
        .run_source("Derived(value) :-\n  Base(value)")
        .unwrap();
    let messages = runner.drain_mailbox(receiver).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(
        subscription_message_symbol(&messages[0], "subject"),
        "relation"
    );
    assert_eq!(
        subscription_message_rows(&messages[0], "assertions"),
        vec![vec![7]]
    );
}

#[test]
fn runner_discards_subscription_registration_when_task_aborts() {
    let mut runner = SourceRunner::new_empty();
    runner.run_source("make_relation(:Observed, 1)").unwrap();
    let report = runner
        .run_source(
            "let caps = mailbox()\n\
             let subscription = subscribe_changes(caps[1], :facts, :Observed, [nothing], :changes)\n\
             raise E_INVARG, \"abort registration\"",
        )
        .unwrap();
    assert!(matches!(report.outcome, TaskOutcome::Aborted { .. }));
    assert_eq!(runner.task_manager.subscription_count(), 0);
}

#[test]
fn runner_host_subscription_replays_retained_fact_changes_from_cursor() {
    let mut runner = SourceRunner::new_empty();
    runner.run_source("make_relation(:Observed, 1)").unwrap();
    let relation = runner
        .task_manager
        .kernel()
        .snapshot()
        .relation_metadata()
        .find(|metadata| metadata.name().name() == Some("Observed"))
        .unwrap()
        .id();
    let (first_receiver, first_sender) = runner.create_mailbox().unwrap();
    let first_subscription = runner
        .register_subscription(
            SubscriptionRequest {
                sender: first_sender,
                subject: SubscriptionSubject::Facts {
                    relation,
                    bindings: vec![None],
                },
                initial_delivery: SubscriptionInitialDelivery::ChangesOnly,
                cursor: None,
                queue_budget: 8,
            },
            super::RuntimeContext::default(),
            &AuthorityContext::root(),
        )
        .unwrap();
    runner.run_source("assert Observed(1)").unwrap();
    let first = runner.drain_mailbox(first_receiver).unwrap();
    let cursor = u64::try_from(subscription_message_int(&first[0], "cursor")).unwrap();
    runner.cancel_subscription(first_subscription).unwrap();

    runner.run_source("assert Observed(2)").unwrap();
    let (resumed_receiver, resumed_sender) = runner.create_mailbox().unwrap();
    runner
        .register_subscription(
            SubscriptionRequest {
                sender: resumed_sender,
                subject: SubscriptionSubject::Facts {
                    relation,
                    bindings: vec![None],
                },
                initial_delivery: SubscriptionInitialDelivery::ChangesOnly,
                cursor: Some(cursor),
                queue_budget: 8,
            },
            super::RuntimeContext::default(),
            &AuthorityContext::root(),
        )
        .unwrap();
    let replayed = runner.drain_mailbox(resumed_receiver).unwrap();
    assert_eq!(replayed.len(), 1);
    assert_eq!(
        subscription_message_rows(&replayed[0], "assertions"),
        vec![vec![2]]
    );
}

#[test]
fn runner_endpoint_close_cancels_owned_subscriptions() {
    let mut runner = SourceRunner::new_empty();
    runner.run_source("make_relation(:Observed, 1)").unwrap();
    let relation = runner
        .task_manager
        .kernel()
        .snapshot()
        .relation_metadata()
        .find(|metadata| metadata.name().name() == Some("Observed"))
        .unwrap()
        .id();
    let first_endpoint = Identity::new(0x00ee_0000_0000_0020).unwrap();
    let second_endpoint = Identity::new(0x00ee_0000_0000_0021).unwrap();
    runner
        .open_endpoint(first_endpoint, None, Symbol::intern("test"))
        .unwrap();
    runner
        .open_endpoint(second_endpoint, None, Symbol::intern("test"))
        .unwrap();
    for endpoint in [first_endpoint, second_endpoint] {
        let (_, sender) = runner.create_mailbox().unwrap();
        runner
            .register_subscription(
                SubscriptionRequest {
                    sender,
                    subject: SubscriptionSubject::Facts {
                        relation,
                        bindings: vec![None],
                    },
                    initial_delivery: SubscriptionInitialDelivery::ChangesOnly,
                    cursor: None,
                    queue_budget: 8,
                },
                super::RuntimeContext::new(None, None, endpoint),
                &AuthorityContext::root(),
            )
            .unwrap();
    }
    assert_eq!(runner.task_manager.subscription_count(), 2);

    runner.close_endpoint(first_endpoint);

    assert_eq!(runner.task_manager.subscription_count(), 1);
    assert_eq!(runner.cancel_all_subscriptions(), 1);
    assert_eq!(runner.task_manager.subscription_count(), 0);
}

fn subscription_message_value(message: &Value, key: &str) -> Value {
    message
        .map_get(&Value::symbol(Symbol::intern(key)))
        .unwrap_or_else(|| panic!("subscription message is missing :{key}"))
}

fn subscription_message_symbol(message: &Value, key: &str) -> &'static str {
    subscription_message_value(message, key)
        .as_symbol()
        .and_then(Symbol::name)
        .unwrap_or_else(|| panic!("subscription message :{key} is not a named symbol"))
}

fn subscription_message_int(message: &Value, key: &str) -> i64 {
    subscription_message_value(message, key)
        .as_int()
        .unwrap_or_else(|| panic!("subscription message :{key} is not an integer"))
}

fn subscription_message_rows(message: &Value, key: &str) -> Vec<Vec<i64>> {
    subscription_message_value(message, key)
        .with_list(|rows| {
            rows.iter()
                .map(|row| {
                    row.with_list(|values| {
                        values
                            .iter()
                            .map(|value| {
                                value.as_int().expect("test row value should be an integer")
                            })
                            .collect::<Vec<_>>()
                    })
                    .expect("subscription row should be a list")
                })
                .collect::<Vec<_>>()
        })
        .expect("subscription message rows should be a list")
}

#[test]
fn runner_mints_actor_authority_from_policy_facts() {
    let mut runner = SourceRunner::new_empty();
    runner
        .run_filein(include_str!("../../../apps/shared/capabilities.mica"))
        .unwrap();

    let alice = runner
        .run_source_as(
            Symbol::intern("alice"),
            ":polish(actor: #alice, item: #lamp)",
        )
        .unwrap();
    assert!(alice.render().contains("complete: \"polished brass lamp\""));
    assert!(
        alice
            .render()
            .contains("effect #alice: [\"polished\", #alice, #lamp]")
    );

    let bob_read = runner
        .run_source_as(Symbol::intern("bob"), "return #lamp.name")
        .unwrap();
    assert!(
        bob_read
            .render()
            .contains("complete: \"polished brass lamp\"")
    );

    let bob_write = runner
        .run_source_as(Symbol::intern("bob"), "#lamp.name = \"stolen\"")
        .unwrap_err();
    assert!(format!("{bob_write:?}").contains("PermissionDenied"));
    assert!(format!("{bob_write:?}").contains("operation: \"write\""));

    let bob_dispatch = runner
        .run_source_as(Symbol::intern("bob"), ":polish(actor: #bob, item: #lamp)")
        .unwrap_err();
    assert!(format!("{bob_dispatch:?}").contains("NoApplicableMethod"));

    let bob_catalog = runner
        .run_source_as(Symbol::intern("bob"), "make_relation(:Escape, 1)")
        .unwrap_err();
    assert!(format!("{bob_catalog:?}").contains("operation: \"grant\""));

    runner
        .run_source("retract HasRole(#alice, #builder)")
        .unwrap();
    runner
        .run_source("assert HasRole(#alice, #visitor)")
        .unwrap();

    let alice_read_after_role_change = runner
        .run_source_as(Symbol::intern("alice"), "return #lamp.name")
        .unwrap();
    assert!(
        alice_read_after_role_change
            .render()
            .contains("complete: \"polished brass lamp\"")
    );

    let alice_dispatch_after_role_change = runner
        .run_source_as(
            Symbol::intern("alice"),
            ":polish(actor: #alice, item: #lamp)",
        )
        .unwrap_err();
    assert!(format!("{alice_dispatch_after_role_change:?}").contains("NoApplicableMethod"));
}

#[test]
fn runner_keeps_direct_grant_facts_as_policy_fallback() {
    let mut runner = SourceRunner::new_empty();
    runner
        .run_filein(
            "make_identity(:bob)\n\
                 make_functional_relation(:Name, 2, [0])\n\
                 make_relation(:GrantRead, 2)\n\
                 assert Name(#bob, \"Bob\")\n\
                 assert GrantRead(#bob, :Name)\n",
        )
        .unwrap();

    let bob_read = runner
        .run_source_as(Symbol::intern("bob"), "return #bob.name")
        .unwrap();
    assert!(bob_read.render().contains("complete: \"Bob\""));

    let bob_write = runner
        .run_source_as(Symbol::intern("bob"), "#bob.name = \"Robert\"")
        .unwrap_err();
    assert!(format!("{bob_write:?}").contains("PermissionDenied"));
    assert!(format!("{bob_write:?}").contains("operation: \"write\""));
}

#[test]
fn runner_filein_ignores_comment_only_chunks() {
    let mut runner = SourceRunner::new_empty();
    let reports = runner
        .run_filein(
            "// one comment\n\
                 // another comment\n\
                 make_identity(:root)\n\
                 // trailing comment\n",
        )
        .unwrap();

    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].render(), "task 1 complete: #root (retries: 0)");
}

#[test]
fn runner_fileout_preserves_functional_relation_declarations() {
    let mut runner = SourceRunner::new_empty();
    runner
        .run_filein_with_unit(
            Symbol::intern("schema"),
            "make_functional_relation(:Name, 2, [0])",
            FileinMode::Add,
        )
        .unwrap();

    let source = runner.fileout_unit(Symbol::intern("schema")).unwrap();

    assert!(source.contains("make_functional_relation(:Name, 2, [0])"));
}

#[test]
fn runner_fileout_preserves_volatile_relation_declarations() {
    let mut runner = SourceRunner::new_empty();
    runner
        .run_filein_with_unit(
            Symbol::intern("schema"),
            "make_relation(:Scratch, 1, :volatile)\n\
             make_functional_relation(:Cache, 2, [0], :volatile)",
            FileinMode::Add,
        )
        .unwrap();

    let source = runner.fileout_unit(Symbol::intern("schema")).unwrap();

    assert!(source.contains("make_relation(:Scratch, 1, :volatile)"));
    assert!(source.contains("make_functional_relation(:Cache, 2, [0], :volatile)"));
}

#[test]
fn report_renders_task_outcome() {
    let mut runner = SourceRunner::new_empty();
    let report = runner.run_source("return true").unwrap();

    assert_eq!(report.render(), "task 1 complete: true (retries: 0)");
}

// ---------------------------------------------------------------------------
// F32 semantics cross-layer tests
// ---------------------------------------------------------------------------

#[test]
fn vm_numeric_equality_across_int_and_float() {
    let mut runner = SourceRunner::new_empty();
    assert!(matches!(
        runner.run_source("return 1 == 1.0").unwrap().outcome,
        TaskOutcome::Complete { value, .. } if value == Value::bool(true)
    ));
    assert!(matches!(
        runner.run_source("return 1.0 == 1").unwrap().outcome,
        TaskOutcome::Complete { value, .. } if value == Value::bool(true)
    ));
    assert!(matches!(
        runner.run_source("return 1 != 1.0").unwrap().outcome,
        TaskOutcome::Complete { value, .. } if value == Value::bool(false)
    ));
}

#[test]
fn vm_numeric_ordering_across_int_and_float() {
    let mut runner = SourceRunner::new_empty();
    assert!(matches!(
        runner.run_source("return 1 < 1.5").unwrap().outcome,
        TaskOutcome::Complete { value, .. } if value == Value::bool(true)
    ));
    assert!(matches!(
        runner.run_source("return 2 > 1.5").unwrap().outcome,
        TaskOutcome::Complete { value, .. } if value == Value::bool(true)
    ));
    assert!(matches!(
        runner.run_source("return 1.5 <= 1.5").unwrap().outcome,
        TaskOutcome::Complete { value, .. } if value == Value::bool(true)
    ));
    assert!(matches!(
        runner.run_source("return 3 >= 2.5").unwrap().outcome,
        TaskOutcome::Complete { value, .. } if value == Value::bool(true)
    ));
}

#[test]
fn vm_float_arithmetic_is_binary32() {
    let mut runner = SourceRunner::new_empty();
    assert!(matches!(
        runner.run_source("return 0.1 + 0.2").unwrap().outcome,
        TaskOutcome::Complete { value, .. }
            if value == Value::float(0.1f32 + 0.2f32).unwrap()
    ));
}

#[test]
fn vm_division_result_kind_rule() {
    let mut runner = SourceRunner::new_empty();
    assert!(matches!(
        runner.run_source("return 4 / 2").unwrap().outcome,
        TaskOutcome::Complete { value, .. } if value == Value::int(2).unwrap()
    ));
    assert!(matches!(
        runner.run_source("return 4 / 2.0").unwrap().outcome,
        TaskOutcome::Complete { value, .. }
            if value == Value::float(2.0).unwrap()
    ));
    assert!(matches!(
        runner.run_source("return 5 / 2").unwrap().outcome,
        TaskOutcome::Complete { value, .. }
            if value == Value::float(2.5).unwrap()
    ));
}

#[test]
fn vm_division_by_zero_returns_e_div() {
    let mut runner = SourceRunner::new_empty();
    assert!(matches!(
        runner.run_source("try\n  return 1 / 0\ncatch E_DIV\n  return 42\nend").unwrap().outcome,
        TaskOutcome::Complete { value, .. } if value == Value::int(42).unwrap()
    ));
    assert!(matches!(
        runner
            .run_source("try\n  return 1.0 / 0.0\ncatch E_DIV\n  return 42\nend")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. } if value == Value::int(42).unwrap()
    ));
}

#[test]
fn vm_arithmetic_overflow_returns_e_arith() {
    let mut runner = SourceRunner::new_empty();
    assert!(matches!(
        runner
            .run_source("try\n  return 3.4028235e38 * 3.4028235e38\ncatch E_ARITH\n  return 42\nend")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. } if value == Value::int(42).unwrap()
    ));
}

#[test]
fn vm_exponent_float_literal() {
    let mut runner = SourceRunner::new_empty();
    assert!(matches!(
        runner.run_source("return 1.5e2").unwrap().outcome,
        TaskOutcome::Complete { value, .. } if value == Value::float(150.0).unwrap()
    ));
    assert!(matches!(
        runner.run_source("return 1e0").unwrap().outcome,
        TaskOutcome::Complete { value, .. } if value == Value::float(1.0).unwrap()
    ));
}

#[test]
fn json_encode_decode_preserves_int_vs_float_kind() {
    let mut runner = SourceRunner::new_empty();
    // Int stays int.
    assert!(matches!(
        runner
            .run_source("return json_decode(json_encode(42))")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. } if value == Value::int(42).unwrap()
    ));
    // Float stays float.
    assert!(matches!(
        runner
            .run_source("return json_decode(json_encode(3.5))")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. } if value == Value::float(3.5).unwrap()
    ));
    // An integral float must retain its kind through JSON as well.
    assert!(matches!(
        runner
            .run_source("return json_decode(json_encode(1.0))")
            .unwrap()
            .outcome,
        TaskOutcome::Complete { value, .. } if value == Value::float(1.0).unwrap()
    ));
}

#[test]
fn json_decode_classifies_token_kinds() {
    let mut runner = SourceRunner::new_empty();
    // "1" is an integer.
    assert!(matches!(
        runner.run_source("return json_decode(\"1\")").unwrap().outcome,
        TaskOutcome::Complete { value, .. } if value == Value::int(1).unwrap()
    ));
    // "1.0" is a float.
    assert!(matches!(
        runner.run_source("return json_decode(\"1.0\")").unwrap().outcome,
        TaskOutcome::Complete { value, .. } if value == Value::float(1.0).unwrap()
    ));
    // "1e0" is a float.
    assert!(matches!(
        runner.run_source("return json_decode(\"1e0\")").unwrap().outcome,
        TaskOutcome::Complete { value, .. } if value == Value::float(1.0).unwrap()
    ));
}

#[test]
fn json_decode_rejects_oversized_integer() {
    let mut runner = SourceRunner::new_empty();
    // 2^55 is outside the Mica integer range (INT_MAX = 2^55 - 1).
    let error = runner
        .run_source("return json_decode(\"36028797018963968\")")
        .unwrap_err();
    assert!(format!("{error:?}").contains("outside the Mica integer range"));

    let error = runner
        .run_source("return json_decode(\"18446744073709551616\")")
        .unwrap_err();
    assert!(format!("{error:?}").contains("outside the Mica integer range"));
}

#[test]
fn float_source_literal_round_trips() {
    let mut runner = SourceRunner::new_empty();
    // Verify that to_literal produces a parseable literal that round-trips.
    let values = [
        Value::float(0.0).unwrap(),
        Value::float(1.5).unwrap(),
        Value::float(-3.25).unwrap(),
        Value::float(0.1).unwrap(),
        Value::float(1e10).unwrap(),
        Value::float(-1e-10).unwrap(),
        Value::float(f32::MAX).unwrap(),
        Value::float(f32::MIN_POSITIVE).unwrap(),
        Value::float(f32::from_bits(1)).unwrap(),
    ];
    for value in values {
        let lit = crate::float_to_literal(value.as_float().unwrap());
        let source = format!("return {lit}");
        let report = runner.run_source(&source).unwrap();
        match report.outcome {
            TaskOutcome::Complete { value: result, .. } => {
                assert_eq!(
                    result.as_float().map(f32::to_bits),
                    value.as_float().map(f32::to_bits),
                    "float literal round-trip failed for {:?}: got {:?}",
                    value,
                    result
                );
            }
            other => panic!("expected complete outcome for {value:?}, got {other:?}"),
        }
    }
}
