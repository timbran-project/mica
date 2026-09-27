// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::{Cli, open_runner, reject_actor};
use clap::Args;
use flate2::read::MultiGzDecoder;
use mica_runtime::{SourceRunner, TaskInput, TaskLimits, TaskOutcome};
use mica_var::{Symbol, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufReader, Read, Seek};
use std::path::PathBuf;
use std::time::Instant;
use xml::attribute::OwnedAttribute;
use xml::name::OwnedName;
use xml::reader::{EventReader, ParserConfig, XmlEvent};

const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
const OWL: &str = "http://www.w3.org/2002/07/owl#";
const CYC: &str = "http://sw.cyc.com/CycAnnotations_v1#";
const MAX_SUBJECT_BYTES: usize = 8 * 1024 * 1024;
const MAX_SUBJECT_FACTS: usize = 65_536;
const SOURCES: &[(&str, &str)] = &[
    (
        "schema",
        include_str!("../../../apps/bycycle-owl/00_schema.mica"),
    ),
    (
        "retrieval",
        include_str!("../../../apps/shared/retrieval.mica"),
    ),
    (
        "taxonomy",
        include_str!("../../../apps/bycycle-owl/10_taxonomy.mica"),
    ),
    (
        "constraints",
        include_str!("../../../apps/bycycle-owl/20_constraints.mica"),
    ),
    (
        "graph",
        include_str!("../../../apps/bycycle-owl/30_graph.mica"),
    ),
    (
        "loader",
        include_str!("../../../apps/bycycle-owl/40_loader.mica"),
    ),
];

#[derive(Args)]
pub(crate) struct Options {
    /// Plain XML or gzip-compressed OWL input.
    #[arg(long)]
    owl: PathBuf,
    /// Count subjects and predicates without opening a store.
    #[arg(long)]
    census: bool,
    /// Install the bundled ontology and loader before importing.
    #[arg(long, conflicts_with = "census")]
    init: bool,
    /// Maximum additional subjects this run; zero means all remaining subjects.
    #[arg(long, default_value_t = 0)]
    limit: usize,
    /// Flush after this many input facts, at a subject boundary.
    #[arg(long, default_value_t = 20_000, value_parser = clap::value_parser!(u32).range(1..))]
    commit_batch: u32,
    /// Actor name granted retrieval access to each imported subject.
    #[arg(long, default_value = "")]
    retrieval_actor: String,
}

#[derive(Default)]
struct Stats {
    subjects: usize,
    skipped: usize,
    dropped_resources: usize,
    dropped_repeats: usize,
    asserted: usize,
    commits: usize,
    subject_tags: BTreeMap<String, usize>,
    predicate_tags: BTreeMap<String, usize>,
}

struct Subject {
    guid: Option<String>,
    triples: Vec<Value>,
    bytes: usize,
}

struct Predicate {
    depth: usize,
    field: &'static str,
    resource: Option<String>,
    text: String,
}

fn attribute<'a>(attributes: &'a [OwnedAttribute], local: &str) -> Option<&'a str> {
    attributes
        .iter()
        .find(|a| a.name.namespace.as_deref() == Some(RDF) && a.name.local_name == local)
        .map(|a| a.value.as_str())
}

fn guid(uri: &str) -> Option<&str> {
    let fragment = uri.rsplit(['/', '#']).next()?;
    (fragment.len() >= 20
        && fragment
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b)))
    .then_some(fragment)
}

fn field(name: &OwnedName) -> Option<&'static str> {
    match (name.namespace.as_deref(), name.local_name.as_str()) {
        (Some(RDF), "type") => Some("Isa"),
        (Some(RDFS), "subClassOf") => Some("Genls"),
        (Some(OWL), "disjointWith") => Some("DisjointWith"),
        (Some(OWL), "sameAs") => Some("SameAs"),
        (Some(RDFS), "label") => Some("Label"),
        (Some(CYC), "label") => Some("CycLabel"),
        (Some(RDFS), "comment") => Some("Comment"),
        (_, "Mx4rwLSVCpwpEbGdrcN5Y29ycA") => Some("Alias"),
        (_, "Mx4rBVVEokNxEdaAAACgydogAg") => Some("QuotedIsa"),
        (_, "Mx4rvhOImJwpEbGdrcN5Y29ycA") => Some("TypeGenls"),
        (_, "Mx4rvdUGBpwpEbGdrcN5Y29ycA") => Some("Arg1Pred"),
        (_, "Mx4rwTvAxJwpEbGdrcN5Y29ycA") => Some("RewriteOf"),
        (_, "Mx4rZOAVeiYGEdqAAAACs2IMmw") => Some("BroaderTerm"),
        (_, "Mx4riWVFR6HJSpaEaHrcWS3MSA") => Some("SeeAlso"),
        (_, "Mx4rTv-jk9SPTXa991kk5mAvHg") => Some("WikiName"),
        (_, "Mx4rNv0nbm4TTjOp7yhmnzOyqg") => Some("WikiURL"),
        _ => None,
    }
}

fn next_event<R: Read>(parser: &mut EventReader<R>) -> Result<XmlEvent, String> {
    parser.next().map_err(|error| format!("OWL XML: {error}"))
}

fn triple(predicate: Predicate, stats: &mut Stats) -> Option<Value> {
    let (text, resource) = if let Some(uri) = predicate.resource {
        if let Some(fragment) = guid(&uri) {
            (fragment.to_owned(), true)
        } else if predicate.field == "SameAs" && !uri.is_empty() {
            (uri, false)
        } else {
            stats.dropped_resources += 1;
            return None;
        }
    } else {
        let text = predicate.text.trim();
        if text.is_empty() {
            stats.skipped += 1;
            return None;
        }
        (text.to_owned(), false)
    };
    Some(Value::list([
        Value::symbol(Symbol::intern(predicate.field)),
        Value::string(text),
        Value::bool(resource),
    ]))
}

fn next_subject<R: Read>(
    parser: &mut EventReader<R>,
    stats: &mut Stats,
    census: bool,
) -> Result<Option<Subject>, String> {
    let (name, attributes) = loop {
        match next_event(parser)? {
            XmlEvent::StartElement {
                name, attributes, ..
            } if attribute(&attributes, "about").is_some() => break (name, attributes),
            XmlEvent::EndDocument => return Ok(None),
            _ => {}
        }
    };
    if census {
        *stats.subject_tags.entry(name.to_string()).or_default() += 1;
    }
    let mut subject = Subject {
        guid: attribute(&attributes, "about")
            .and_then(guid)
            .map(str::to_owned),
        triples: Vec::new(),
        bytes: attributes.iter().map(|a| a.value.len()).sum(),
    };
    let mut depth = 1;
    let mut pending: Option<Predicate> = None;
    loop {
        match next_event(parser)? {
            XmlEvent::StartElement {
                name, attributes, ..
            } => {
                depth += 1;
                subject.bytes += attributes.iter().map(|a| a.value.len()).sum::<usize>();
                if census {
                    *stats.predicate_tags.entry(name.to_string()).or_default() += 1;
                }
                if pending.is_none()
                    && let Some(field) = field(&name)
                {
                    pending = Some(Predicate {
                        depth,
                        field,
                        resource: attribute(&attributes, "resource").map(str::to_owned),
                        text: String::new(),
                    });
                }
            }
            XmlEvent::Characters(text) | XmlEvent::CData(text) | XmlEvent::Whitespace(text) => {
                subject.bytes += text.len();
                if let Some(predicate) = pending.as_mut() {
                    predicate.text.push_str(&text);
                }
            }
            XmlEvent::EndElement { .. } => {
                if pending
                    .as_ref()
                    .is_some_and(|predicate| predicate.depth == depth)
                    && let Some(value) = triple(pending.take().unwrap(), stats)
                {
                    subject.triples.push(value);
                }
                depth -= 1;
                if depth == 0 {
                    return Ok(Some(subject));
                }
            }
            XmlEvent::EndDocument => return Err("OWL XML ended inside a subject".to_owned()),
            _ => {}
        }
        if subject.bytes > MAX_SUBJECT_BYTES
            || subject.triples.len() > MAX_SUBJECT_FACTS
            || depth > 256
        {
            return Err("OWL subject exceeds 8 MiB, 65536 facts, or 256 nesting levels".to_owned());
        }
    }
}

fn invoke(
    runner: &mut SourceRunner,
    selector: &str,
    roles: Vec<(Symbol, Value)>,
) -> Result<Value, String> {
    let mut request = SourceRunner::root_source_request("");
    request.input = TaskInput::Invocation {
        selector: Symbol::intern(selector),
        roles,
    };
    let submitted = runner
        .submit_invocation(request)
        .map_err(|e| runner.render_source_task_error(&e))?;
    runner.forget_terminal_task(submitted.task_id);
    match submitted.outcome {
        TaskOutcome::Complete { value, .. } => Ok(value),
        outcome => Err(format!("{selector}: {outcome:?}")),
    }
}

fn initialize(runner: &mut SourceRunner) -> Result<(), String> {
    for (name, source) in SOURCES {
        for report in runner
            .run_filein(source)
            .map_err(|e| runner.render_source_task_error_with_source(&e, Some(name), source))?
        {
            if !matches!(report.outcome, TaskOutcome::Complete { .. }) {
                return Err(format!("{name}: {}", report.render()));
            }
        }
    }
    runner.forget_all_terminal_tasks();
    Ok(())
}

fn progress(digest: &str, actor: &str, subjects: usize, complete: bool) -> Result<Value, String> {
    let count = i64::try_from(subjects)
        .ok()
        .and_then(|count| Value::int(count).ok())
        .ok_or("OWL subject count exceeds Mica integer range")?;
    Ok(Value::list([
        Value::string(digest),
        Value::string(actor),
        count,
        Value::bool(complete),
    ]))
}

fn resume(state: &Value, digest: &str, actor: &str) -> Result<(usize, bool), String> {
    if state.list_len() == Some(0) {
        return Ok((0, false));
    }
    if state.list_len() != Some(4)
        || state.list_get(0) != Some(Value::string(digest))
        || state.list_get(1) != Some(Value::string(actor))
    {
        return Err(
            "OWL resume state does not match the input SHA-256 and retrieval actor".to_owned(),
        );
    }
    let count = state
        .list_get(2)
        .and_then(|v| v.as_int())
        .and_then(|v| usize::try_from(v).ok())
        .ok_or("invalid OWL resume subject count")?;
    let complete = state
        .list_get(3)
        .and_then(|v| v.as_bool())
        .ok_or("invalid OWL resume completion flag")?;
    Ok((count, complete))
}

fn flush(
    runner: &mut SourceRunner,
    batch: &mut Vec<Value>,
    expected: &mut Value,
    next: Value,
    actor: &str,
    stats: &mut Stats,
) -> Result<(), String> {
    let result = invoke(
        runner,
        "bycycle_load_batch",
        vec![
            (Symbol::intern("batch"), Value::list(std::mem::take(batch))),
            (Symbol::intern("expected"), expected.clone()),
            (Symbol::intern("progress"), next.clone()),
            (Symbol::intern("retrieval_actor"), Value::string(actor)),
        ],
    )?;
    let count = |index| {
        result
            .list_get(index)
            .and_then(|v| v.as_int())
            .and_then(|v| usize::try_from(v).ok())
            .ok_or("invalid OWL loader result")
    };
    stats.asserted += count(0)?;
    stats.dropped_repeats += count(1)?;
    stats.commits += 1;
    *expected = next;
    Ok(())
}

fn import<R: Read>(
    reader: R,
    runner: &mut SourceRunner,
    digest: &str,
    options: &Options,
    stats: &mut Stats,
) -> Result<(usize, bool), String> {
    let mut expected = invoke(runner, "bycycle_progress", Vec::new())?;
    let (skip, complete) = resume(&expected, digest, &options.retrieval_actor)?;
    if complete {
        return Ok((skip, true));
    }
    let mut parser = parser(reader);
    let mut seen = 0;
    let mut batch = Vec::new();
    let mut facts = 0;
    let mut bytes = 0;
    let mut complete = false;
    loop {
        if options.limit > 0 && stats.subjects >= options.limit {
            break;
        }
        // Replayed subjects rebuild XML namespace context but do not count as work for this run.
        let mut scanned = Stats::default();
        let Some(subject) = next_subject(&mut parser, &mut scanned, false)? else {
            complete = true;
            break;
        };
        let Some(guid) = subject.guid else {
            stats.skipped += 1;
            continue;
        };
        seen += 1;
        if seen <= skip {
            continue;
        }
        stats.subjects += 1;
        stats.skipped += scanned.skipped;
        stats.dropped_resources += scanned.dropped_resources;
        facts += subject.triples.len() + 1;
        bytes += subject.bytes;
        batch.push(Value::list([
            Value::string(guid),
            Value::list(subject.triples),
        ]));
        if facts >= options.commit_batch as usize || bytes >= MAX_SUBJECT_BYTES {
            flush(
                runner,
                &mut batch,
                &mut expected,
                progress(digest, &options.retrieval_actor, seen, false)?,
                &options.retrieval_actor,
                stats,
            )?;
            facts = 0;
            bytes = 0;
        }
    }
    if seen < skip {
        return Err("OWL input ended before its resume subject count".to_owned());
    }
    if !batch.is_empty() || complete {
        flush(
            runner,
            &mut batch,
            &mut expected,
            progress(digest, &options.retrieval_actor, seen, complete)?,
            &options.retrieval_actor,
            stats,
        )?;
    }
    Ok((seen, complete))
}

fn parser<R: Read>(reader: R) -> EventReader<R> {
    ParserConfig::new()
        .max_data_length(MAX_SUBJECT_BYTES)
        .max_attribute_length(MAX_SUBJECT_BYTES)
        .create_reader(reader)
}

pub(crate) fn run(cli: &Cli, options: &Options) -> Result<(), String> {
    reject_actor(cli)?;
    let start = Instant::now();
    let mut file =
        File::open(&options.owl).map_err(|e| format!("{}: {e}", options.owl.display()))?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    let digest = format!("{:x}", hash.finalize());
    file.rewind().map_err(|e| e.to_string())?;
    let reader: Box<dyn Read> = if options.owl.extension().is_some_and(|ext| ext == "gz") {
        Box::new(BufReader::new(MultiGzDecoder::new(BufReader::new(file))))
    } else {
        Box::new(BufReader::new(file))
    };
    let mut stats = Stats::default();
    if options.census {
        let mut parser = parser(reader);
        while next_subject(&mut parser, &mut stats, true)?.is_some() {
            stats.subjects += 1;
        }
        println!(
            "{}",
            serde_json::json!({"subjects": stats.subjects, "subject_tags": stats.subject_tags, "predicate_tags": stats.predicate_tags, "seconds": start.elapsed().as_secs_f64(), "sha256": digest})
        );
        return Ok(());
    }
    if cli.store.is_none() {
        return Err("OWL loading requires --store DIR (or --census)".to_owned());
    }
    let mut runner = open_runner(cli)?.with_task_limits(TaskLimits {
        instruction_budget: 100_000_000,
        ..TaskLimits::default()
    });
    if options.init {
        initialize(&mut runner)?;
    }
    let (total, complete) = import(reader, &mut runner, &digest, options, &mut stats)?;
    runner
        .flush_persistence()
        .map_err(|e| runner.render_source_task_error(&e))?;
    println!(
        "{}",
        serde_json::json!({"subjects": stats.subjects, "total_subjects": total, "complete": complete, "asserted": stats.asserted, "skipped": stats.skipped, "dropped_resources": stats.dropped_resources, "dropped_repeats": stats.dropped_repeats, "commits": stats.commits, "seconds": start.elapsed().as_secs_f64(), "sha256": digest, "durability": format!("{:?}", cli.durability).to_lowercase()})
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use mica_relation_kernel::FjallDurabilityMode;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    const FIXTURE: &str = include_str!("../../../apps/bycycle-owl/testdata/fixture.owl");

    fn options(limit: usize, commit_batch: u32) -> Options {
        Options {
            owl: PathBuf::new(),
            census: false,
            init: false,
            limit,
            commit_batch,
            retrieval_actor: "reader".to_owned(),
        }
    }

    fn check(runner: &mut SourceRunner, source: &str) {
        let report = runner.run_source(source).unwrap();
        assert!(
            matches!(report.outcome, TaskOutcome::Complete { .. }),
            "{}",
            report.render()
        );
    }

    #[test]
    fn owl_fixture_preserves_literals_resources_and_inference() {
        for interpreter_only in [true, false] {
            let mut runner = SourceRunner::new_empty().with_interpreter_only(interpreter_only);
            initialize(&mut runner).unwrap();
            let mut stats = Stats::default();
            assert_eq!(
                import(
                    FIXTURE.as_bytes(),
                    &mut runner,
                    "fixture",
                    &options(0, 4),
                    &mut stats
                )
                .unwrap(),
                (4, true)
            );
            assert_eq!(stats.subjects, 4);
            assert_eq!(stats.dropped_resources, 1);
            assert_eq!(stats.dropped_repeats, 2);
            assert_eq!(stats.asserted, 13);
            check(
                &mut runner,
                r#"
                require Label(#bycycle/guid/Mx4rTestAnimalGuid000001, "Animal")
                require Alias(#bycycle/guid/Mx4rTestAnimalGuid000001, "Beast")
                require Comment(#bycycle/guid/Mx4rTestAnimalGuid000001, "A <b>living</b> thing &amp; more")
                require Label(#bycycle/guid/Mx4rTestDogGuid000000002, "Dog & kin")
                require Comment(#bycycle/guid/Mx4rTestDogGuid000000002, "Canines")
                require WikiName(#bycycle/guid/Mx4rTestAnimalGuid000001, "Animal")
                require !WikiName(#bycycle/guid/Mx4rTestAnimalGuid000001, "Animalia")
                require InconsistentWith(#bycycle/guid/Mx4rTestRoboDogGuid00004, #bycycle/guid/Mx4rTestAnimalGuid000001, #bycycle/guid/Mx4rTestArtifactGuid0003)
                require CanRetrieveSubject(#reader, #bycycle/guid/Mx4rTestRoboDogGuid00004)
                require len(GuidOf(?identity, ?guid)) == 4
                return true
            "#,
            );
        }
    }

    struct Store(PathBuf);

    impl Store {
        fn new() -> Self {
            static SEQUENCE: AtomicU64 = AtomicU64::new(0);
            Self(std::env::temp_dir().join(format!(
                    "mica-owl-{}-{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos(),
                    SEQUENCE.fetch_add(1, Ordering::Relaxed)
                )))
        }
    }

    impl Drop for Store {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn owl_resume_reopens_store_and_rejects_changed_input_or_actor() {
        let store = Store::new();
        let open = || SourceRunner::open_fjall(&store.0, FjallDurabilityMode::Strict).unwrap();
        let mut runner = open();
        initialize(&mut runner).unwrap();
        assert_eq!(
            import(
                FIXTURE.as_bytes(),
                &mut runner,
                "fixture",
                &options(2, 100),
                &mut Stats::default()
            )
            .unwrap(),
            (2, false)
        );
        runner.flush_persistence().unwrap();
        drop(runner);
        let mut runner = open();
        assert!(
            import(
                FIXTURE.as_bytes(),
                &mut runner,
                "changed",
                &options(0, 100),
                &mut Stats::default()
            )
            .unwrap_err()
            .contains("SHA-256")
        );
        let mut changed_actor = options(0, 100);
        changed_actor.retrieval_actor = "other".to_owned();
        assert!(
            import(
                FIXTURE.as_bytes(),
                &mut runner,
                "fixture",
                &changed_actor,
                &mut Stats::default()
            )
            .is_err()
        );
        let mut stats = Stats::default();
        assert_eq!(
            import(
                FIXTURE.as_bytes(),
                &mut runner,
                "fixture",
                &options(0, 100),
                &mut stats
            )
            .unwrap(),
            (4, true)
        );
        assert_eq!(stats.subjects, 2);
        check(
            &mut runner,
            "require len(GuidOf(?identity, ?guid)) == 4\nrequire len(Alias(?identity, ?text)) == 1\nreturn true",
        );
        drop(runner);
        let mut runner = open();
        let mut stats = Stats::default();
        assert_eq!(
            import(
                FIXTURE.as_bytes(),
                &mut runner,
                "fixture",
                &options(0, 100),
                &mut stats
            )
            .unwrap(),
            (4, true)
        );
        assert_eq!(stats.commits, 0);
        check(
            &mut runner,
            "require bycycle_progress() == [\"fixture\", \"reader\", 4, true]\nreturn true",
        );
    }

    #[test]
    fn owl_invalid_xml_discards_pending_batch_and_gzip_preserves_content() {
        let mut runner = SourceRunner::new_empty();
        initialize(&mut runner).unwrap();
        let truncated = FIXTURE.split("<owl:NamedIndividual").next().unwrap();
        assert!(
            import(
                truncated.as_bytes(),
                &mut runner,
                "fixture",
                &options(0, 100),
                &mut Stats::default()
            )
            .unwrap_err()
            .contains("XML")
        );
        check(
            &mut runner,
            "require bycycle_progress() == []\nrequire len(GuidOf(?identity, ?guid)) == 0\nreturn true",
        );
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(FIXTURE.as_bytes()).unwrap();
        let compressed = encoder.finish().unwrap();
        assert_eq!(
            import(
                MultiGzDecoder::new(compressed.as_slice()),
                &mut runner,
                "gzip",
                &options(0, 100),
                &mut Stats::default()
            )
            .unwrap(),
            (4, true)
        );
    }

    #[test]
    fn owl_prefixes_do_not_change_predicates_and_guid_names_remain_distinct() {
        let source = r##"<r:RDF xmlns:r="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:s="http://www.w3.org/2000/01/rdf-schema#" xmlns:o="http://www.w3.org/2002/07/owl#">
            <o:Class r:about="#abcdefghijklmnopqrs-t"><s:label>é🦀</s:label><o:sameAs r:resource="https://example.org/item"/></o:Class>
            <o:Class r:about="#abcdefghijklmnopqrs_t"><s:subClassOf r:resource="#abcdefghijklmnopqrs-t"/></o:Class>
        </r:RDF>"##;
        let mut runner = SourceRunner::new_empty();
        initialize(&mut runner).unwrap();
        import(
            source.as_bytes(),
            &mut runner,
            "names",
            &options(0, 1),
            &mut Stats::default(),
        )
        .unwrap();
        check(
            &mut runner,
            r#"
            require len(GuidOf(?identity, ?guid)) == 2
            require len(Label(?identity, "é🦀")) == 1
            require len(SameAs(?identity, "https://example.org/item")) == 1
            require len(Subsumes(?parent, ?child)) == 1
            return true
        "#,
        );
    }
}
