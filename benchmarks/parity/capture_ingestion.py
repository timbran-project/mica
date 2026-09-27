#!/usr/bin/env python3
"""Measure pinned ingestion commands with fixed input and fresh strict stores."""

import argparse
import datetime
import json
import os
from pathlib import Path
import re
import shutil
import statistics
import subprocess
import time

from capture import ROOT, archive, digest, execute, positive, revision, run, save


def owl_fixture(count):
    root = "Mx4rParityRoot0000000000"
    rows = ['<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" '
            'xmlns:rdfs="http://www.w3.org/2000/01/rdf-schema#" '
            'xmlns:owl="http://www.w3.org/2002/07/owl#">',
            f'<owl:Class rdf:about="#{root}"><rdfs:label>Root é🦀</rdfs:label></owl:Class>']
    for index in range(count):
        rows.append(f'<owl:Class rdf:about="#Mx4rParityItem{index:012d}">'
                    f'<rdfs:label>Item {index} é🦀</rdfs:label>'
                    f'<rdfs:subClassOf rdf:resource="#{root}"/></owl:Class>')
    rows.append('</rdf:RDF>')
    return '\n'.join(rows) + '\n'


def cycl_fixture(count):
    return ''.join(
        f'(#$BaseKB (#$isa #$Item{i} #$Thing) :TRUE :FORWARD :MONOTONIC)\n'
        if i % 2 == 0 else
        f'(#$PeopleMt (#$comment (#$NartFn #$Item{i}) "Text é🦀") :TRUE :FORWARD :DEFAULT)\n'
        for i in range(count)
    )


def validate_stdout(case, implementation, stdout, subjects, assertions):
    if implementation == 'rust':
        report = json.loads(stdout)
        expected = ({'subjects': subjects + 1, 'total_subjects': subjects + 1,
                     'asserted': 2 * subjects + 1, 'complete': True,
                     'dropped_resources': 0, 'dropped_repeats': 0, 'durability': 'strict'}
                    if case == 'owl' else
                    {'assertions': assertions, 'malformed_lines': 0, 'asserted': 0,
                     'predicates': {'#$isa': (assertions + 1) // 2, '#$comment': assertions // 2}})
        for key, value in expected.items():
            if report.get(key) != value:
                raise ValueError(f'{key}: expected {value!r}, got {report.get(key)!r}')
        return
    if case == 'owl':
        match = re.search(r'done: (\d+) subjects, (\d+) triples,', stdout)
        if not match or tuple(map(int, match.groups())) != (subjects + 1, 2 * subjects + 1):
            raise ValueError('Odin OWL counts differ from the generated input')
        return
    if f'{assertions} assertions parsed, 0 lines skipped; 2 predicates:' not in stdout:
        raise ValueError('Odin CycL census count mismatch')
    counts = dict((name, int(count)) for count, name in re.findall(r'^(\d+)\t(.+)$', stdout, re.M))
    if counts != {'#$isa': (assertions + 1) // 2, '#$comment': assertions // 2}:
        raise ValueError(f'Odin CycL predicate mismatch: {counts}')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path)
    parser.add_argument('--rust-revision', default='HEAD')
    parser.add_argument('--odin-repo', type=Path, default=ROOT.parent / 'omica')
    parser.add_argument('--odin-revision', default='bfb368c')
    parser.add_argument('--cargo-target-dir', type=Path)
    parser.add_argument('--cpu', type=int, default=5)
    parser.add_argument('--runs', type=positive, default=3)
    parser.add_argument('--subjects', type=positive, default=1000)
    parser.add_argument('--assertions', type=positive, default=100000)
    parser.add_argument('--timeout', type=positive, default=120)
    args = parser.parse_args()
    if args.cpu not in os.sched_getaffinity(0):
        parser.error('CPU is outside the current affinity mask')
    if args.assertions < 2:
        parser.error('--assertions must be at least two')
    output = args.output.resolve()
    output.mkdir(parents=True)
    rust_rev, odin_rev = revision(ROOT, args.rust_revision), revision(args.odin_repo, args.odin_revision)
    rust_source, odin_source = output / 'rust-source', output / 'odin-source'
    archive(ROOT, rust_rev, rust_source)
    archive(args.odin_repo, odin_rev, odin_source)
    owl, cycl = output / 'input.owl', output / 'input.cycl'
    owl.write_text(owl_fixture(args.subjects))
    cycl.write_text(cycl_fixture(args.assertions))
    odin = shutil.which('odin')
    if not odin:
        raise RuntimeError('Odin compiler is not available')
    target = args.cargo_target_dir.resolve() if args.cargo_target_dir else rust_source / 'target'
    builds = [(['cargo', 'build', '--locked', '--release', '-p', 'mica-runner', '--target-dir', str(target)], rust_source, 'rust')]
    builds += [([odin, 'build', f'tools/{tool}', '-o:speed', f'-out:{output / tool}'], odin_source, tool)
               for tool in ['owlstream', 'cycl-load', 'filein']]
    manifest = {
        'format': 1, 'created_utc': datetime.datetime.now(datetime.timezone.utc).isoformat(),
        'rust_revision': rust_rev, 'odin_revision': odin_rev,
        'rust_compiler': run(['rustc', '-vV']), 'odin_compiler': run([odin, 'version']),
        'machine': list(os.uname()), 'cpu': args.cpu, 'workers': 1,
        'accelerator': 'none', 'authority': 'root', 'runs': args.runs,
        'inputs': {'owl': {'sha256': digest(owl), 'subjects': args.subjects + 1, 'facts': 2 * args.subjects + 1},
                   'cycl': {'sha256': digest(cycl), 'assertions': args.assertions}},
        'protocol': {
            'timing': 'whole child process, from launch through exit; zero warmup',
            'memory': 'GNU time peak process RSS, including open, parsing, execution, commit, and close',
            'owl': 'fresh initialized strict store; no inference rules; one input batch; result verification after timing',
            'owl_tier': {'rust': 'native-enabled Mica loader', 'odin': 'native host loader'},
            'owl_durability': 'strict', 'owl_storage': {'rust': 'Fjall', 'odin': 'Odin store'},
            'cycl': 'no fact writes; Rust parser only; Odin includes in-memory schema initialization',
            'cycl_tier': 'native host parsers',
            'batch': 'Rust publishes cursor with facts; Odin publishes cursor in a separate transaction',
            'timeout_seconds': args.timeout,
        },
        'harness_sha256': {p.name: digest(p) for p in [Path(__file__), Path(__file__).with_name('capture.py')]},
        'builds': [{'command': command, 'cwd': str(cwd)} for command, cwd, _ in builds],
        'results': [],
    }
    save(output / 'manifest.json', manifest)
    for command, cwd, label in builds:
        print(f'building {label}', flush=True)
        with (output / f'{label}-build.log').open('w') as log:
            subprocess.run(command, cwd=cwd, stdout=log, stderr=subprocess.STDOUT, check=True)
    rust = output / 'mica'
    shutil.copy2(target / 'release/mica', rust)
    manifest['binaries'] = {name: digest(output / name) for name in ['mica', 'owlstream', 'cycl-load', 'filein']}
    prefix = ['taskset', '-c', str(args.cpu)]
    for repetition in range(args.runs):
        implementations = ['rust', 'odin'] if repetition % 2 == 0 else ['odin', 'rust']
        for case in ['owl', 'cycl']:
            for implementation in implementations:
                name = f'{case}-{implementation}-{repetition}'
                store = output / f'{name}.store'
                source = rust_source if implementation == 'rust' else odin_source
                result = {'case': case, 'implementation': implementation, 'run': repetition, 'passed': False}
                try:
                    if case == 'owl':
                        setup = ([str(rust), '--store', str(store), '--durability', 'strict', 'filein',
                                  'apps/bycycle-owl/00_schema.mica', 'apps/bycycle-owl/40_loader.mica']
                                 if implementation == 'rust' else
                                 [str(output / 'filein'), '--store', str(store), '--durability', 'strict', 'apps/bycycle-owl/00_schema.mica'])
                        prepared = execute(prefix + setup, source, args.timeout)
                        (output / f'{name}-setup.stdout').write_text(prepared.stdout)
                        (output / f'{name}-setup.stderr').write_text(prepared.stderr)
                        if prepared.returncode:
                            raise ValueError(f'setup returned {prepared.returncode}')
                        result['setup_command'] = setup
                        command = ([str(rust), '--store', str(store), '--durability', 'strict', 'owl', '--owl', str(owl), '--commit-batch', '1000000000']
                                   if implementation == 'rust' else
                                   [str(output / 'owlstream'), '--store', str(store), '--durability', 'strict', '--owl', str(owl), '--commit-batch', '1000000000'])
                    else:
                        command = ([str(rust), 'cycl-census', str(cycl)] if implementation == 'rust' else
                                   [str(output / 'cycl-load'), 'apps/bycycle/00_schema.mica', str(cycl)])
                    rss = output / f'{name}.rss'
                    command = ['/usr/bin/time', '-f', '%M', '-o', str(rss), *prefix, *command]
                    result['command'] = command
                    start = time.perf_counter_ns()
                    process = execute(command, source, args.timeout)
                    result['elapsed_ns'] = time.perf_counter_ns() - start
                    (output / f'{name}.stdout').write_text(process.stdout)
                    (output / f'{name}.stderr').write_text(process.stderr)
                    result['returncode'] = process.returncode
                    if process.returncode:
                        raise ValueError(f'command returned {process.returncode}')
                    result['peak_rss_kib'] = int(rss.read_text().strip())
                    validate_stdout(case, implementation, process.stdout, args.subjects, args.assertions)
                    if case == 'owl':
                        query = (f'require len(GuidOf(?id, ?guid)) == {args.subjects + 1}\n'
                                 f'require len(Label(?id, ?label)) == {args.subjects + 1}\n'
                                 f'require len(Genls(?child, ?parent)) == {args.subjects}\n'
                                 'let root = GuidOf(?id, "Mx4rParityRoot0000000000")[0][:id]\n'
                                 'require Label(root, "Root é🦀")\nrequire Genls(_, root)\nreturn 8675309')
                        verification = ([str(rust), '--store', str(store), 'eval', query] if implementation == 'rust' else
                                        [str(output / 'filein'), '--store', str(store), '--eval', query])
                        checked = execute(prefix + verification, source, args.timeout)
                        (output / f'{name}-verify.stdout').write_text(checked.stdout)
                        (output / f'{name}-verify.stderr').write_text(checked.stderr)
                        result['verify_command'] = verification
                        pattern = r'^task \d+ complete: 8675309 \(retries: \d+\)$' if implementation == 'rust' else r'^8675309$'
                        if checked.returncode or not re.search(pattern, checked.stdout, re.M):
                            raise ValueError('persisted facts failed verification')
                    result['passed'] = True
                except (ValueError, subprocess.TimeoutExpired) as error:
                    result['error'] = str(error)
                manifest['results'].append(result)
                save(output / 'manifest.json', manifest)
                print(name, 'passed' if result['passed'] else result['error'], flush=True)
    summary = {}
    for case in ['owl', 'cycl']:
        for implementation in ['rust', 'odin']:
            values = [r for r in manifest['results'] if r['case'] == case and r['implementation'] == implementation]
            key = f'{case}-{implementation}'
            summary[key] = {'passed': all(r['passed'] for r in values)}
            if summary[key]['passed']:
                summary[key].update(median_ms=statistics.median(r['elapsed_ns'] / 1e6 for r in values),
                                    elapsed_ms=[r['elapsed_ns'] / 1e6 for r in values],
                                    peak_rss_kib=[r['peak_rss_kib'] for r in values])
    save(output / 'summary.json', summary)
    raise SystemExit(0 if all(r['passed'] for r in manifest['results']) else 1)


if __name__ == '__main__':
    main()
