// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

use mica_relation_kernel::buffer::{BufferConflictPolicy, BufferMetadata};
use mica_relation_kernel::{
    FjallDurabilityMode, FjallStateProvider, RelationDurability, RelationKernel,
};
use mica_var::{Identity, Symbol};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

fn main() -> Result<(), String> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if arguments.len() != 4 {
        return Err(
            "usage: buffer_persistence_probe STORE EDITS SCALARS strict|relaxed".to_owned(),
        );
    }
    let path = PathBuf::from(&arguments[0]);
    if path.exists() {
        return Err("probe store must not exist".to_owned());
    }
    let edits = arguments[1]
        .parse::<usize>()
        .map_err(|error| error.to_string())?;
    let scalars = arguments[2]
        .parse::<usize>()
        .map_err(|error| error.to_string())?;
    if edits == 0 || scalars < 2 {
        return Err("edits must be positive and scalars must be at least two".to_owned());
    }
    let durability = match arguments[3].as_str() {
        "strict" => FjallDurabilityMode::Strict,
        "relaxed" => FjallDurabilityMode::Relaxed,
        _ => return Err("durability must be strict or relaxed".to_owned()),
    };
    let id = Identity::new(1).unwrap();
    let provider = Arc::new(FjallStateProvider::open_with_durability(&path, durability)?);
    let kernel = RelationKernel::with_provider(provider);
    let mut tx = kernel.begin();
    tx.create_buffer(BufferMetadata {
        id,
        name: Symbol::intern("probe"),
        durability: RelationDurability::Durable,
        conflict: BufferConflictPolicy::Span,
    })
    .map_err(|error| format!("{error:?}"))?;
    tx.replace_buffer(id, 0..0, &"λ".repeat(scalars))
        .map_err(|error| format!("{error:?}"))?;
    tx.commit().map_err(|error| format!("{error:?}"))?;
    kernel
        .flush_persistence()
        .map_err(|error| format!("{error:?}"))?;
    let mut latencies = Vec::with_capacity(edits);
    let started = Instant::now();
    for index in 0..edits {
        let operation = Instant::now();
        let mut tx = kernel.begin();
        tx.replace_buffer(id, 1..2, if index % 2 == 0 { "🦀" } else { "x" })
            .map_err(|error| format!("{error:?}"))?;
        tx.commit().map_err(|error| format!("{error:?}"))?;
        latencies.push(operation.elapsed().as_secs_f64() * 1e6);
    }
    kernel
        .flush_persistence()
        .map_err(|error| format!("{error:?}"))?;
    let edit_ms = started.elapsed().as_secs_f64() * 1e3;
    drop(kernel);
    let started = Instant::now();
    let provider = Arc::new(FjallStateProvider::open_with_durability(&path, durability)?);
    let kernel = RelationKernel::load_from_state(provider.load_state()?, provider)
        .map_err(|error| format!("{error:?}"))?;
    let recovery_ms = started.elapsed().as_secs_f64() * 1e3;
    let snapshot = kernel.snapshot();
    let buffer = snapshot.buffer(id).map_err(|error| format!("{error:?}"))?;
    assert_eq!(buffer.revision(), edits as u64 + 1);
    assert_eq!(buffer.text().len(), scalars);
    assert_eq!(
        buffer.text().slice(1..2).unwrap(),
        if edits % 2 == 1 { "🦀" } else { "x" }
    );
    latencies.sort_by(f64::total_cmp);
    println!(
        "{{\"edits\":{edits},\"scalars\":{scalars},\"durability\":\"{}\",\"edit_and_flush_ms\":{edit_ms},\"edit_p50_us\":{},\"edit_p95_us\":{},\"edit_p99_us\":{},\"edit_max_us\":{},\"recovery_ms\":{recovery_ms}}}",
        arguments[3],
        latencies[edits / 2],
        latencies[edits * 95 / 100],
        latencies[edits * 99 / 100],
        latencies[edits - 1],
    );
    Ok(())
}
