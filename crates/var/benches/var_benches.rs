// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com> This program is free
// software: you can redistribute it and/or modify it under the terms of the GNU
// Affero General Public License as published by the Free Software Foundation,
// version 3.
//
// This program is distributed in the hope that it will be useful, but WITHOUT
// ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS
// FOR A PARTICULAR PURPOSE. See the GNU Affero General Public License for more
// details.
//
// You should have received a copy of the GNU Affero General Public License along
// with this program. If not, see <https://www.gnu.org/licenses/>.

use mica_var::{Identity, Symbol, Value};
use micromeasure::{
    BenchContext, BenchmarkMainOptions, NoContext, Throughput, benchmark_main, black_box,
};
use std::time::Duration;

struct IntContext(Value);

impl BenchContext for IntContext {
    fn prepare(_num_chunks: usize) -> Self {
        Self(Value::int(0).unwrap())
    }
}

struct StringContext(Value);

impl BenchContext for StringContext {
    fn prepare(_num_chunks: usize) -> Self {
        Self(Value::string("brass lamp"))
    }
}

struct ListContext(Value);

impl BenchContext for ListContext {
    fn prepare(_num_chunks: usize) -> Self {
        Self(Value::list((0..64).map(|i| Value::int(i).unwrap())))
    }
}

struct SymbolNamesContext {
    names: Box<[String]>,
}

impl BenchContext for SymbolNamesContext {
    fn prepare(_num_chunks: usize) -> Self {
        Self {
            names: (0..4096)
                .map(|index| format!("selector_{index}"))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        }
    }
}

fn construct_ints(_ctx: &mut NoContext, chunk_size: usize, _chunk_num: usize) {
    for i in 0..chunk_size {
        black_box(Value::int(i as i64).unwrap());
    }
}

fn construct_identities(_ctx: &mut NoContext, chunk_size: usize, _chunk_num: usize) {
    for i in 0..chunk_size {
        black_box(Value::identity(Identity::new(i as u64).unwrap()));
    }
}

fn construct_symbols(_ctx: &mut NoContext, chunk_size: usize, _chunk_num: usize) {
    let symbol = Symbol::intern("take");
    for _ in 0..chunk_size {
        black_box(Value::symbol(symbol));
    }
}

fn intern_symbol_hot(_ctx: &mut NoContext, chunk_size: usize, _chunk_num: usize) {
    for _ in 0..chunk_size {
        black_box(Symbol::intern("take"));
    }
}

fn intern_symbol_varied(ctx: &mut SymbolNamesContext, chunk_size: usize, chunk_num: usize) {
    let len = ctx.names.len();
    for i in 0..chunk_size {
        let index = chunk_num.wrapping_mul(chunk_size).wrapping_add(i) % len;
        black_box(Symbol::intern(&ctx.names[index]));
    }
}

fn int_add(ctx: &mut IntContext, chunk_size: usize, _chunk_num: usize) {
    let mut value = ctx.0.clone();
    let one = Value::int(1).unwrap();
    for _ in 0..chunk_size {
        value = value.checked_add(&one).unwrap();
        black_box(&value);
    }
    ctx.0 = value;
}

fn int_cmp(ctx: &mut IntContext, chunk_size: usize, _chunk_num: usize) {
    let value = ctx.0.clone();
    for _ in 0..chunk_size {
        black_box(value.cmp(&value));
    }
}

fn string_arc_clone(ctx: &mut StringContext, chunk_size: usize, _chunk_num: usize) {
    for _ in 0..chunk_size {
        black_box(ctx.0.clone());
    }
}

fn list_arc_clone(ctx: &mut ListContext, chunk_size: usize, _chunk_num: usize) {
    for _ in 0..chunk_size {
        black_box(ctx.0.clone());
    }
}

fn ordered_key_identity(_ctx: &mut NoContext, chunk_size: usize, _chunk_num: usize) {
    for i in 0..chunk_size {
        let value = Value::identity(Identity::new(i as u64).unwrap());
        black_box(value.ordered_key_bytes());
    }
}

// Each operation builds an independent persistent map; sizes expose update scaling.
fn map_insert<const N: i64>(_ctx: &mut NoContext, chunk_size: usize, _chunk_num: usize) {
    for _ in 0..chunk_size {
        let mut map = Value::map([]);
        for n in 0..N {
            let key = Value::int(n).unwrap();
            map = map.map_set(key.clone(), Value::list([key])).unwrap();
        }
        black_box(map);
    }
}

fn list_append<const N: usize, const NESTED: bool>(
    _ctx: &mut NoContext,
    chunk_size: usize,
    _chunk_num: usize,
) {
    for _ in 0..chunk_size {
        let mut list = Value::list([]);
        for n in 0..N {
            let value = Value::int(n as i64).unwrap();
            let value = if NESTED { Value::list([value]) } else { value };
            list = list.list_append(value).unwrap();
        }
        black_box(list);
    }
}

benchmark_main!(
    BenchmarkMainOptions {
        filter_help: Some(
            "all, construct, symbol, int, arc, list, map, key, or any benchmark name substring"
                .to_string()
        ),
        runtime: micromeasure::BenchmarkRuntimeOptions {
            benchmark_duration: Duration::from_secs(1),
            ..Default::default()
        },
        ..Default::default()
    },
    |runner| {
        runner.group::<NoContext>("construct", |g| {
            g.throughput(Throughput::per_operation(1, "value"))
                .bench("construct_ints", construct_ints);
            g.throughput(Throughput::per_operation(1, "value"))
                .bench("construct_identities", construct_identities);
            g.throughput(Throughput::per_operation(1, "value"))
                .bench("construct_symbols", construct_symbols);
        });

        runner.group::<NoContext>("symbol", |g| {
            g.throughput(Throughput::per_operation(1, "intern"))
                .bench("intern_symbol_hot", intern_symbol_hot);
        });
        runner.group::<SymbolNamesContext>("symbol", |g| {
            g.throughput(Throughput::per_operation(1, "intern"))
                .bench("intern_symbol_varied", intern_symbol_varied);
        });

        runner.group::<IntContext>("int", |g| {
            g.throughput(Throughput::per_operation(1, "op"))
                .bench("int_add", int_add);
            g.throughput(Throughput::per_operation(1, "cmp"))
                .bench("int_cmp", int_cmp);
        });

        runner.group::<StringContext>("arc", |g| {
            g.throughput(Throughput::per_operation(1, "clone"))
                .bench("string_arc_clone", string_arc_clone);
        });
        runner.group::<ListContext>("arc", |g| {
            g.throughput(Throughput::per_operation(1, "clone"))
                .bench("list_arc_clone", list_arc_clone);
        });

        runner.group::<NoContext>("list", |g| {
            g.throughput(Throughput::per_operation(2000, "element"))
                .bench("list_append_nested_2000", list_append::<2000, true>);
            g.throughput(Throughput::per_operation(8000, "element"))
                .bench("list_append_nested_8000", list_append::<8000, true>);
            g.throughput(Throughput::per_operation(16000, "element"))
                .bench("list_append_nested_16000", list_append::<16000, true>);
            g.throughput(Throughput::per_operation(16000, "element"))
                .bench("list_append_scalar_16000", list_append::<16000, false>);
        });

        runner.group::<NoContext>("map", |g| {
            g.throughput(Throughput::per_operation(2000, "entry"))
                .bench("map_insert_2000", map_insert::<2000>);
            g.throughput(Throughput::per_operation(8000, "entry"))
                .bench("map_insert_8000", map_insert::<8000>);
            g.throughput(Throughput::per_operation(16000, "entry"))
                .bench("map_insert_16000", map_insert::<16000>);
        });

        runner.group::<NoContext>("key", |g| {
            g.throughput(Throughput::per_operation(1, "key"))
                .bench("ordered_key_identity", ordered_key_identity);
        });
    }
);
